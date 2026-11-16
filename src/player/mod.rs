//! The player: queue + audio engine + source backends, driven by commands.
//!
//! Frontends (TUI, GUI, MPD server, jam sessions) talk to a [`Player`] only through
//! [`PlayerCommand`]s, cheap [`PlayerSnapshot`]s and the shared queue. That keeps every
//! frontend interchangeable and makes remote control (MPD, listen-along) a thin layer.

pub mod browse;
mod open;
pub mod resolve;

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, select};
use parking_lot::RwLock;

use crate::audio::SourceError;
use crate::audio::{
    AnalysisFrame, Command, EngineConfig, EngineEvent, EngineHandle, PlaybackState,
};
use crate::model::{EntryId, Queue, QueueEntry, RepeatMode, Track};
use open::{Job, Opened, Purpose};

pub use crate::audio::PlaybackState as State;
pub use resolve::{LocalFiles, Resolvers, SourceResolver};

/// "Previous" restarts the current track if we're further in than this.
const RESTART_THRESHOLD: Duration = Duration::from_secs(3);
/// After this many songs in a row fail to open, stop instead of trying the rest of the queue.
const MAX_FAILURES_IN_ROW: u32 = 3;

#[derive(Debug, Clone)]
pub enum PlayerCommand {
    Enqueue(Vec<Track>),
    /// Append and start playing the first of them right away.
    EnqueueAndPlay(Vec<Track>),
    /// Replace the queue with `tracks` and play from `start` (clicking a song in an album,
    /// playlist or search result).
    PlayTracks {
        tracks: Vec<Track>,
        start: usize,
    },
    /// Insert after the current track, in order.
    PlayNext(Vec<Track>),
    /// Insert at a position in the visible queue order (paste).
    Insert {
        index: usize,
        tracks: Vec<Track>,
    },
    /// Move an entry in the visible queue order.
    Move {
        from: usize,
        to: usize,
    },
    PlayEntry(EntryId),
    Remove(Vec<EntryId>),
    Clear,
    Next,
    Prev,
    Play,
    Pause,
    TogglePause,
    Stop,
    Seek(Duration),
    /// Relative seek in milliseconds.
    SeekBy(i64),
    SetVolume(f32),
    VolumeBy(f32),
    ToggleShuffle,
    SetShuffle(bool),
    SetRepeat(RepeatMode),
    CycleRepeat,
    /// Jams: make the queue a copy of the host's and play its current entry (from `position`
    /// when it has to start it; one that's already playing carries on).
    Mirror(Box<Mirror>),
    /// Whether a song that won't open is skipped (jam guests wait for the host instead).
    SetAutoSkip(bool),
}

/// See [`PlayerCommand::Mirror`].
#[derive(Debug, Clone)]
pub struct Mirror {
    /// In play order.
    pub entries: Vec<QueueEntry>,
    pub current: Option<EntryId>,
    pub repeat: RepeatMode,
    pub position: Duration,
    pub playing: bool,
}

impl PlayerCommand {
    /// Commands that change what everyone in a jam hears (everything but volume and the
    /// jam's own commands); a jam guest sends these to the host.
    pub fn shared(&self) -> bool {
        !matches!(
            self,
            Self::SetVolume(_) | Self::VolumeBy(_) | Self::Mirror(_) | Self::SetAutoSkip(_)
        )
    }
}

/// Where commands go: the player thread, or (for shared ones, while following a jam) the jam.
struct Link {
    tx: Sender<PlayerCommand>,
    route: RwLock<Option<Sender<PlayerCommand>>>,
}

impl Link {
    fn send(&self, cmd: PlayerCommand) {
        if cmd.shared()
            && let Some(route) = &*self.route.read()
        {
            let _ = route.send(cmd);
            return;
        }
        let _ = self.tx.send(cmd);
    }
}

#[derive(Debug, Clone)]
pub enum PlayerEvent {
    TrackChanged(Option<QueueEntry>),
    StateChanged(PlaybackState),
    Error(String),
}

#[derive(Debug, Clone, Default)]
pub struct PlayerSnapshot {
    pub state: PlaybackState,
    pub current: Option<QueueEntry>,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub volume: f32,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub buffered: Duration,
    pub sample_rate: u32,
    pub device: String,
    /// `current` is still being opened (fetched, for streams); the engine is behind it.
    pub loading: bool,
    /// While loading: the source asked to wait this much longer (Spotify pacing its keys).
    pub waiting: Option<Duration>,
}

