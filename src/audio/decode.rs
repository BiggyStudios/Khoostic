//! Symphonia-backed [`AudioSource`] for files and any seekable byte stream.

use std::fs::File;
use std::path::Path;
use std::time::Duration;

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase, Timestamp};

use crate::audio::source::{AudioSource, ReadStatus, SourceError, SourceSpec};

/// File extensions we try to play. Used when scanning folders.
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "opus", "wav", "wave", "aif", "aiff", "m4a", "mp4", "aac", "alac",
    "caf", "mka", "mkv", "webm",
];

pub fn is_supported_path(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        SUPPORTED_EXTENSIONS
            .iter()
            .any(|s| s.eq_ignore_ascii_case(e))
    })
}

pub struct SymphoniaSource {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    duration: Option<Duration>,
    spec: SourceSpec,
    /// After an accurate seek: decoded frames before this timestamp are discarded.
    skip_until: Option<Timestamp>,
}

impl SymphoniaSource {
    pub fn open_path(path: impl AsRef<Path>) -> Result<Self, SourceError> {
        let path = path.as_ref();
        let file = File::open(path)?;
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        Self::open(Box::new(file), hint)
    }

    /// Opens any seekable byte source (a file, a buffered HTTP stream, ...).
    pub fn open(source: Box<dyn MediaSource>, hint: Hint) -> Result<Self, SourceError> {
        let mss = MediaSourceStream::new(source, Default::default());
        let format = symphonia::default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|e| SourceError::Unsupported(e.to_string()))?;

        let track = format
            .default_track(TrackType::Audio)
            .ok_or_else(|| SourceError::Unsupported("no audio track".into()))?;
        let track_id = track.id;
        let time_base = track.time_base;
        let duration = match (time_base, track.duration) {
            (Some(tb), Some(d)) => tb.calc_duration(d).map(time_to_std),
            _ => None,
        };
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or_else(|| SourceError::Unsupported("missing audio codec parameters".into()))?;
        let spec = SourceSpec {
            sample_rate: params.sample_rate.unwrap_or(44_100),
            channels: params.channels.as_ref().map_or(2, |c| c.count()),
        };
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(|e| SourceError::Unsupported(e.to_string()))?;

        Ok(Self {
            format,
            decoder,
            track_id,
            time_base,
            duration,
            spec,
            skip_until: None,
        })
    }

    fn rebuild_decoder(&mut self) -> Result<(), SourceError> {
        let track = self
            .format
            .tracks()
            .iter()
            .find(|t| t.id == self.track_id)
            .or_else(|| self.format.default_track(TrackType::Audio))
            .ok_or_else(|| SourceError::Unsupported("audio track vanished".into()))?;
        self.track_id = track.id;
        self.time_base = track.time_base;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or_else(|| SourceError::Unsupported("missing audio codec parameters".into()))?;
        self.decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(|e| SourceError::Unsupported(e.to_string()))?;
        Ok(())
    }

    /// How many leading frames of a packet starting at `pts` fall before `skip_until`.
    fn frames_to_skip(&self, pts: Timestamp) -> usize {
        let (Some(until), Some(tb)) = (self.skip_until, self.time_base) else {
            return 0;
        };
        let ticks = until.get().saturating_sub(pts.get());
        if ticks <= 0 {
            return 0;
        }
        let secs = ticks as f64 * tb.numer.get() as f64 / tb.denom.get() as f64;
        (secs * self.spec.sample_rate as f64).round() as usize
    }
}

impl AudioSource for SymphoniaSource {
    fn spec(&self) -> SourceSpec {
        self.spec
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn read(&mut self, out: &mut Vec<f32>) -> Result<ReadStatus, SourceError> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(ReadStatus::End),
                Err(SymError::ResetRequired) => {
                    self.rebuild_decoder()?;
                    continue;
                }
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(ReadStatus::End);
                }
                Err(e) => return Err(SourceError::Decode(e.to_string())),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let skip = self.frames_to_skip(packet.pts);
            let buf = match self.decoder.decode(&packet) {
                Ok(buf) => buf,
                // Corrupt frames happen in the wild; skip them rather than killing the track.
                Err(SymError::DecodeError(e)) => {
                    tracing::warn!("skipping undecodable packet: {e}");
                    continue;
                }
                Err(SymError::IoError(_)) => continue,
                Err(e) => return Err(SourceError::Decode(e.to_string())),
            };
            let spec = buf.spec();
            self.spec = SourceSpec {
                sample_rate: spec.rate(),
                channels: spec.channels().count(),
            };
            let frames = buf.frames();
            if frames == 0 {
                continue;
            }
            if skip >= frames {
                continue;
            }
            self.skip_until = None;
            let channels = self.spec.channels;
            let start = out.len();
            out.resize(start + buf.samples_interleaved(), 0.0);
            buf.copy_to_slice_interleaved(&mut out[start..]);
            if skip > 0 {
                out.drain(start..start + skip * channels);
            }
            return Ok(ReadStatus::Data);
        }
    }

    fn seek(&mut self, to: Duration) -> Result<Duration, SourceError> {
        let time = Time::try_from_secs_f64(to.as_secs_f64())
            .ok_or_else(|| SourceError::Seek("position out of range".into()))?;
        let seeked = self
            .format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time,
                    track_id: Some(self.track_id),
                },
            )
            .map_err(|e| SourceError::Seek(e.to_string()))?;
        self.decoder.reset();
        self.skip_until = Some(seeked.required_ts);
        // We trim to `required_ts`, so the effective position is what was asked for.
        let reached = self
            .time_base
            .and_then(|tb| tb.calc_time(seeked.required_ts))
            .map(time_to_std)
            .unwrap_or(to);
        Ok(reached)
    }
}

fn time_to_std(t: Time) -> Duration {
    Duration::from_secs_f64(t.as_secs_f64().max(0.0))
}
