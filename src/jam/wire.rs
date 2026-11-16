//! What jam peers say to each other: one JSON message per line over TCP.

use std::io::{self, BufRead, Write};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::model::{CoverRef, EntryId, RepeatMode, Track, TrackLocator};

/// Bumped when old and new versions can't understand each other.
pub const VERSION: u32 = 1;
/// No message is anywhere near this long; a peer sending more is cut off.
const MAX_LINE: u64 = 4 << 20;

/// A song as another peer sees it: where it comes from plus enough to find another copy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireTrack {
    pub uri: String,
    pub title: String,
    pub artists: Vec<String>,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub cover: Option<String>,
    #[serde(default)]
    pub artist_ids: Vec<String>,
    #[serde(default)]
    pub album_id: Option<String>,
}

impl WireTrack {
    pub fn from_track(t: &Track) -> Self {
        Self {
            uri: t.locator.to_uri(),
            title: t.title.clone(),
            artists: t.artists.clone(),
            album: t.album.clone(),
            duration_ms: t.duration.map(|d| d.as_millis() as u64),
            cover: match &t.cover {
                Some(CoverRef::Url(u)) => Some(u.clone()),
                _ => None,
            },
            artist_ids: t.artist_ids.clone(),
            album_id: t.album_id.clone(),
        }
    }

    /// The song as described, with its original locator (which may not play here).
    pub fn to_track(&self) -> Track {
        Track {
            locator: TrackLocator::from_uri(&self.uri).unwrap_or_default(),
            title: self.title.clone(),
            artists: self.artists.clone(),
            album: self.album.clone(),
            duration: self.duration_ms.map(Duration::from_millis),
            cover: self.cover.clone().map(CoverRef::Url),
            artist_ids: self.artist_ids.clone(),
            album_id: self.album_id.clone(),
            ..Default::default()
        }
    }
}

/// One queue entry of the host's, under the host's entry id (guests reuse the ids, so
/// "play this one" and "remove that one" mean the same entry everywhere).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireEntry {
    pub id: u64,
    pub track: WireTrack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Repeat {
    Off,
    All,
    One,
}

impl From<RepeatMode> for Repeat {
    fn from(r: RepeatMode) -> Self {
        match r {
            RepeatMode::Off => Self::Off,
            RepeatMode::All => Self::All,
            RepeatMode::One => Self::One,
        }
    }
}

impl From<Repeat> for RepeatMode {
    fn from(r: Repeat) -> Self {
        match r {
            Repeat::Off => Self::Off,
            Repeat::All => Self::All,
            Repeat::One => Self::One,
        }
    }
}

/// What the host is playing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Playback {
    /// Bumped whenever the queue changes (entries are only sent then).
    pub queue_version: u64,
    /// Upcoming songs (and a few before), in play order; `None` when unchanged.
    pub entries: Option<Vec<WireEntry>>,
    pub current: Option<u64>,
    pub playing: bool,
    pub repeat: Repeat,
    pub shuffle: bool,
    /// Position in the current song at host clock `at_ms`.
    pub position_ms: u64,
    pub at_ms: u64,
    /// The host is still opening `current`.
    pub loading: bool,
}

/// Guest → host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToHost {
    Hello {
        version: u32,
        secret: String,
        name: String,
    },
    /// Clock sync: the guest's clock when sent.
    Ping {
        t: u64,
    },
    Control(Control),
    Bye,
}

/// Host → guest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToGuest {
    Welcome {
        jam: String,
        host: String,
    },
    Refused {
        reason: String,
    },
    /// Answers a ping with the host's clock.
    Pong {
        t: u64,
        host_ms: u64,
    },
    Playback(Playback),
    /// Everyone in the jam, host first.
    Members(Vec<String>),
    Ended,
}