pub struct Player {
    link: Arc<Link>,
    stop: Sender<()>,
    events: Receiver<PlayerEvent>,
    engine: Arc<EngineHandle>,
    queue: Arc<RwLock<Queue>>,
    loading: Arc<RwLock<Option<(EntryId, bool)>>>,
    held: Arc<RwLock<Option<Instant>>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Player {
    pub fn start(config: EngineConfig, resolvers: Resolvers) -> anyhow::Result<Self> {
        let engine = Arc::new(EngineHandle::start(config)?);
        let queue = Arc::new(RwLock::new(Queue::new()));
        let (tx, rx) = crossbeam_channel::unbounded();
        let (stop, stop_rx) = crossbeam_channel::bounded(1);
        let (ev_tx, events) = crossbeam_channel::unbounded();
        let held = Arc::new(RwLock::new(None));
        let (jobs, opened) = open::spawn(resolvers, held.clone())?;
        let loading = Arc::new(RwLock::new(None));

        let mut ctl = Controller {
            engine: engine.clone(),
            queue: queue.clone(),
            events: ev_tx,
            jobs,
            ticket: 0,
            loading: None,
            shown_loading: loading.clone(),
            preloading: None,
            preloaded: None,
            active: false,
            failures: 0,
            auto_skip: true,
        };
        let thread = thread::Builder::new()
            .name("khoostic-player".into())
            .spawn(move || ctl.run(rx, stop_rx, opened))?;

        Ok(Self {
            link: Arc::new(Link {
                tx,
                route: RwLock::new(None),
            }),
            stop,
            events,
            engine,
            queue,
            loading,
            held,
            thread: Some(thread),
        })
    }

    pub fn send(&self, cmd: PlayerCommand) {
        self.link.send(cmd);
    }

    pub fn events(&self) -> &Receiver<PlayerEvent> {
        &self.events
    }

    /// Shared queue, for display. Mutate it through commands so playback stays consistent.
    pub fn queue(&self) -> &RwLock<Queue> {
        &self.queue
    }

    pub fn analysis(&self) -> Arc<AnalysisFrame> {
        self.engine.analysis()
    }

    pub fn snapshot(&self) -> PlayerSnapshot {
        snapshot(&self.engine, &self.queue, &self.loading, &self.held)
    }

    /// A cheap, cloneable handle for background services (scrobbling, autoplay) that need
    /// to watch playback and send commands from their own threads.
    pub fn handle(&self) -> PlayerHandle {
        PlayerHandle {
            link: self.link.clone(),
            engine: self.engine.clone(),
            queue: self.queue.clone(),
            loading: self.loading.clone(),
            held: self.held.clone(),
        }
    }
}

/// See [`Player::handle`]. Keeps working until the [`Player`] is dropped.
#[derive(Clone)]
pub struct PlayerHandle {
    link: Arc<Link>,
    engine: Arc<EngineHandle>,
    queue: Arc<RwLock<Queue>>,
    loading: Arc<RwLock<Option<(EntryId, bool)>>>,
    held: Arc<RwLock<Option<Instant>>>,
}

impl PlayerHandle {
    pub fn send(&self, cmd: PlayerCommand) {
        self.link.send(cmd);
    }

    /// Straight to this player, even while following a jam (the jam itself uses this).
    pub fn send_local(&self, cmd: PlayerCommand) {
        let _ = self.link.tx.send(cmd);
    }

    /// Sends shared commands (see [`PlayerCommand::shared`]) to `route` instead, or stops.
    pub fn set_route(&self, route: Option<Sender<PlayerCommand>>) {
        *self.link.route.write() = route;
    }

    /// Following someone else's jam: background helpers (autoplay) stay out of it.
    pub fn routed(&self) -> bool {
        self.link.route.read().is_some()
    }

    pub fn snapshot(&self) -> PlayerSnapshot {
        snapshot(&self.engine, &self.queue, &self.loading, &self.held)
    }

