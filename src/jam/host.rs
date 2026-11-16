//! Hosting a jam: accept guests, stream the player's state to them, apply their requests.

use std::io::BufReader;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use super::wire::{self, Control, Invite, Playback, ToGuest, ToHost, WireEntry, WireTrack};
use super::{JamStatus, Shared};
use crate::discord::Party;
use crate::model::EntryId;
use crate::player::{PlayerCommand as Cmd, State};

/// How often the host's playback is checked for changes.
const TICK: Duration = Duration::from_millis(100);
/// Sent even when nothing changed, so guests keep correcting drift.
const HEARTBEAT: Duration = Duration::from_secs(2);
/// Songs sent before and after the current one.
const WINDOW: (usize, usize) = (20, 300);
/// A guest that says nothing for this long (they ping every 2 s) is gone.
const SILENCE: Duration = Duration::from_secs(20);

pub(crate) struct Host {
    run: Arc<Run>,
}

struct Run {
    shared: Arc<Shared>,
    stop: AtomicBool,
    jam: String,
    secret: String,
    invite: String,
    started: Instant,
    guests: Mutex<Vec<Guest>>,
    next_guest: AtomicU64,
    /// What guests were last told (queue entries are only sent when they change).
    told: Mutex<Option<(Playback, Instant)>>,
}

struct Guest {
    id: u64,
    name: String,
    out: TcpStream,
}

impl Host {
    pub(crate) fn start(shared: Arc<Shared>) -> anyhow::Result<Self> {
        let (listener, port) = bind(&shared.bind, shared.port)?;
        listener.set_nonblocking(true)?;
        let secret = super::secret();
        let invite = Invite {
            addr: match listener.local_addr()?.ip() {
                ip if ip.is_unspecified() => format!("{}:{port}", super::lan_ip()),
                ip if ip.is_ipv6() => format!("[{ip}]:{port}"),
                ip => format!("{ip}:{port}"),
            },
            secret: secret.clone(),
        }
        .to_string();
        let run = Arc::new(Run {
            shared,
            stop: AtomicBool::new(false),
            jam: format!("khoostic-jam-{}", super::secret()),
            secret,
            invite,
            started: Instant::now(),
            guests: Mutex::default(),
            next_guest: AtomicU64::new(1),
            told: Mutex::new(None),
        });
        run.update_status();
        let r = run.clone();
        thread::Builder::new()
            .name("khoostic-jam-accept".into())
            .spawn(move || r.accept(listener))?;
        let r = run.clone();
        thread::Builder::new()
            .name("khoostic-jam-host".into())
            .spawn(move || r.broadcast_loop())?;
        tracing::info!(invite = %run.invite, "hosting a jam");
        Ok(Self { run })
    }

    pub(crate) fn stop(self) {
        let run = &self.run;
        run.stop.store(true, Ordering::Release);
        for g in run.guests.lock().drain(..) {
            let mut out = &g.out;
            let _ = wire::send(&mut out, &ToGuest::Ended);
            let _ = g.out.shutdown(Shutdown::Both);
        }
    }
}

/// The configured port, or the next free one.
fn bind(addr: &str, port: u16) -> anyhow::Result<(TcpListener, u16)> {
    let addr = if addr.is_empty() { "0.0.0.0" } else { addr };
    let mut last = None;
    for p in port..port.saturating_add(10) {
        match TcpListener::bind((addr, p)) {
            Ok(l) => return Ok((l, p)),
            Err(e) => last = Some(e),
        }
    }
    Err(anyhow::anyhow!(
        "no free port from {port}: {}",
        last.map_or_else(String::new, |e| e.to_string())
    ))
}

impl Run {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    fn clock_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn update_status(&self) {
        let guests: Vec<String> = self.guests.lock().iter().map(|g| g.name.clone()).collect();
        self.shared.set_party(Some(Party {
            id: self.jam.clone(),
            size: 1 + guests.len() as u32,
            label: "Hosting a jam".into(),
        }));
        self.shared.set_status(JamStatus::Hosting {
            invite: self.invite.clone(),
            guests,
        });
    }

