//! Being a guest in a jam: the player follows the host, and its controls go to the host.

use std::collections::VecDeque;
use std::io::BufReader;
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use parking_lot::Mutex;

use super::wire::{self, Control, Invite, Playback, ToGuest, ToHost, WireEntry, WireTrack};
use super::{JamStatus, Shared};
use crate::discord::Party;
use crate::model::{EntryId, Queue, QueueEntry};
use crate::player::{Mirror, PlayerCommand as Cmd, State};

const TICK: Duration = Duration::from_millis(50);
const PING_EVERY: Duration = Duration::from_secs(2);
/// Further off the host than this, the song is nudged back in line...
const MAX_DRIFT_MS: i64 = 120;
/// ...but not more often than this (each nudge is a tiny skip).
const NUDGE_GAP: Duration = Duration::from_millis(2500);
/// A seek lands a little behind its target (the output refills first): aim this far ahead.
const SEEK_LEAD_MS: i64 = 25;

pub(crate) struct Guest {
    run: Arc<Run>,
}

struct Run {
    shared: Arc<Shared>,
    stop: AtomicBool,
    out: TcpStream,
    host: String,
    jam: String,
    started: Instant,
    state: Mutex<Follow>,
}

#[derive(Default)]
struct Follow {
    /// The latest playback from the host, with the latest queue entries.
    playback: Option<Playback>,
    entries: Vec<WireEntry>,
    /// (round trip, host clock minus ours) of recent pings.
    clock: VecDeque<(u64, i64)>,
    members: Vec<String>,
    last_nudge: Option<Instant>,
    /// Songs being looked up (a search), by URI; the queue is mirrored again when they're found.
    looking: Vec<String>,
    /// Something was found since the last mirror.
    found_more: bool,
    note: Option<String>,
}

impl Guest {
    pub(crate) fn join(shared: Arc<Shared>, invite: &Invite) -> anyhow::Result<Self> {
        let addr = invite
            .addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| anyhow::anyhow!("no address for {}", invite.addr))?;
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(6)).map_err(|e| {
            anyhow::anyhow!("{e} (is the host on the same network, and the jam still on?)")
        })?;
        stream.set_nodelay(true)?;
        stream.set_write_timeout(Some(Duration::from_secs(3)))?;
        stream.set_read_timeout(Some(Duration::from_secs(8)))?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut out = &stream;
        wire::send(
            &mut out,
            &ToHost::Hello {
                version: wire::VERSION,
                secret: invite.secret.clone(),
                name: shared.name.clone(),
            },
        )?;
        let (jam, host) = match wire::recv::<ToGuest>(&mut reader)? {
            Some(ToGuest::Welcome { jam, host }) => (jam, host),
            Some(ToGuest::Refused { reason }) => anyhow::bail!("{reason}"),
            _ => anyhow::bail!("the host didn't answer like a Khoostic jam"),
        };
        // Pings come every 2 s; silence means the host is gone.
        stream.set_read_timeout(Some(Duration::from_secs(20)))?;
        tracing::info!(%host, "joined a jam");

        let (route, routed) = crossbeam_channel::unbounded();
        shared.player.set_route(Some(route));
        shared.send_local(Cmd::SetAutoSkip(false));
        let run = Arc::new(Run {
            shared,
            stop: AtomicBool::new(false),
            out: stream,
            host: host.clone(),
            jam,
            started: Instant::now(),
            state: Mutex::new(Follow {
                members: vec![host],
                ..Default::default()
            }),
        });
        run.update_status();
        let r = run.clone();
        thread::Builder::new()
            .name("khoostic-jam-listen".into())
            .spawn(move || r.listen(reader))?;
        let r = run.clone();
        thread::Builder::new()
            .name("khoostic-jam-follow".into())
            .spawn(move || r.follow(&routed))?;
        Ok(Self { run })
    }

    pub(crate) fn stop(self) {
        self.run.stop();
    }
}

impl Run {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    fn stop(&self) {
        if self.stop.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut out = &self.out;
        let _ = wire::send(&mut out, &ToHost::Bye);
        let _ = self.out.shutdown(Shutdown::Both);
        // The music carries on here, under this player's own control again.
        self.shared.player.set_route(None);
        self.shared.send_local(Cmd::SetAutoSkip(true));
    }

    fn clock_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// Host clock minus ours, from the quickest recent ping (the least skewed by delays).
    fn offset(f: &Follow) -> Option<(u64, i64)> {
        f.clock.iter().min_by_key(|(rtt, _)| *rtt).copied()
    }

