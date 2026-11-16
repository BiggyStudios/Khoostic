//! The [`AudioSource`] abstraction: anything that can produce PCM.
//!
//! Local files and HTTP streams (Subsonic/Navidrome) are decoded by symphonia; Spotify audio
//! will come from librespot through the same trait. The engine never knows the difference.

use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceSpec {
    pub sample_rate: u32,
    pub channels: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadStatus {
    /// Samples were appended to the output buffer.
    Data,
    /// The source is exhausted. No samples were appended.
    End,
}

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported media: {0}")]
    Unsupported(String),
    #[error("decode error: {0}")]
    Decode(String),
    #[error("seek failed: {0}")]
    Seek(String),
    /// Briefly out of reach (network trouble, a throttled service); worth retrying soon.
    #[error("temporarily unavailable: {0}")]
    Unavailable(String),
}

impl SourceError {
    pub fn is_temporary(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}

pub trait AudioSource: Send {
    /// Current stream format. May change between reads (chained streams); the engine checks it
    /// after every read.
    fn spec(&self) -> SourceSpec;

    fn duration(&self) -> Option<Duration>;

    /// Decodes the next chunk and appends interleaved `f32` samples (in `[-1, 1]`) to `out`.
    fn read(&mut self, out: &mut Vec<f32>) -> Result<ReadStatus, SourceError>;

    /// Seeks to `to`, returning the position actually reached.
    fn seek(&mut self, to: Duration) -> Result<Duration, SourceError>;
}