/// A guest's change to the shared playback (player commands, with songs described and
/// positions as entry ids, since guests see a window of the host's queue).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Control {
    Enqueue(Vec<WireTrack>),
    EnqueueAndPlay(Vec<WireTrack>),
    PlayTracks {
        tracks: Vec<WireTrack>,
        start: usize,
    },
    PlayNext(Vec<WireTrack>),
    /// Insert before this entry (at the end if `None`).
    Insert {
        before: Option<u64>,
        tracks: Vec<WireTrack>,
    },
    /// Move an entry to where another one is.
    Move {
        id: u64,
        to: u64,
    },
    PlayEntry(u64),
    Remove(Vec<u64>),
    Clear,
    Next,
    Prev,
    Play,
    Pause,
    TogglePause,
    Stop,
    SeekMs(u64),
    SeekBy(i64),
    ToggleShuffle,
    SetShuffle(bool),
    SetRepeat(Repeat),
    CycleRepeat,
}

pub fn entry_ids(ids: &[EntryId]) -> Vec<u64> {
    ids.iter().map(|e| e.0).collect()
}

pub fn send<T: Serialize>(w: &mut impl Write, msg: &T) -> io::Result<()> {
    let mut line = serde_json::to_vec(msg)?;
    line.push(b'\n');
    w.write_all(&line)?;
    w.flush()
}

/// The next message; `Ok(None)` when the peer hung up.
pub fn recv<T: for<'de> Deserialize<'de>>(r: &mut impl BufRead) -> io::Result<Option<T>> {
    let mut line = String::new();
    let n = io::Read::take(&mut *r, MAX_LINE).read_line(&mut line)?;
    if n == 0 {
        return Ok(None);
    }
    if !line.ends_with('\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too long",
        ));
    }
    serde_json::from_str(&line)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// An invite: where the host listens and the jam's secret, written `address:port/secret`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    pub addr: String,
    pub secret: String,
}

impl Invite {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let s = s.strip_prefix("khoostic://jam/").unwrap_or(s);
        let (addr, secret) = s.rsplit_once('/')?;
        let (host, port) = addr.rsplit_once(':')?;
        if host.is_empty() || port.parse::<u16>().is_err() || secret.len() < 6 {
            return None;
        }
        Some(Self {
            addr: addr.to_owned(),
            secret: secret.to_owned(),
        })
    }
}

impl std::fmt::Display for Invite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.secret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invites_round_trip_and_reject_junk() {
        let i = Invite::parse(" 192.168.1.20:47470/k7f3qxpa ").unwrap();
        assert_eq!(i.addr, "192.168.1.20:47470");
        assert_eq!(Invite::parse(&i.to_string()), Some(i.clone()));
        assert_eq!(
            Invite::parse("khoostic://jam/192.168.1.20:47470/k7f3qxpa"),
            Some(i)
        );
        assert!(Invite::parse("[fd7a::1]:47470/k7f3qxpa").is_some());
        for bad in [
            "",
            "hello",
            "1.2.3.4/abcdefgh",
            "1.2.3.4:99999/abcdefgh",
            "1.2.3.4:1/ab",
        ] {
            assert_eq!(Invite::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn messages_survive_the_wire() {
        let t = Track {
            locator: TrackLocator::Spotify { id: "abc".into() },
            title: "Swimming Pools".into(),
            artists: vec!["Kendrick Lamar".into()],
            duration: Some(Duration::from_millis(313_000)),
            cover: Some(CoverRef::Url("https://i.scdn.co/x".into())),
            ..Default::default()
        };
        let w = WireTrack::from_track(&t);
        assert_eq!(w.to_track(), t);
        let msgs = vec![
            ToHost::Hello {
                version: VERSION,
                secret: "s".into(),
                name: "Sam".into(),
            },
            ToHost::Control(Control::Insert {
                before: Some(3),
                tracks: vec![w],
            }),
        ];
        let mut buf = Vec::new();
        for m in &msgs {
            send(&mut buf, m).unwrap();
        }
        let mut r = io::Cursor::new(buf);
        for m in &msgs {
            assert_eq!(recv::<ToHost>(&mut r).unwrap().as_ref(), Some(m));
        }
        assert_eq!(recv::<ToHost>(&mut r).unwrap(), None);
    }
}
