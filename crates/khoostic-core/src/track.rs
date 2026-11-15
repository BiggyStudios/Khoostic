//! Source-agnostic track model.
//!
//! A [`Track`] describes *what* to play; a [`TrackLocator`] describes *where* it lives.
//! Resolving a locator into decodable audio is the job of the player's source resolvers,
//! so this crate stays free of any I/O or audio dependencies.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

/// Where a track's audio can be obtained from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TrackLocator {
    /// A file on the local filesystem.
    Local(PathBuf),
    /// A Spotify track, by base62 track id (`spotify:track:<id>`).
    Spotify { id: String },
    /// A track on a Subsonic-compatible server (Navidrome, Airsonic, ...).
    Subsonic { server: String, id: String },
    /// A song known to an MPD server, by its MPD URI.
    Mpd { server: String, uri: String },
}

impl TrackLocator {
    pub fn kind(&self) -> SourceKind {
        match self {
            Self::Local(_) => SourceKind::Local,
            Self::Spotify { .. } => SourceKind::Spotify,
            Self::Subsonic { .. } => SourceKind::Subsonic,
            Self::Mpd { .. } => SourceKind::Mpd,
        }
    }
}

/// Coarse classification of a track's origin. Useful for UI badges and for rules such as
/// "local tracks can't be added to Spotify playlists".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceKind {
    Local,
    Spotify,
    Subsonic,
    Mpd,
}

impl SourceKind {
    /// Whether a track of this kind may be stored in a remote Spotify playlist.
    pub fn spotify_playlist_compatible(self) -> bool {
        matches!(self, Self::Spotify)
    }
}

impl fmt::Display for SourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Local => "local",
            Self::Spotify => "spotify",
            Self::Subsonic => "subsonic",
            Self::Mpd => "mpd",
        })
    }
}

/// Descriptive metadata for a playable track.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub locator: TrackLocator,
    pub title: String,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub duration: Option<Duration>,
}

impl Track {
    /// Builds a minimal track for a local file, using the file stem as the title.
    /// Proper tag reading arrives with the library database.
    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let title = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Self {
            locator: TrackLocator::Local(path),
            title,
            artists: Vec::new(),
            album: None,
            duration: None,
        }
    }

    pub fn artist_line(&self) -> String {
        self.artists.join(", ")
    }
}