    fn accept(self: Arc<Self>, listener: TcpListener) {
        while !self.stopped() {
            match listener.accept() {
                Ok((stream, addr)) => {
                    let me = self.clone();
                    let _ = thread::Builder::new()
                        .name("khoostic-jam-guest".into())
                        .spawn(move || me.serve(stream, addr));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(100))
                }
                Err(e) => {
                    tracing::warn!("jam: accepting: {e}");
                    thread::sleep(Duration::from_millis(500));
                }
            }
        }
    }

    /// One guest's connection, from hello to goodbye.
    fn serve(self: Arc<Self>, stream: TcpStream, addr: SocketAddr) {
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_nodelay(true);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(8)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
        let Ok(read) = stream.try_clone() else { return };
        let mut reader = BufReader::new(read);
        let mut out = &stream;
        let name = match wire::recv::<ToHost>(&mut reader) {
            Ok(Some(ToHost::Hello {
                version,
                secret,
                name,
            })) => {
                let refuse = if version != wire::VERSION {
                    Some("they're on a different Khoostic version")
                } else if secret != self.secret {
                    Some("that invite isn't for this jam (it may have ended)")
                } else if self.guests.lock().len() >= super::MAX_GUESTS {
                    Some("the jam is full")
                } else {
                    None
                };
                if let Some(reason) = refuse {
                    let _ = wire::send(
                        &mut out,
                        &ToGuest::Refused {
                            reason: reason.into(),
                        },
                    );
                    return;
                }
                name.chars().take(40).collect::<String>()
            }
            _ => return,
        };
        tracing::info!(%name, %addr, "jam: guest joined");
        let id = self.next_guest.fetch_add(1, Ordering::Relaxed);
        {
            let mut guests = self.guests.lock();
            let welcome = ToGuest::Welcome {
                jam: self.jam.clone(),
                host: self.shared.name.clone(),
            };
            let full = ToGuest::Playback(self.playback(true));
            if wire::send(&mut out, &welcome).is_err() || wire::send(&mut out, &full).is_err() {
                return;
            }
            let Ok(out) = stream.try_clone() else { return };
            guests.push(Guest {
                id,
                name: name.clone(),
                out,
            });
        }
        self.members_changed();
        let _ = stream.set_read_timeout(Some(SILENCE));

        while !self.stopped() {
            match wire::recv::<ToHost>(&mut reader) {
                Ok(Some(ToHost::Ping { t })) => {
                    let pong = ToGuest::Pong {
                        t,
                        host_ms: self.clock_ms(),
                    };
                    if wire::send(&mut out, &pong).is_err() {
                        break;
                    }
                }
                Ok(Some(ToHost::Control(c))) => self.apply(c),
                Ok(Some(ToHost::Hello { .. })) => {}
                Ok(Some(ToHost::Bye)) | Ok(None) | Err(_) => break,
            }
        }
        tracing::info!(%name, "jam: guest left");
        self.guests.lock().retain(|g| g.id != id);
        let _ = stream.shutdown(Shutdown::Both);
        if !self.stopped() {
            self.members_changed();
        }
    }

    fn members_changed(&self) {
        let mut members = vec![self.shared.name.clone()];
        members.extend(self.guests.lock().iter().map(|g| g.name.clone()));
        self.send_all(&ToGuest::Members(members));
        self.update_status();
    }

    fn send_all(&self, msg: &ToGuest) {
        let mut guests = self.guests.lock();
        guests.retain(|g| {
            let mut out = &g.out;
            let ok = wire::send(&mut out, msg).is_ok();
            if !ok {
                let _ = g.out.shutdown(Shutdown::Both);
            }
            ok
        });
    }

    /// The host's playback now; `entries` adds the queue window.
    fn playback(&self, entries: bool) -> Playback {
        let player = &self.shared.player;
        let snap = player.snapshot();
        let q = player.queue().read();
        let entries = entries.then(|| {
            q.play_window(WINDOW.0, WINDOW.1)
                .iter()
                .map(|e| WireEntry {
                    id: e.id.0,
                    track: WireTrack::from_track(&e.track),
                })
                .collect()
        });
        Playback {
            queue_version: q.version(),
            entries,
            current: snap.current.as_ref().map(|e| e.id.0),
            playing: snap.state == State::Playing,
            repeat: q.repeat().into(),
            shuffle: q.shuffle(),
            position_ms: snap.position.as_millis() as u64,
            at_ms: self.clock_ms(),
            loading: snap.loading,
        }
    }

    /// Tells guests whenever the playback changes in a way they can't predict.
    fn broadcast_loop(self: Arc<Self>) {
        while !self.stopped() {
            thread::sleep(TICK);
            if self.guests.lock().is_empty() {
                *self.told.lock() = None;
                continue;
            }
            let now = self.playback(false);
            let mut told = self.told.lock();
            let send = match &*told {
                None => Some(true),
                Some((last, when)) => {
                    let queue = last.queue_version != now.queue_version;
                    let changed = queue
                        || last.current != now.current
                        || last.playing != now.playing
                        || last.loading != now.loading
                        || last.repeat != now.repeat;
                    // Where the last message says we'd be by now.
                    let expected = last.position_ms
                        + if last.playing {
                            now.at_ms - last.at_ms
                        } else {
                            0
                        };
                    let jumped = now.position_ms.abs_diff(expected) > 800;
                    (changed || jumped || when.elapsed() > HEARTBEAT).then_some(queue)
                }
            };
            let Some(with_entries) = send else { continue };
            let msg = if with_entries {
                self.playback(true)
            } else {
                now
            };
            *told = Some((msg.clone(), Instant::now()));
            drop(told);
            self.send_all(&ToGuest::Playback(msg));
        }
    }

    /// A guest's request, done to the host's player (the change then reaches everyone).
    fn apply(&self, c: Control) {
        let shared = &self.shared;
        let tracks = |ws: Vec<WireTrack>| ws.iter().map(|w| shared.find(w)).collect::<Vec<_>>();
        let index_of = |id: u64| shared.player.queue().read().index_of(EntryId(id));
        let cmd = match c {
            Control::Enqueue(t) => Cmd::Enqueue(tracks(t)),
            Control::EnqueueAndPlay(t) => Cmd::EnqueueAndPlay(tracks(t)),
            Control::PlayTracks { tracks: t, start } => Cmd::PlayTracks {
                tracks: tracks(t),
                start,
            },
            Control::PlayNext(t) => Cmd::PlayNext(tracks(t)),
            Control::Insert { before, tracks: t } => {
                let index = before
                    .and_then(index_of)
                    .unwrap_or_else(|| shared.player.queue().read().len());
                Cmd::Insert {
                    index,
                    tracks: tracks(t),
                }
            }
            Control::Move { id, to } => match (index_of(id), index_of(to)) {
                (Some(from), Some(to)) => Cmd::Move { from, to },
                _ => return,
            },
            Control::PlayEntry(id) => Cmd::PlayEntry(EntryId(id)),
            Control::Remove(ids) => Cmd::Remove(ids.into_iter().map(EntryId).collect()),
            Control::Clear => Cmd::Clear,
            Control::Next => Cmd::Next,
            Control::Prev => Cmd::Prev,
            Control::Play => Cmd::Play,
            Control::Pause => Cmd::Pause,
            Control::TogglePause => Cmd::TogglePause,
            Control::Stop => Cmd::Stop,
            Control::SeekMs(ms) => Cmd::Seek(Duration::from_millis(ms)),
            Control::SeekBy(ms) => Cmd::SeekBy(ms),
            Control::ToggleShuffle => Cmd::ToggleShuffle,
            Control::SetShuffle(on) => Cmd::SetShuffle(on),
            Control::SetRepeat(r) => Cmd::SetRepeat(r.into()),
            Control::CycleRepeat => Cmd::CycleRepeat,
        };
        shared.send_local(cmd);
    }
}