    fn update_status(&self) {
        let f = self.state.lock();
        let members = f.members.clone();
        let latency_ms = Self::offset(&f).map(|(rtt, _)| (rtt / 2) as u32);
        let note = f.note.clone();
        drop(f);
        self.shared.set_party(Some(Party {
            id: self.jam.clone(),
            size: members.len().max(2) as u32,
            label: format!("In {}'s jam", self.host),
        }));
        self.shared.set_status(JamStatus::Joined {
            host: self.host.clone(),
            members,
            latency_ms,
            note,
        });
    }

    /// Messages from the host.
    fn listen(self: Arc<Self>, mut reader: BufReader<TcpStream>) {
        let why = loop {
            match wire::recv::<ToGuest>(&mut reader) {
                Ok(Some(ToGuest::Playback(p))) => self.playback(p),
                Ok(Some(ToGuest::Pong { t, host_ms })) => {
                    let now = self.clock_ms();
                    let rtt = now.saturating_sub(t);
                    let offset = host_ms as i64 + (rtt / 2) as i64 - now as i64;
                    let mut f = self.state.lock();
                    f.clock.push_back((rtt, offset));
                    if f.clock.len() > 8 {
                        f.clock.pop_front();
                    }
                    drop(f);
                    self.update_status();
                }
                Ok(Some(ToGuest::Members(m))) => {
                    self.state.lock().members = m;
                    self.update_status();
                }
                Ok(Some(ToGuest::Ended)) => break format!("{} ended the jam", self.host),
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break "Lost the connection to the jam".to_owned(),
            }
            if self.stopped() {
                return;
            }
        };
        if !self.stopped() {
            tracing::info!("jam over: {why}");
            self.shared.end(why);
        }
    }

    /// New playback from the host: mirror it when what plays changed (drift is `follow`'s).
    fn playback(self: &Arc<Self>, p: Playback) {
        let mut f = self.state.lock();
        let changed = match &f.playback {
            None => true,
            Some(last) => {
                last.queue_version != p.queue_version
                    || last.current != p.current
                    || last.playing != p.playing
                    || last.repeat != p.repeat
            }
        };
        if let Some(e) = &p.entries {
            f.entries = e.clone();
        }
        f.playback = Some(p);
        if changed {
            self.mirror(&mut f);
        }
    }

    /// Where the host is in the song right now (ms), by the synced clock.
    fn host_position(&self, f: &Follow) -> Option<u64> {
        let p = f.playback.as_ref()?;
        if !p.playing || p.loading {
            return Some(p.position_ms);
        }
        let (_, offset) = Self::offset(f).unwrap_or((0, 0));
        let host_now = self.clock_ms() as i64 + offset;
        Some(p.position_ms + (host_now - p.at_ms as i64).max(0) as u64)
    }

    /// Makes this player a copy of the host's.
    fn mirror(self: &Arc<Self>, f: &mut Follow) {
        let Some(p) = f.playback.clone() else { return };
        let shared = &self.shared;
        let mut entries = Vec::with_capacity(f.entries.len());
        let mut missing = Vec::new();
        for e in &f.entries {
            let track = match shared.quick(&e.track) {
                Some(t) => t,
                None => {
                    missing.push((Some(e.id) == p.current, e.track.clone()));
                    e.track.to_track()
                }
            };
            entries.push(QueueEntry {
                id: EntryId(e.id),
                track: Arc::new(track),
            });
        }
        // The current song first, then the rest in order.
        missing.sort_by_key(|(current, _)| !current);
        for (_, w) in missing {
            self.look_up(f, w);
        }
        let current = p.current.map(EntryId);
        let unplayable = entries
            .iter()
            .find(|e| Some(e.id) == current)
            .filter(|e| !shared.playable(&e.track))
            .map(|e| e.track.title.clone());
        let looking = current.is_some_and(|id| {
            f.entries
                .iter()
                .any(|e| e.id == id.0 && f.looking.contains(&e.track.uri))
        });
        f.note = unplayable.map(|title| {
            if looking {
                format!("Finding “{title}”…")
            } else {
                format!("“{title}” isn't available here")
            }
        });
        let position = self.host_position(f).unwrap_or(0);
        f.last_nudge = Some(Instant::now());
        shared.send_local(Cmd::Mirror(Box::new(Mirror {
            entries,
            current,
            repeat: p.repeat.into(),
            position: Duration::from_millis(position),
            playing: p.playing,
        })));
    }

    /// Finds a song in the background (a library or Spotify search).
    fn look_up(self: &Arc<Self>, f: &mut Follow, w: WireTrack) {
        if f.looking.contains(&w.uri) || f.looking.len() > 40 {
            return;
        }
        f.looking.push(w.uri.clone());
        let me = self.shared.clone();
        let run = Arc::downgrade(self);
        let _ = thread::Builder::new()
            .name("khoostic-jam-find".into())
            .spawn(move || {
                me.find(&w);
                if let Some(run) = run.upgrade() {
                    let mut f = run.state.lock();
                    f.looking.retain(|u| *u != w.uri);
                    f.found_more = true;
                }
            });
    }

