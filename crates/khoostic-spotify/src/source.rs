//! Spotify audio as a Khoostic [`AudioSource`]: librespot fetches and decrypts the Ogg Vorbis
//! stream, our symphonia decoder plays it through the same engine as local files (so gapless,
//! seeking, resampling and visualizers all just work).

use std::io::{self, Read, Seek, SeekFrom};
use std::time::Duration;

use anyhow::{Context, bail};
use khoostic_audio::{AudioSource, ReadStatus, SourceError, SourceSpec, SymphoniaSource};
use librespot_audio::{AudioDecrypt, AudioFile};
use librespot_core::{Session, SpotifyId, SpotifyUri};
use librespot_metadata::audio::{AudioFileFormat, AudioItem};
use parking_lot::Mutex;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSource;

/// Spotify prefixes Ogg files with a 0xa7-byte header of its own.
const SPOTIFY_OGG_HEADER_END: u64 = 0xa7;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Bitrate {
    B96,
    B160,
    #[default]
    B320,
}

impl Bitrate {
    pub fn from_kbps(kbps: u32) -> Self {
        match kbps {
            0..=128 => Self::B96,
            129..=256 => Self::B160,
            _ => Self::B320,
        }
    }

    /// Preferred formats, best first. 320 kbps needs Premium; we fall back gracefully.
    fn formats(self) -> &'static [AudioFileFormat] {
        use AudioFileFormat::*;
        match self {
            Self::B320 => &[OGG_VORBIS_320, OGG_VORBIS_160, OGG_VORBIS_96],
            Self::B160 => &[OGG_VORBIS_160, OGG_VORBIS_96, OGG_VORBIS_320],
            Self::B96 => &[OGG_VORBIS_96, OGG_VORBIS_160, OGG_VORBIS_320],
        }
    }
}

fn bytes_per_second(format: AudioFileFormat) -> usize {
    use AudioFileFormat::*;
    let kib = match format {
        OGG_VORBIS_96 => 12.0,
        OGG_VORBIS_160 => 20.0,
        _ => 40.0,
    };
    (kib * 1024.0) as usize
}

/// The decrypted stream minus Spotify's header, as a seekable byte source for symphonia.
/// (The mutex only exists to satisfy `MediaSource: Sync`; reads come from one thread.)
struct SpotifyStream {
    inner: Mutex<AudioDecrypt<AudioFile>>,
    len: u64,
    pos: u64,
}

impl Read for SpotifyStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.get_mut().read(buf)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for SpotifyStream {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(p) => p as i64,
            SeekFrom::Current(d) => self.pos as i64 + d,
            SeekFrom::End(d) => self.len as i64 + d,
        };
        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before start",
            ));
        }
        self.inner
            .get_mut()
            .seek(SeekFrom::Start(target as u64 + SPOTIFY_OGG_HEADER_END))?;
        self.pos = target as u64;
        Ok(self.pos)
    }
}

impl MediaSource for SpotifyStream {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.len)
    }
}

pub struct SpotifySource {
    decoder: SymphoniaSource,
    duration: Duration,
    /// Keeps the session (and its download tasks) alive while we play.
    _session: Session,
}

impl AudioSource for SpotifySource {
    fn spec(&self) -> SourceSpec {
        self.decoder.spec()
    }

    fn duration(&self) -> Option<Duration> {
        // Spotify's metadata is exact; Ogg duration would need a read of the file's end.
        Some(self.duration)
    }

    fn read(&mut self, out: &mut Vec<f32>) -> Result<ReadStatus, SourceError> {
        self.decoder.read(out)
    }

    fn seek(&mut self, to: Duration) -> Result<Duration, SourceError> {
        self.decoder.seek(to.min(self.duration))
    }
}

/// Network part of opening a track (must run on the tokio runtime).
pub(crate) async fn fetch(
    session: &Session,
    id: &str,
    bitrate: Bitrate,
) -> anyhow::Result<(AudioDecrypt<AudioFile>, u64, Duration)> {
    let spotify_id =
        SpotifyId::from_base62(id).map_err(|e| anyhow::anyhow!("bad Spotify id {id}: {e}"))?;
    let uri = SpotifyUri::Track { id: spotify_id };
    let mut item = AudioItem::get_file(session, uri)
        .await
        .context("loading track metadata")?;
    if let Err(e) = &item.availability {
        bail!("not available: {e}");
    }
    // Region-locked tracks often have a playable alternative.
    if item.files.is_empty() {
        let alts = item.alternatives.take().map(|t| t.0).unwrap_or_default();
        let mut found = None;
        for alt in alts {
            if let Ok(a) = AudioItem::get_file(session, alt).await
                && a.availability.is_ok()
                && !a.files.is_empty()
            {
                found = Some(a);
                break;
            }
        }
        item = found.context("no playable version of this track in your region")?;
    }
    let (format, file_id) = bitrate
        .formats()
        .iter()
        .find_map(|f| item.files.get(f).map(|id| (*f, *id)))
        .context("no Ogg Vorbis stream for this track")?;
    let track_id: SpotifyId = (&item.track_id)
        .try_into()
        .map_err(|_| anyhow::anyhow!("bad track id"))?;
    let file = AudioFile::open(session, file_id, bytes_per_second(format))
        .await
        .context("opening audio stream")?;
    let len = file
        .get_stream_loader_controller()
        .context("stream controller")?
        .len() as u64;
    let key = session
        .audio_key()
        .request(track_id, file_id)
        .await
        .context("getting decryption key")?;
    tracing::info!(track = %item.name, ?format, "streaming from Spotify");
    Ok((
        AudioDecrypt::new(Some(key), file),
        len,
        Duration::from_millis(item.duration_ms as u64),
    ))
}

/// Blocking part: builds the decoder. Reading may wait on downloads, so this must *not* run on
/// a tokio worker.
pub(crate) fn open_decoder(
    session: Session,
    file: AudioDecrypt<AudioFile>,
    len: u64,
    duration: Duration,
) -> anyhow::Result<SpotifySource> {
    let stream = SpotifyStream {
        inner: Mutex::new(file),
        len: len.saturating_sub(SPOTIFY_OGG_HEADER_END),
        pos: 0,
    };
    let mut stream = stream;
    stream.seek(SeekFrom::Start(0))?;
    let mut hint = Hint::new();
    hint.mime_type("audio/ogg");
    let decoder = SymphoniaSource::open(Box::new(stream), hint)?;
    Ok(SpotifySource {
        decoder,
        duration,
        _session: session,
    })
}