    pub fn queue(&self) -> &RwLock<Queue> {
        &self.queue
    }
}

fn snapshot(
    engine: &EngineHandle,
    queue: &RwLock<Queue>,
    loading: &RwLock<Option<(EntryId, bool)>>,
    held: &RwLock<Option<Instant>>,
) -> PlayerSnapshot {
    {
        let st = engine.status();
        let q = queue.read();
        // A song being opened shows right away, at its start.
        if let Some((e, paused)) = loading
            .read()
            .and_then(|(id, p)| Some((q.get(id).cloned()?, p)))
        {
            return PlayerSnapshot {
                // What it will do once it's open.
                state: if paused {
                    PlaybackState::Paused
                } else {
                    PlaybackState::Playing
                },
                position: Duration::ZERO,
                duration: e.track.duration,
                current: Some(e),
                volume: st.volume,
                shuffle: q.shuffle(),
                repeat: q.repeat(),
                buffered: Duration::ZERO,
                sample_rate: st.sample_rate,
                device: st.device.clone(),
                loading: true,
                waiting: held
                    .read()
                    .map(|until| until.saturating_duration_since(Instant::now())),
            };
        }
        // Prefer what the engine says is audible; the queue cursor can briefly lead it.
        let current = st
            .token
            .and_then(|t| q.get(EntryId(t)).cloned())
            .or_else(|| q.current().cloned());
        PlayerSnapshot {
            state: st.state,
            current,
            position: st.position,
            duration: st.duration,
            volume: st.volume,
            shuffle: q.shuffle(),
            repeat: q.repeat(),
            buffered: st.buffered,
            sample_rate: st.sample_rate,
            device: st.device.clone(),
            loading: false,
            waiting: None,
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The entry being opened to play (the engine still has whatever played before).
#[derive(Clone, Copy)]
struct Loading {
    ticket: u64,
    id: EntryId,
    /// Start it paused (the caller asked for that, or the user paused while it opened).
    paused: bool,
}

struct Controller {
    engine: Arc<EngineHandle>,
    queue: Arc<RwLock<Queue>>,
    events: Sender<PlayerEvent>,
    jobs: Sender<Job>,
    /// Last ticket handed out; results carrying an older one are stale.
    ticket: u64,
    loading: Option<Loading>,
    /// Shared with snapshots, so frontends show the entry being opened right away.
    shown_loading: Arc<RwLock<Option<(EntryId, bool)>>>,
    /// The gapless follow-up being opened.
    preloading: Option<(u64, EntryId)>,
    /// Entry currently handed to the engine as the gapless follow-up.
    preloaded: Option<EntryId>,
    /// Whether we intend to be playing something (false after Stop / end of queue).
    active: bool,
    /// Songs that failed to open since the last one that played or the last user command.
    failures: u32,
    /// Skip songs that won't open (off for jam guests, who wait for the host).
    auto_skip: bool,
}

impl Controller {
    fn run(&mut self, rx: Receiver<PlayerCommand>, stop: Receiver<()>, opened: Receiver<Opened>) {
        let engine_events = self.engine.events().clone();
        loop {
            select! {
                recv(stop) -> _ => return,
                recv(rx) -> cmd => match cmd {
                    Ok(cmd) => self.command(cmd),
                    Err(_) => return,
                },
                recv(engine_events) -> ev => match ev {
                    Ok(ev) => self.engine_event(ev),
                    Err(_) => return,
                },
                recv(opened) -> o => match o {
                    Ok(o) => self.opened(o),
                    Err(_) => return,
                },
            }
        }
    }

    fn emit(&self, ev: PlayerEvent) {
        let _ = self.events.send(ev);
    }

    fn command(&mut self, cmd: PlayerCommand) {
        self.command_inner(cmd);
        // Pause/play while a song opens changes what snapshots should say.
        self.set_loading(self.loading);
    }

    fn command_inner(&mut self, cmd: PlayerCommand) {
        use PlayerCommand::*;
        // Whatever the user does next gets the full benefit of the doubt again.
        self.failures = 0;
        match cmd {
            Enqueue(tracks) => {
                self.queue.write().extend(tracks);
                self.refresh_preload();
            }
            EnqueueAndPlay(tracks) => {
                let first = self.queue.write().extend(tracks).first().copied();
                if let Some(id) = first {
                    self.queue.write().jump_to(id);
                    self.load_current();
                }
            }
            PlayTracks { tracks, start } => {
                let target = {
                    let mut q = self.queue.write();
                    q.clear();
                    let ids = q.extend(tracks);
                    ids.get(start).or(ids.first()).copied()
                };
                match target {
                    Some(id) => {
                        self.queue.write().jump_to(id);
                        self.load_current();
                    }
                    None => self.stop(),
                }
            }
            PlayNext(tracks) => {
                self.queue.write().push_next_many(tracks);
                self.refresh_preload();
            }
            Insert { index, tracks } => {
                self.queue.write().insert_at(index, tracks);
                self.refresh_preload();
            }
            Move { from, to } => {
                self.queue.write().move_entry(from, to);
                self.refresh_preload();
            }
            PlayEntry(id) => {
                if self.queue.write().jump_to(id) {
                    self.load_current();
                }
            }
            Remove(ids) => {
                let mut removed_current = false;
                {
                    let mut q = self.queue.write();
                    for id in ids {
                        removed_current |= q.remove(id);
                    }
                }
                if removed_current && self.active {
                    // Keep the user's play/pause state across the jump.
                    let paused = self.paused();
                    self.load_current_with(paused);
                } else if removed_current {
                    self.stop();
                } else {
                    self.refresh_preload();
                }
            }
            Clear => {
                self.queue.write().clear();
                self.stop();
            }
            Next => {
                let has_next = self.queue.write().skip_next().is_some();
                if has_next {
                    let paused = self.loading.is_some_and(|l| l.paused);
                    self.load_current_with(paused)
                } else {
                    self.stop()
                }
            }
            Prev => {
                let st = self.engine.status();
                // While a song opens, the engine's position belongs to the one before it.
                if self.loading.is_none() && st.token.is_some() && st.position > RESTART_THRESHOLD {
                    self.engine.send(Command::Seek(Duration::ZERO));
                } else if self.queue.write().skip_prev().is_some() {
                    let paused = self.loading.is_some_and(|l| l.paused);
                    self.load_current_with(paused);
                }
            }
            Play => {
                // While a song opens only the intent changes (the old song stays quiet).
                if let Some(l) = &mut self.loading {
                    l.paused = false;
                } else if self.engine.status().state == PlaybackState::Stopped {
                    let has = {
                        let mut q = self.queue.write();
                        q.current().is_some() || q.skip_next().is_some()
                    };
                    if has {
                        self.load_current();
                    }
                } else {
                    self.engine.send(Command::Play);
                }
            }
            Pause => {
                if let Some(l) = &mut self.loading {
                    l.paused = true;
                }
                self.engine.send(Command::Pause);
            }
            TogglePause => {
                if let Some(l) = &mut self.loading {
                    l.paused = !l.paused;
                } else if self.engine.status().state == PlaybackState::Stopped {
                    self.command_inner(Play);
                } else {
                    self.engine.send(Command::TogglePause);
                }
            }
            Stop => self.stop(),
            // Seeking would move the song that's being replaced.
            Seek(_) | SeekBy(_) if self.loading.is_some() => {}
            Seek(to) => self.engine.send(Command::Seek(to)),
            SeekBy(ms) => {
                let st = self.engine.status();
                let pos = st.position.as_millis() as i64 + ms;
                let mut to = Duration::from_millis(pos.max(0) as u64);
                if let Some(d) = st.duration {
                    to = to.min(d.saturating_sub(Duration::from_millis(250)));
                }
                self.engine.send(Command::Seek(to));
            }
            SetVolume(v) => self.engine.send(Command::SetVolume(v)),
            VolumeBy(dv) => {
                let v = self.engine.status().volume + dv;
                self.engine.send(Command::SetVolume(v.clamp(0.0, 1.0)));
            }
            ToggleShuffle => {
                {
                    let mut q = self.queue.write();
                    let on = !q.shuffle();
                    q.set_shuffle(on);
                }
                self.refresh_preload();
            }
            CycleRepeat => {
                {
                    let mut q = self.queue.write();
                    let r = q.repeat().cycle();
                    q.set_repeat(r);
                }
                self.refresh_preload();
            }
            SetShuffle(on) => {
                self.queue.write().set_shuffle(on);
                self.refresh_preload();
            }
            SetRepeat(mode) => {
                self.queue.write().set_repeat(mode);
                self.refresh_preload();
            }
            SetAutoSkip(on) => self.auto_skip = on,
            Mirror(m) => self.mirror(*m),
        }
    }

    /// Whether the user has things paused (counting a song that's still opening).
    fn paused(&self) -> bool {
        match self.loading {
            Some(l) => l.paused,
            None => self.engine.status().state == PlaybackState::Paused,
        }
    }

    fn engine_event(&mut self, ev: EngineEvent) {
        match ev {
            // A preload that slipped through before the switch: the song being opened wins.
            EngineEvent::TrackStarted { .. } if self.loading.is_some() => {}
            EngineEvent::TrackStarted { token, .. } => {
                let id = EntryId(token);
                if self.preloaded == Some(id) {
                    self.preloaded = None;
                }
                let entry = {
                    let mut q = self.queue.write();
                    if q.current_id() != Some(id) {
                        q.jump_to(id); // gapless transition into the preloaded entry
                    }
                    q.get(id).cloned()
                };
                self.emit(PlayerEvent::TrackChanged(entry));
                self.refresh_preload();
            }
            EngineEvent::AboutToFinish { .. } => self.refresh_preload(),
            EngineEvent::TrackFinished { .. } => {}
            EngineEvent::StateChanged(PlaybackState::Stopped) if self.loading.is_some() => {
                // The old song ran out while the next one opens; it takes over when ready.
                self.emit(PlayerEvent::StateChanged(PlaybackState::Stopped));
            }
            EngineEvent::StateChanged(state) => {
                if state == PlaybackState::Stopped {
                    let was_active = self.active;
                    self.active = false;
                    self.preloaded = None;
                    self.preloading = None;
                    let carry_on = {
                        let mut q = self.queue.write();
                        if q.peek_next().is_some() {
                            // Ran out before the follow-up was ready (slow to open, or it
                            // failed): carry on with it the normal way.
                            was_active && q.skip_next().is_some()
                        } else {
                            // Played off the end: park the cursor after the last entry.
                            if q.current().is_some() {
                                q.advance();
                            }
                            false
                        }
                    };
                    if carry_on {
                        self.load_current();
                        return;
                    }
                    self.emit(PlayerEvent::TrackChanged(None));
                }
                self.emit(PlayerEvent::StateChanged(state));
            }
            EngineEvent::Seeked { .. } => {}
            EngineEvent::SourceError { token, message } => {
                let title = self
                    .queue
                    .read()
                    .get(EntryId(token))
                    .map(|e| e.track.title.clone());
                let what = title.unwrap_or_else(|| format!("entry {token}"));
                self.emit(PlayerEvent::Error(format!("{what}: {message}")));
            }
            EngineEvent::OutputError(e) => self.emit(PlayerEvent::Error(format!("output: {e}"))),
        }
    }

    fn next_ticket(&mut self) -> u64 {
        self.ticket += 1;
        self.ticket
    }

    fn set_loading(&mut self, loading: Option<Loading>) {
        self.loading = loading;
        *self.shown_loading.write() = loading.map(|l| (l.id, l.paused));
    }

    fn stop(&mut self) {
        self.active = false;
        self.preloaded = None;
        self.preloading = None;
        self.set_loading(None);
        self.engine.send(Command::Stop);
    }

    /// Starts opening the queue's current entry; it plays once it's open (see `opened`).
    fn load_current(&mut self) {
        self.load_current_with(false);
    }

    fn load_current_with(&mut self, paused: bool) {
        self.load_current_at(paused, Duration::ZERO);
    }

    fn load_current_at(&mut self, paused: bool, start_at: Duration) {
        let Some(entry) = self.queue.read().current().cloned() else {
            self.stop();
            return;
        };
        // The old song goes quiet now: it would only play on under the new one's title while
        // that opens (which can take a while from Spotify). Paused, not stopped: a "stopped"
        // arriving after the new song is in would read as a song ending and skip on.
        if self.loading.is_none() && self.engine.status().state == PlaybackState::Playing {
            self.engine.send(Command::Pause);
        }
        // The engine must not slide into the old follow-up while this one opens.
        if self.preloaded.take().is_some() {
            self.engine.send(Command::ClearPreload);
        }
        self.preloading = None;
        self.active = true;
        let ticket = self.next_ticket();
        self.set_loading(Some(Loading {
            ticket,
            id: entry.id,
            paused,
        }));
        let _ = self.jobs.send(Job {
            ticket,
            id: entry.id,
            track: entry.track,
            purpose: Purpose::Play,
            start_at,
        });
    }

    fn opened(&mut self, o: Opened) {
        match o.purpose {
            Purpose::Play => {
                let Some(l) = self.loading.filter(|l| l.ticket == o.ticket) else {
                    return; // overtaken by a newer pick
                };
                self.set_loading(None);
                match o.result {
                    Ok(source) => {
                        self.failures = 0;
                        self.engine.load(l.id.0, source, l.paused);
                    }
                    Err(e) => self.failed(l, &e),
                }
            }
            Purpose::Preload => {
                if self.preloading != Some((o.ticket, o.id)) {
                    return;
                }
                self.preloading = None;
                // A failed follow-up is retried (and reported) when its turn comes.
                if let Ok(source) = o.result
                    && self.active
                    && self.loading.is_none()
                {
                    self.preloaded = Some(o.id);
                    self.engine.preload(o.id.0, source);
                }
            }
        }
    }

    /// A song wouldn't open: report it and move on, but don't run through the whole queue
    /// when nothing will play (Spotify down, signed out).
    fn failed(&mut self, l: Loading, e: &SourceError) {
        let title = self
            .queue
            .read()
            .get(l.id)
            .map_or_else(|| "A song".to_owned(), |e| e.track.title.clone());
        // Spotify kept saying "wait" for minutes: nothing else would play either.
        if matches!(e, SourceError::RetryLater { .. }) {
            self.emit(PlayerEvent::Error(format!(
                "Spotify isn't letting songs load right now; try {title} again in a minute"
            )));
            self.stop();
            return;
        }
        self.failures += 1;
        if self.failures >= MAX_FAILURES_IN_ROW {
            self.emit(PlayerEvent::Error(format!(
                "{title}: {e} (stopped: {} songs in a row wouldn't play)",
                self.failures
            )));
            self.failures = 0;
            self.stop();
            return;
        }
        self.emit(PlayerEvent::Error(format!("{title}: {e}")));
        if !self.auto_skip {
            self.stop();
            return;
        }
        let has_next = {
            let mut q = self.queue.write();
            q.current_id() == Some(l.id) && q.skip_next().is_some()
        };
        if has_next {
            self.load_current_with(l.paused);
        } else {
            self.stop();
        }
    }

    /// Becomes a copy of a jam host's queue, carrying on with the song already playing if the
    /// host is on it too.
    fn mirror(&mut self, m: Mirror) {
        let playing = self
            .loading
            .map(|l| l.id)
            .or_else(|| self.engine.status().token.map(EntryId));
        let keep = m.current.is_some() && m.current == playing && {
            let q = self.queue.read();
            let mine = m.current.and_then(|id| q.get(id));
            let theirs = m.entries.iter().find(|e| Some(e.id) == m.current);
            mine.zip(theirs)
                .is_some_and(|(a, b)| a.track.locator == b.track.locator)
        };
        self.queue.write().mirror(m.entries, m.current, m.repeat);
        if m.current.is_none() {
            return self.stop();
        }
        if !keep {
            return self.load_current_at(!m.playing, m.position);
        }
        match &mut self.loading {
            Some(l) => l.paused = !m.playing,
            None => {
                self.refresh_preload();
                self.engine.send(if m.playing {
                    Command::Play
                } else {
                    Command::Pause
                });
            }
        }
    }

    /// Makes sure the engine's gapless follow-up matches what the queue says comes next.
    fn refresh_preload(&mut self) {
        if !self.active || self.loading.is_some() {
            return;
        }
        let next = self.queue.read().peek_next().cloned();
        let Some(next) = next else {
            self.preloading = None;
            if self.preloaded.take().is_some() {
                self.engine.send(Command::ClearPreload);
            }
            return;
        };
        if self.preloaded == Some(next.id) || self.preloading.is_some_and(|p| p.1 == next.id) {
            return;
        }
        if self.preloaded.take().is_some() {
            self.engine.send(Command::ClearPreload);
        }
        // Repeat-one: the "next" entry is the current one; this opens a fresh decoder of it.
        let ticket = self.next_ticket();
        self.preloading = Some((ticket, next.id));
        let _ = self.jobs.send(Job {
            ticket,
            id: next.id,
            track: next.track,
            purpose: Purpose::Preload,
            start_at: Duration::ZERO,
        });
    }
}