    /// Keeps time with the host and passes this player's controls on to it.
    fn follow(self: Arc<Self>, routed: &Receiver<Cmd>) {
        let mut last_ping: Option<Instant> = None;
        while !self.stopped() {
            match routed.recv_timeout(TICK) {
                Ok(cmd) => {
                    self.forward(cmd);
                    continue;
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(_) => return,
            }
            if last_ping.is_none_or(|t| t.elapsed() >= PING_EVERY) {
                last_ping = Some(Instant::now());
                let mut out = &self.out;
                let ping = ToHost::Ping { t: self.clock_ms() };
                if wire::send(&mut out, &ping).is_err() {
                    continue; // the listener notices and ends the session
                }
            }
            let mut f = self.state.lock();
            if std::mem::take(&mut f.found_more) {
                self.mirror(&mut f);
                drop(f);
                self.update_status();
                continue;
            }
            self.keep_time(&mut f);
        }
    }

    /// Nudges the song back in line with the host's when it drifted.
    fn keep_time(&self, f: &mut Follow) {
        let Some(p) = &f.playback else { return };
        if !p.playing || p.loading || f.clock.is_empty() {
            return;
        }
        if f.last_nudge.is_some_and(|t| t.elapsed() < NUDGE_GAP) {
            return;
        }
        let snap = self.shared.player.snapshot();
        let same_song = snap.current.as_ref().map(|e| e.id.0) == p.current;
        if !same_song || snap.loading || snap.state != State::Playing {
            return;
        }
        let Some(host) = self.host_position(f) else {
            return;
        };
        let drift = snap.position.as_millis() as i64 - host as i64;
        if drift.abs() > MAX_DRIFT_MS {
            tracing::debug!(drift, "jam: nudging back in time");
            f.last_nudge = Some(Instant::now());
            let to = (host as i64 + SEEK_LEAD_MS).max(0) as u64;
            self.shared.send_local(Cmd::Seek(Duration::from_millis(to)));
        }
    }

    /// One of this player's controls, sent to the host (play/pause also act here at once, so
    /// they feel instant).
    fn forward(&self, cmd: Cmd) {
        if matches!(cmd, Cmd::Play | Cmd::Pause | Cmd::TogglePause) {
            self.shared.send_local(cmd.clone());
        }
        let control = {
            let q = self.shared.player.queue().read();
            control(cmd, &q)
        };
        if let Some(c) = control {
            let mut out = &self.out;
            let _ = wire::send(&mut out, &ToHost::Control(c));
        }
    }
}

/// A player command as a request to the host (positions become entry ids).
fn control(cmd: Cmd, q: &Queue) -> Option<Control> {
    let tracks_of = |t: Vec<crate::model::Track>| t.iter().map(WireTrack::from_track).collect();
    let id_at = |i: usize| q.entries().get(i).map(|e| e.id.0);
    Some(match cmd {
        Cmd::Enqueue(t) => Control::Enqueue(tracks_of(t)),
        Cmd::EnqueueAndPlay(t) => Control::EnqueueAndPlay(tracks_of(t)),
        Cmd::PlayTracks { tracks, start } => Control::PlayTracks {
            tracks: tracks_of(tracks),
            start,
        },
        Cmd::PlayNext(t) => Control::PlayNext(tracks_of(t)),
        Cmd::Insert { index, tracks } => Control::Insert {
            before: id_at(index),
            tracks: tracks_of(tracks),
        },
        Cmd::Move { from, to } => Control::Move {
            id: id_at(from)?,
            to: id_at(to)?,
        },
        Cmd::PlayEntry(id) => Control::PlayEntry(id.0),
        Cmd::Remove(ids) => Control::Remove(wire::entry_ids(&ids)),
        Cmd::Clear => Control::Clear,
        Cmd::Next => Control::Next,
        Cmd::Prev => Control::Prev,
        Cmd::Play => Control::Play,
        Cmd::Pause => Control::Pause,
        Cmd::TogglePause => Control::TogglePause,
        Cmd::Stop => Control::Stop,
        Cmd::Seek(to) => Control::SeekMs(to.as_millis() as u64),
        Cmd::SeekBy(ms) => Control::SeekBy(ms),
        Cmd::ToggleShuffle => Control::ToggleShuffle,
        Cmd::SetShuffle(on) => Control::SetShuffle(on),
        Cmd::SetRepeat(r) => Control::SetRepeat(r.into()),
        Cmd::CycleRepeat => Control::CycleRepeat,
        Cmd::SetVolume(_) | Cmd::VolumeBy(_) | Cmd::Mirror(_) | Cmd::SetAutoSkip(_) => return None,
    })
}
