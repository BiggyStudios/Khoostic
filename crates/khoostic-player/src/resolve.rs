//! Turning [`Track`]s into decodable audio.
//!
//! Each backend (local files today; Spotify, Subsonic and MPD later) provides a
//! [`SourceResolver`]. The player asks them in order until one accepts the track.

use std::sync::Arc;

use khoostic_audio::{AudioSource, SourceError, SymphoniaSource};
use khoostic_core::{Track, TrackLocator};

pub trait SourceResolver: Send + Sync {
    fn accepts(&self, locator: &TrackLocator) -> bool;
    fn open(&self, track: &Track) -> Result<Box<dyn AudioSource>, SourceError>;
}

pub struct LocalFiles;

impl SourceResolver for LocalFiles {
    fn accepts(&self, locator: &TrackLocator) -> bool {
        matches!(locator, TrackLocator::Local(_))
    }

    fn open(&self, track: &Track) -> Result<Box<dyn AudioSource>, SourceError> {
        let TrackLocator::Local(path) = &track.locator else {
            return Err(SourceError::Unsupported("not a local track".into()));
        };
        Ok(Box::new(SymphoniaSource::open_path(path)?))
    }
}

#[derive(Clone, Default)]
pub struct Resolvers(Vec<Arc<dyn SourceResolver>>);

impl Resolvers {
    pub fn new() -> Self {
        Self::default()
    }

    /// The resolvers that need no configuration.
    pub fn with_defaults() -> Self {
        Self::new().with(LocalFiles)
    }

    pub fn with(mut self, r: impl SourceResolver + 'static) -> Self {
        self.0.push(Arc::new(r));
        self
    }

    pub fn open(&self, track: &Track) -> Result<Box<dyn AudioSource>, SourceError> {
        self.0
            .iter()
            .find(|r| r.accepts(&track.locator))
            .ok_or_else(|| {
                SourceError::Unsupported(format!("no backend for {} tracks", track.locator.kind()))
            })?
            .open(track)
    }
}
