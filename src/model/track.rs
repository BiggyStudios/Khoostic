//! Source-agnostic track model.
//!
//! A [`Track`] describes *what* to play; a [`TrackLocator`] describes *where* it lives.
//! Resolving a locator into decodable audio is the job of the player's source resolvers,
//! so the model stays free of any I/O or audio dependencies.

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

impl Default for TrackLocator {
    fn default() -> Self {
        Self::Local(PathBuf::new())
    }
}

impl TrackLocator {
    /// Stable string form for storage (playlists, jams): `local:<path>`, `spotify:track:<id>`,
    /// `subsonic:<server>|<id>`, `mpd:<server>|<uri>`.
    pub fn to_uri(&self) -> String {
        match self {
            Self::Local(p) => format!("local:{}", p.display()),
            Self::Spotify { id } => format!("spotify:track:{id}"),
            Self::Subsonic { server, id } => format!("subsonic:{server}|{id}"),
            Self::Mpd { server, uri } => format!("mpd:{server}|{uri}"),
        }
    }

    pub fn from_uri(s: &str) -> Option<Self> {
        if let Some(p) = s.strip_prefix("local:") {
            return Some(Self::Local(PathBuf::from(p)));
        }
        if let Some(id) = s.strip_prefix("spotify:track:") {
            return Some(Self::Spotify { id: id.to_owned() });
        }
        let split = |rest: &str| {
            rest.split_once('|')
                .map(|(a, b)| (a.to_owned(), b.to_owned()))
        };
        if let Some((server, id)) = s.strip_prefix("subsonic:").and_then(split) {
            return Some(Self::Subsonic { server, id });
        }
        if let Some((server, uri)) = s.strip_prefix("mpd:").and_then(split) {
            return Some(Self::Mpd { server, uri });
        }
        None
    }

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

/// Where cover art for a track can be found.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CoverRef {
    /// A thumbnail in the local cover cache, by content hash.
    Cached(String),
    /// A remote image (Spotify, Subsonic).
    Url(String),
}

/// Descriptive metadata for a playable track.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Track {
    pub locator: TrackLocator,
    pub title: String,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub year: Option<u16>,
    pub genre: Option<String>,
    pub duration: Option<Duration>,
    pub cover: Option<CoverRef>,
}

impl Track {
    /// A minimal track for a local file, titled after the file name. The library fills in
    /// real tags.
    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let title = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Self {
            locator: TrackLocator::Local(path),
            title,
            ..Default::default()
        }
    }

    pub fn artist_line(&self) -> String {
        self.artists.join(", ")
    }

    /// The artist an album is filed under.
    pub fn primary_artist(&self) -> Option<&str> {
        self.album_artist
            .as_deref()
            .or(self.artists.first().map(String::as_str))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_round_trip() {
        for loc in [
            TrackLocator::Local(PathBuf::from(r"C:\Music\a b|c.flac")),
            TrackLocator::Spotify {
                id: "4uLU6hMCjMI75M1A2tKUQC".into(),
            },
            TrackLocator::Subsonic {
                server: "https://nd.example".into(),
                id: "tr-1".into(),
            },
            TrackLocator::Mpd {
                server: "localhost:6600".into(),
                uri: "a/b|c.mp3".into(),
            },
        ] {
            assert_eq!(TrackLocator::from_uri(&loc.to_uri()), Some(loc));
        }
        assert_eq!(TrackLocator::from_uri("ftp:x"), None);
    }
}
