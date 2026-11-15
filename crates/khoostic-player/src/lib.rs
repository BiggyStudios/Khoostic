//! The player: queue + audio engine + source backends, driven by commands.
//!
//! Frontends (TUI, GUI, MPD server, jam sessions) talk to a [`Player`] only through
//! [`PlayerCommand`]s, cheap [`PlayerSnapshot`]s and the shared queue. That keeps every
//! frontend interchangeable and makes remote control (MPD, listen-along) a thin layer.

pub mod browse;
pub mod resolve;

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, select};
use khoostic_audio::{
    AnalysisFrame, Command, EngineConfig, EngineEvent, EngineHandle, PlaybackState,
};
use khoostic_core::{EntryId, Queue, QueueEntry, RepeatMode, Track};
use parking_lot::RwLock;

pub use khoostic_audio::PlaybackState as State;
pub use resolve::{LocalFiles, Resolvers, SourceResolver};

/// "Previous" restarts the current track if we're further in than this.
const RESTART_THRESHOLD: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
pub enum PlayerCommand {
    Enqueue(Vec<Track>),
    /// Append and start playing the first of them right away.
    EnqueueAndPlay(Vec<Track>),
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
}

pub struct Player {
    tx: Sender<PlayerCommand>,
    stop: Sender<()>,
    events: Receiver<PlayerEvent>,
    engine: Arc<EngineHandle>,
    queue: Arc<RwLock<Queue>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Player {
    pub fn start(config: EngineConfig, resolvers: Resolvers) -> anyhow::Result<Self> {
        let engine = Arc::new(EngineHandle::start(config)?);
        let queue = Arc::new(RwLock::new(Queue::new()));
        let (tx, rx) = crossbeam_channel::unbounded();
        let (stop, stop_rx) = crossbeam_channel::bounded(1);
        let (ev_tx, events) = crossbeam_channel::unbounded();

        let mut ctl = Controller {
            engine: engine.clone(),
            queue: queue.clone(),
            resolvers,
            events: ev_tx,
            preloaded: None,
            active: false,
        };
        let thread = thread::Builder::new()
            .name("khoostic-player".into())
            .spawn(move || ctl.run(rx, stop_rx))?;

        Ok(Self {
            tx,
            stop,
            events,
            engine,
            queue,
            thread: Some(thread),
        })
    }

    pub fn send(&self, cmd: PlayerCommand) {
        let _ = self.tx.send(cmd);
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
        let st = self.engine.status();
        let q = self.queue.read();
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

struct Controller {
    engine: Arc<EngineHandle>,
    queue: Arc<RwLock<Queue>>,
    resolvers: Resolvers,
    events: Sender<PlayerEvent>,
    /// Entry currently handed to the engine as the gapless follow-up.
    preloaded: Option<EntryId>,
    /// Whether we intend to be playing something (false after Stop / end of queue).
    active: bool,
}

impl Controller {
    fn run(&mut self, rx: Receiver<PlayerCommand>, stop: Receiver<()>) {
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
            }
        }
    }

    fn emit(&self, ev: PlayerEvent) {
        let _ = self.events.send(ev);
    }

    fn command(&mut self, cmd: PlayerCommand) {
        use PlayerCommand::*;
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
                    let paused = self.engine.status().state == PlaybackState::Paused;
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
                    self.load_current()
                } else {
                    self.stop()
                }
            }
            Prev => {
                let st = self.engine.status();
                if st.token.is_some() && st.position > RESTART_THRESHOLD {
                    self.engine.send(Command::Seek(Duration::ZERO));
                } else if self.queue.write().skip_prev().is_some() {
                    self.load_current();
                }
            }
            Play => {
                if self.engine.status().state == PlaybackState::Stopped {
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
            Pause => self.engine.send(Command::Pause),
            TogglePause => {
                if self.engine.status().state == PlaybackState::Stopped {
                    self.command(Play);
                } else {
                    self.engine.send(Command::TogglePause);
                }
            }
            Stop => self.stop(),
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
        }
    }

    fn engine_event(&mut self, ev: EngineEvent) {
        match ev {
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
            EngineEvent::StateChanged(state) => {
                if state == PlaybackState::Stopped {
                    self.active = false;
                    self.preloaded = None;
                    // Played off the end of the queue: park the cursor after the last entry.
                    let mut q = self.queue.write();
                    if q.peek_next().is_none() && q.current().is_some() {
                        q.advance();
                    }
                    drop(q);
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

    fn stop(&mut self) {
        self.active = false;
        self.preloaded = None;
        self.engine.send(Command::Stop);
    }

    /// Starts the queue's current entry, skipping entries that fail to open.
    fn load_current(&mut self) {
        self.load_current_with(false);
    }

    fn load_current_with(&mut self, paused: bool) {
        let attempts = self.queue.read().len();
        for _ in 0..attempts.max(1) {
            let Some(entry) = self.queue.read().current().cloned() else {
                self.stop();
                return;
            };
            match self.resolvers.open(&entry.track) {
                Ok(source) => {
                    self.preloaded = None;
                    self.active = true;
                    self.engine.load(entry.id.0, source, paused);
                    return;
                }
                Err(e) => {
                    self.emit(PlayerEvent::Error(format!("{}: {e}", entry.track.title)));
                    if self.queue.write().skip_next().is_none() {
                        self.stop();
                        return;
                    }
                }
            }
        }
        self.stop();
    }

    /// Makes sure the engine's gapless follow-up matches what the queue says comes next.
    fn refresh_preload(&mut self) {
        if !self.active {
            return;
        }
        let next = self.queue.read().peek_next().cloned();
        let Some(next) = next else {
            if self.preloaded.take().is_some() {
                self.engine.send(Command::ClearPreload);
            }
            return;
        };
        if self.preloaded == Some(next.id) {
            return;
        }
        // Repeat-one: the "next" entry is the current one; preload a fresh decoder of it.
        match self.resolvers.open(&next.track) {
            Ok(source) => {
                self.preloaded = Some(next.id);
                self.engine.preload(next.id.0, source);
            }
            Err(e) => {
                self.preloaded = None;
                self.engine.send(Command::ClearPreload);
                self.emit(PlayerEvent::Error(format!("{}: {e}", next.track.title)));
            }
        }
    }
}
