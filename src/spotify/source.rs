//! Spotify audio as a Khoostic [`AudioSource`]: librespot fetches and decrypts the Ogg Vorbis
//! stream, our symphonia decoder plays it through the same engine as local files (so gapless,
//! seeking, resampling and visualizers all just work).

use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom};
use std::time::{Duration, Instant};

use anyhow::Context;
use librespot_audio::{AudioDecrypt, AudioFile};
use librespot_core::audio_key::AudioKey;
use librespot_core::{FileId, Session, SpotifyId, SpotifyUri};
use librespot_metadata::audio::{AudioFileFormat, AudioItem};
use parking_lot::Mutex;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSource;

use crate::audio::{AudioSource, ReadStatus, SourceError, SourceSpec, SymphoniaSource};

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

/// Spotify hands out decryption keys sparingly: a few in a row, then it refuses every request
/// for about 20 seconds (per account; a new connection doesn't help, and asking during the pause
/// seems to stretch it). So keys are remembered, and after a refusal none is asked for until the
/// pause is likely over.
#[derive(Default)]
pub(crate) struct KeyGate {
    keys: Mutex<HashMap<(SpotifyId, FileId), AudioKey>>,
    /// No requests before this; and how many refusals in a row led here.
    pause: Mutex<Option<(Instant, u32)>>,
}

/// Spotify is refusing keys for now: try again after this long.
#[derive(Debug, thiserror::Error)]
#[error("Spotify is pausing song loading for a moment")]
pub(crate) struct Refused(pub Duration);

/// Waits after the 1st, 2nd, 3rd... refusal in a row.
const KEY_PAUSES: [u64; 4] = [8, 12, 20, 30];

impl KeyGate {
    async fn get(
        &self,
        session: &Session,
        track: SpotifyId,
        file: FileId,
    ) -> anyhow::Result<AudioKey> {
        if let Some(k) = self.keys.lock().get(&(track, file)) {
            return Ok(*k);
        }
        let now = Instant::now();
        if let Some((until, _)) = *self.pause.lock()
            && until > now
        {
            return Err(Refused(until - now).into());
        }
        match session.audio_key().request(track, file).await {
            Ok(k) => {
                self.keys.lock().insert((track, file), k);
                *self.pause.lock() = None;
                Ok(k)
            }
            Err(e) if e.kind == librespot_core::error::ErrorKind::Unavailable => {
                let mut pause = self.pause.lock();
                let strikes = pause.map_or(0, |(_, s)| s) + 1;
                let wait = Duration::from_secs(
                    KEY_PAUSES[(strikes as usize - 1).min(KEY_PAUSES.len() - 1)],
                );
                *pause = Some((Instant::now() + wait, strikes));
                tracing::info!(strikes, ?wait, "Spotify refused a decryption key");
                Err(Refused(wait).into())
            }
            Err(e) => Err(anyhow::Error::from(e).context("getting decryption key")),
        }
    }
}

/// This track can't be played (as opposed to Spotify being briefly unreachable).
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct NotPlayable(pub String);

/// Network part of opening a track (must run on the tokio runtime).
pub(crate) async fn fetch(
    session: &Session,
    keys: &KeyGate,
    id: &str,
    bitrate: Bitrate,
) -> anyhow::Result<(AudioDecrypt<AudioFile>, u64, Duration)> {
    let spotify_id =
        SpotifyId::from_base62(id).map_err(|e| NotPlayable(format!("bad Spotify id {id}: {e}")))?;
    let uri = SpotifyUri::Track { id: spotify_id };
    let mut item = AudioItem::get_file(session, uri)
        .await
        .context("loading track metadata")?;
    if let Err(e) = &item.availability {
        return Err(NotPlayable(format!("not available: {e}")).into());
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
        item = found.ok_or_else(|| {
            NotPlayable("no playable version of this track in your region".into())
        })?;
    }
    let (format, file_id) = bitrate
        .formats()
        .iter()
        .find_map(|f| item.files.get(f).map(|id| (*f, *id)))
        .ok_or_else(|| NotPlayable("no Ogg Vorbis stream for this track".into()))?;
    let track_id: SpotifyId = (&item.track_id)
        .try_into()
        .map_err(|_| anyhow::anyhow!("bad track id"))?;
    // The key first: no point downloading a song Spotify won't unlock right now.
    let key = keys.get(session, track_id, file_id).await?;
    let file = AudioFile::open(session, file_id, bytes_per_second(format))
        .await
        .context("opening audio stream")?;
    let len = file
        .get_stream_loader_controller()
        .context("stream controller")?
        .len() as u64;
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
