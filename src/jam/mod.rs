//! Jams: listening together. One person hosts; friends join with an invite and hear the
//! same songs at the same moment, and everyone can add songs, skip, pause and seek.
//!
//! * The host is the source of truth. Its player runs as usual; [`host`] streams its queue
//!   and position to the guests and applies their requests to it.
//! * A guest's player becomes a copy of the host's ([`PlayerCommand::Mirror`]): same queue
//!   (with the host's entry ids), same song, and a synced clock keeps it within a fraction of
//!   a second ([`guest`]). The guest's own controls are sent to the host instead.
//! * Songs travel as descriptions ([`wire::WireTrack`]). Each peer plays its own copy:
//!   Spotify songs from Spotify, someone's own files from the same file in the library (or
//!   Spotify's version of the song). A song nobody can get stays silent until the next one.
//!
//! Connections are direct TCP (newline-delimited JSON): the host listens on a port and the
//! invite says where. On one network that just works; across the internet the host forwards
//! the port or everyone joins the same VPN (Tailscale, ZeroTier).

mod guest;
mod host;
pub mod wire;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

use crate::config::JamConfig;
use crate::discord::{Discord, Party};
use crate::discovery::Discovery;
use crate::model::{Track, TrackLocator};
use crate::player::{PlayerCommand, PlayerHandle};
use crate::spotify::{Spotify, Status as SpotifyStatus};
use wire::{Invite, WireTrack};

/// Guests per jam.
pub const MAX_GUESTS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JamStatus {
    Idle,
    Hosting {
        invite: String,
        /// Who's listening along (not you).
        guests: Vec<String>,
    },
    Joining,
    Joined {
        host: String,
        /// Everyone, host first.
        members: Vec<String>,
        /// One way, to the host.
        latency_ms: Option<u32>,
        /// Why you might not hear anything right now.
        note: Option<String>,
    },
    /// The last jam ended (or couldn't start), and why.
    Ended(String),
}

pub struct Jams {
    shared: Arc<Shared>,
}

/// What the host and guest sides share.
pub(crate) struct Shared {
    player: PlayerHandle,
    discovery: Arc<Discovery>,
    spotify: Option<Arc<Spotify>>,
    discord: Option<Arc<Discord>>,
    name: String,
    port: u16,
    bind: String,
    status: Mutex<JamStatus>,
    version: AtomicU64,
    session: Mutex<Option<Session>>,
    /// Bumped by every join and leave: a join that finishes connecting after its ticket
    /// changed is dropped.
    ticket: AtomicU64,
    /// Songs from other peers, as this peer plays them, by URI.
    found: Mutex<HashMap<String, Track>>,
}

enum Session {
    Host(host::Host),
    Guest(guest::Guest),
}

impl Jams {
    pub fn new(
        config: &JamConfig,
        player: PlayerHandle,
        discovery: Arc<Discovery>,
        spotify: Option<Arc<Spotify>>,
        discord: Option<Arc<Discord>>,
    ) -> Arc<Self> {
        let name = Some(config.name.trim().to_owned())
            .filter(|n| !n.is_empty())
            .or_else(|| std::env::var("USERNAME").ok())
            .or_else(|| std::env::var("USER").ok())
            .unwrap_or_else(|| "Someone".into());
        Arc::new(Self {
            shared: Arc::new(Shared {
                player,
                discovery,
                spotify,
                discord,
                name,
                port: config.port,
                bind: config.bind.trim().to_owned(),
                status: Mutex::new(JamStatus::Idle),
                version: AtomicU64::new(0),
                session: Mutex::new(None),
                ticket: AtomicU64::new(0),
                found: Mutex::default(),
            }),
        })
    }

    pub fn status(&self) -> JamStatus {
        self.shared.status.lock().clone()
    }

    /// Bumped on every status change (for redraws).
    pub fn version(&self) -> u64 {
        self.shared.version.load(Ordering::Acquire)
    }

    pub fn active(&self) -> bool {
        self.shared.session.lock().is_some()
    }

    /// Your name as others in a jam see it.
    pub fn name(&self) -> &str {
        &self.shared.name
    }

    /// Starts hosting (leaving any jam first).
    pub fn host(&self) {
        self.leave();
        match host::Host::start(self.shared.clone()) {
            Ok(h) => *self.shared.session.lock() = Some(Session::Host(h)),
            Err(e) => self.shared.end(format!("Couldn't start a jam: {e:#}")),
        }
    }

    /// Joins the jam behind `invite` (in the background; see [`Jams::status`]).
    pub fn join(&self, invite: &str) -> anyhow::Result<()> {
        let invite = Invite::parse(invite)
            .ok_or_else(|| anyhow::anyhow!("that doesn't look like a jam invite"))?;
        self.leave();
        let ticket = self.shared.ticket.fetch_add(1, Ordering::AcqRel) + 1;
        self.shared.set_status(JamStatus::Joining);
        let shared = self.shared.clone();
        std::thread::Builder::new()
            .name("khoostic-jam-join".into())
            .spawn(move || match guest::Guest::join(shared.clone(), &invite) {
                Ok(g) => {
                    let mut s = shared.session.lock();
                    if shared.ticket.load(Ordering::Acquire) == ticket && s.is_none() {
                        *s = Some(Session::Guest(g));
                    } else {
                        g.stop(); // left (or started something else) while connecting
                    }
                }
                Err(e) if shared.ticket.load(Ordering::Acquire) == ticket => {
                    shared.end(format!("Couldn't join: {e:#}"))
                }
                Err(_) => {}
            })?;
        Ok(())
    }

    /// Leaves the jam (ending it for everyone when hosting).
    pub fn leave(&self) {
        self.shared.ticket.fetch_add(1, Ordering::AcqRel);
        let session = self.shared.session.lock().take();
        match session {
            Some(Session::Host(h)) => h.stop(),
            Some(Session::Guest(g)) => g.stop(),
            None if self.status() == JamStatus::Joining => {}
            None => return,
        }
        self.shared.set_status(JamStatus::Idle);
        self.shared.set_party(None);
    }
}

impl Drop for Jams {
    fn drop(&mut self) {
        self.leave();
    }
}

impl Shared {
    fn set_status(&self, s: JamStatus) {
        *self.status.lock() = s;
        self.version.fetch_add(1, Ordering::AcqRel);
    }

    /// The session ended on its own (the host left, the connection dropped).
    fn end(&self, why: String) {
        if let Some(s) = self.session.lock().take() {
            match s {
                Session::Host(h) => h.stop(),
                Session::Guest(g) => g.stop(),
            }
        }
        self.set_party(None);
        self.set_status(JamStatus::Ended(why));
    }

    fn set_party(&self, party: Option<Party>) {
        if let Some(d) = &self.discord {
            d.set_party(party);
        }
    }

    fn spotify_ready(&self) -> bool {
        self.spotify
            .as_ref()
            .is_some_and(|s| matches!(s.status(), SpotifyStatus::Connected(a) if a.premium))
    }

    /// This peer's copy of a song, if finding it is quick (no searching); `None` means
    /// [`Shared::find`] has to look.
    fn quick(&self, w: &WireTrack) -> Option<Track> {
        if let Some(t) = self.found.lock().get(&w.uri) {
            return Some(t.clone());
        }
        let theirs = w.to_track();
        match &theirs.locator {
            TrackLocator::Spotify { .. } if self.spotify_ready() => Some(theirs),
            TrackLocator::Local(p) if p.is_file() => Some(theirs),
            _ => None,
        }
    }

    /// This peer's copy of a song: the same file or Spotify song, the library's copy, or
    /// Spotify's version. Falls back to the original (which won't play here). Can take a few
    /// seconds (searches); remembered.
    fn find(&self, w: &WireTrack) -> Track {
        if let Some(t) = self.quick(w) {
            return t;
        }
        let theirs = w.to_track();
        let found = theirs
            .artists
            .first()
            .and_then(|a| self.discovery.find_song(a, &theirs.title))
            .map(|mut t| {
                // Keep their cover when the copy has none.
                if t.cover.is_none() {
                    t.cover = theirs.cover.clone();
                }
                t
            });
        let t = found.unwrap_or(theirs);
        self.found.lock().insert(w.uri.clone(), t.clone());
        t
    }

    /// Whether this peer can play a song (after [`Shared::find`]).
    fn playable(&self, t: &Track) -> bool {
        match &t.locator {
            TrackLocator::Spotify { .. } => self.spotify_ready(),
            TrackLocator::Local(p) => p.is_file(),
            _ => true,
        }
    }

    fn send_local(&self, cmd: PlayerCommand) {
        self.player.send_local(cmd);
    }
}

/// This machine's address on its network (where friends can reach a jam you host).
fn lan_ip() -> String {
    // Connecting a UDP socket sends nothing; it just picks the outgoing interface.
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".into())
}

/// A random secret for invites.
fn secret() -> String {
    const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
    let mut bytes = [0u8; 10];
    if getrandom::fill(&mut bytes).is_err() {
        bytes.iter_mut().for_each(|b| *b = fastrand::u8(..));
    }
    bytes
        .iter()
        .map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char)
        .collect()
}
