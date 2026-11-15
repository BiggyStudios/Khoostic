//! The playback engine.
//!
//! One engine thread owns the decoders and the output device. Callers hand it already-opened
//! [`AudioSource`]s tagged with an opaque [`Token`]; the engine reports back with events.
//!
//! ## Gapless playback and exact positions
//!
//! Samples are pushed into the output ring as one continuous stream. The engine records a
//! [`Segment`] per track: the absolute frame index at which it starts (and, once decoded, ends)
//! in that stream. The audio callback counts every frame it takes out of the ring, so comparing
//! the two tells us exactly which track is audible and where in it we are — no matter how far
//! ahead the decoder has run. When a preloaded track is available at end-of-stream, its samples
//! simply follow the previous track's in the ring: zero gap, no device restart.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use crate::analysis::{self, AnalysisFrame};
use crate::condition::{CHANNELS, Conditioner};
use crate::output::{self, Output};
use crate::source::{AudioSource, ReadStatus};

/// Opaque identifier the caller attaches to a source (the player uses queue entry ids).
pub type Token = u64;

pub enum Command {
    /// Stop whatever is playing and start `source` immediately.
    Load {
        token: Token,
        source: Box<dyn AudioSource>,
        paused: bool,
    },
    /// Queue `source` to follow the current track gaplessly. Replaces any previous preload.
    Preload {
        token: Token,
        source: Box<dyn AudioSource>,
    },
    ClearPreload,
    Play,
    Pause,
    TogglePause,
    Stop,
    Seek(Duration),
    /// Perceptual volume in `0..=1`.
    SetVolume(f32),
    Shutdown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlaybackState {
    #[default]
    Stopped,
    Playing,
    Paused,
}

#[derive(Clone, Debug)]
pub enum EngineEvent {
    /// The first sample of this track just reached the device.
    TrackStarted {
        token: Token,
        duration: Option<Duration>,
    },
    /// The last sample of this track was played.
    TrackFinished {
        token: Token,
    },
    /// The decoder reached the end of this track and nothing is preloaded. Preload now to keep
    /// playback gapless (there is roughly one ring buffer of audio left).
    AboutToFinish {
        token: Token,
    },
    StateChanged(PlaybackState),
    Seeked {
        token: Token,
        position: Duration,
    },
    SourceError {
        token: Token,
        message: String,
    },
    OutputError(String),
}

#[derive(Clone, Debug, Default)]
pub struct EngineStatus {
    pub state: PlaybackState,
    pub token: Option<Token>,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub volume: f32,
    /// Audio decoded but not yet played.
    pub buffered: Duration,
    pub sample_rate: u32,
    pub device: String,
}

#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// Case-insensitive substring of the output device name. `None` = system default.
    pub device: Option<String>,
    /// How far ahead the decoder runs. Larger survives slower sources; seeking and skipping
    /// are unaffected because they flush.
    pub buffer: Duration,
    pub volume: f32,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            device: None,
            buffer: Duration::from_millis(1000),
            volume: 0.8,
        }
    }
}

pub struct EngineHandle {
    tx: Sender<Command>,
    events: Receiver<EngineEvent>,
    status: Arc<ArcSwap<EngineStatus>>,
    analysis: Arc<ArcSwap<AnalysisFrame>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl EngineHandle {
    pub fn start(config: EngineConfig) -> anyhow::Result<Self> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (ev_tx, events) = crossbeam_channel::unbounded();
        let status = Arc::new(ArcSwap::from_pointee(EngineStatus::default()));
        let analysis = Arc::new(ArcSwap::from_pointee(AnalysisFrame::default()));
        let (ready_tx, ready_rx) = crossbeam_channel::bounded(1);

        let (st, an) = (status.clone(), analysis.clone());
        let thread = thread::Builder::new()
            .name("khoostic-engine".into())
            .spawn(move || {
                // The stream is created on this thread: cpal streams aren't Send everywhere.
                let opened =
                    match output::open(config.device.as_deref(), config.buffer, ev_tx.clone()) {
                        Ok(o) => o,
                        Err(e) => {
                            let _ = ready_tx.send(Err(e));
                            return;
                        }
                    };
                let stop = Arc::new(AtomicBool::new(false));
                let analysis_thread =
                    analysis::spawn(opened.analysis, opened.output.sample_rate, an, stop.clone());
                let _ = ready_tx.send(Ok(()));
                let mut engine = Engine::new(opened.output, opened.producer, rx, ev_tx, st);
                engine.set_volume(config.volume);
                engine.run();
                stop.store(true, Ordering::Relaxed);
                let _ = analysis_thread.join();
            })?;

        ready_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("engine thread died during startup"))??;
        Ok(Self {
            tx,
            events,
            status,
            analysis,
            thread: Some(thread),
        })
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }

    pub fn load(&self, token: Token, source: Box<dyn AudioSource>, paused: bool) {
        self.send(Command::Load {
            token,
            source,
            paused,
        });
    }

    pub fn preload(&self, token: Token, source: Box<dyn AudioSource>) {
        self.send(Command::Preload { token, source });
    }

    pub fn events(&self) -> &Receiver<EngineEvent> {
        &self.events
    }

    /// Latest engine status (cheap; refreshed ~60 times per second while playing).
    pub fn status(&self) -> Arc<EngineStatus> {
        self.status.load_full()
    }

    /// Latest analysis frame for visualizers.
    pub fn analysis(&self) -> Arc<AnalysisFrame> {
        self.analysis.load_full()
    }

    /// Shared handle to the analysis feed, for render threads that outlive borrows.
    pub fn analysis_feed(&self) -> Arc<ArcSwap<AnalysisFrame>> {
        self.analysis.clone()
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Active {
    token: Token,
    source: Box<dyn AudioSource>,
    cond: Conditioner,
}

impl Active {
    fn new(token: Token, source: Box<dyn AudioSource>, out_rate: u32) -> Self {
        let cond = Conditioner::new(source.spec().sample_rate, out_rate);
        Self {
            token,
            source,
            cond,
        }
    }

    fn duration(&self) -> Option<Duration> {
        self.source.duration()
    }
}

#[derive(Debug)]
struct Segment {
    token: Token,
    /// Absolute frame in the pushed stream where this track starts.
    start: u64,
    /// Absolute frame where it ends; known once fully decoded.
    end: Option<u64>,
    /// Track position corresponding to `start` (non-zero after a seek).
    offset: Duration,
    duration: Option<Duration>,
    started: bool,
}

const FLUSH_TIMEOUT: Duration = Duration::from_millis(300);
const STATUS_INTERVAL: Duration = Duration::from_millis(16);
/// Max source reads per loop iteration, so commands stay responsive while filling.
const DECODE_BUDGET: usize = 32;

struct Engine {
    output: Output,
    producer: rtrb::Producer<f32>,
    rx: Receiver<Command>,
    events: Sender<EngineEvent>,
    status: Arc<ArcSwap<EngineStatus>>,

    decoding: Option<Active>,
    preload: Option<Active>,
    /// Fully decoded but still (partly) in the ring; kept so a seek can still reach it.
    retired: Option<Active>,
    segments: VecDeque<Segment>,

    pending: Vec<f32>,
    pending_pos: usize,
    read_buf: Vec<f32>,
    pushed: u64,

    state: PlaybackState,
    volume: f32,
    last_status: Instant,
}

impl Engine {
    fn new(
        output: Output,
        producer: rtrb::Producer<f32>,
        rx: Receiver<Command>,
        events: Sender<EngineEvent>,
        status: Arc<ArcSwap<EngineStatus>>,
    ) -> Self {
        Self {
            output,
            producer,
            rx,
            events,
            status,
            decoding: None,
            preload: None,
            retired: None,
            segments: VecDeque::new(),
            pending: Vec::with_capacity(32 * 1024),
            pending_pos: 0,
            read_buf: Vec::with_capacity(32 * 1024),
            pushed: 0,
            state: PlaybackState::Stopped,
            volume: 1.0,
            last_status: Instant::now() - STATUS_INTERVAL,
        }
    }

    fn emit(&self, ev: EngineEvent) {
        let _ = self.events.send(ev);
    }

    fn run(&mut self) {
        loop {
            let busy = self.has_work() && self.producer.slots() >= 1024 * CHANNELS;
            let timeout = match (busy, self.state) {
                (true, _) => Duration::ZERO,
                (false, PlaybackState::Stopped) if self.segments.is_empty() => {
                    Duration::from_millis(100)
                }
                _ => Duration::from_millis(4),
            };
            match self.rx.recv_timeout(timeout) {
                Ok(cmd) => {
                    if !self.handle(cmd) {
                        return;
                    }
                    while let Ok(cmd) = self.rx.try_recv() {
                        if !self.handle(cmd) {
                            return;
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            self.fill();
            self.track_boundaries();
            self.publish_status(false);
        }
    }

    fn has_work(&self) -> bool {
        self.pending_pos < self.pending.len() || self.decoding.is_some()
    }

    /// Returns `false` on shutdown.
    fn handle(&mut self, cmd: Command) -> bool {
        match cmd {
            Command::Load {
                token,
                source,
                paused,
            } => {
                self.clear_all();
                let active = Active::new(token, source, self.output.sample_rate);
                self.start_segment(&active, self.pushed, Duration::ZERO);
                self.decoding = Some(active);
                self.set_state(if paused {
                    PlaybackState::Paused
                } else {
                    PlaybackState::Playing
                });
            }
            Command::Preload { token, source } => {
                let active = Active::new(token, source, self.output.sample_rate);
                if self.decoding.is_none() {
                    // Already past end-of-stream of the current track (or idle): continue
                    // straight into this one.
                    self.start_segment(&active, self.pushed, Duration::ZERO);
                    self.decoding = Some(active);
                    if self.state == PlaybackState::Stopped {
                        self.set_state(PlaybackState::Playing);
                    }
                } else {
                    self.preload = Some(active);
                }
            }
            Command::ClearPreload => self.preload = None,
            Command::Play => {
                if self.state == PlaybackState::Paused {
                    self.set_state(PlaybackState::Playing);
                }
            }
            Command::Pause => {
                if self.state == PlaybackState::Playing {
                    self.set_state(PlaybackState::Paused);
                }
            }
            Command::TogglePause => match self.state {
                PlaybackState::Playing => self.set_state(PlaybackState::Paused),
                PlaybackState::Paused => self.set_state(PlaybackState::Playing),
                PlaybackState::Stopped => {}
            },
            Command::Stop => {
                self.clear_all();
                self.set_state(PlaybackState::Stopped);
            }
            Command::Seek(to) => self.seek(to),
            Command::SetVolume(v) => self.set_volume(v),
            Command::Shutdown => return false,
        }
        self.publish_status(true);
        true
    }

    fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        // Cubic taper: perceptually even steps across the whole slider.
        self.output.shared.set_gain(self.volume.powi(3));
    }

    fn set_state(&mut self, state: PlaybackState) {
        self.output
            .shared
            .paused
            .store(state != PlaybackState::Playing, Ordering::Relaxed);
        if self.state != state {
            self.state = state;
            self.emit(EngineEvent::StateChanged(state));
        }
    }

    fn start_segment(&mut self, active: &Active, start: u64, offset: Duration) {
        self.segments.push_back(Segment {
            token: active.token,
            start,
            end: None,
            offset,
            duration: active.duration(),
            started: false,
        });
    }

    /// Drops all sources and queued audio. Afterwards `pushed == consumed`.
    fn clear_all(&mut self) {
        self.decoding = None;
        self.preload = None;
        self.retired = None;
        self.segments.clear();
        self.discard_queued_audio();
    }

    fn discard_queued_audio(&mut self) {
        self.pending.clear();
        self.pending_pos = 0;
        // On success the callback has counted every discarded frame as consumed, so this
        // resynchronizes exactly. On a stalled device it's the best guess available.
        self.output.flush(FLUSH_TIMEOUT);
        self.pushed = self.output.shared.consumed.load(Ordering::Acquire);
    }

    fn audible(&self) -> Option<&Segment> {
        self.segments.front()
    }

    fn seek(&mut self, to: Duration) {
        let Some(audible) = self.audible().map(|s| s.token) else {
            return;
        };

        // If the audible track already finished decoding, the decoder has moved on to the next
        // one. Rewind that decoder and push it back into the preload slot.
        if self.retired.as_ref().is_some_and(|r| r.token == audible) {
            if let Some(mut next) = self.decoding.take()
                && next.source.seek(Duration::ZERO).is_ok()
            {
                next.cond.reset();
                self.preload = Some(next);
            }
            self.decoding = self.retired.take();
        }
        let Some(active) = self.decoding.as_mut().filter(|a| a.token == audible) else {
            return;
        };

        match active.source.seek(to) {
            Ok(reached) => {
                active.cond.reset();
                let duration = active.duration();
                self.discard_queued_audio();
                self.segments.clear();
                self.segments.push_back(Segment {
                    token: audible,
                    start: self.pushed,
                    end: None,
                    offset: reached,
                    duration,
                    started: true,
                });
                self.emit(EngineEvent::Seeked {
                    token: audible,
                    position: reached,
                });
            }
            Err(e) => self.emit(EngineEvent::SourceError {
                token: audible,
                message: e.to_string(),
            }),
        }
    }

    /// Decodes and pushes audio until the ring is full or the budget is spent.
    fn fill(&mut self) {
        for _ in 0..DECODE_BUDGET {
            if self.pending_pos < self.pending.len() {
                let avail = (self.pending.len() - self.pending_pos).min(self.producer.slots()) & !1;
                if avail == 0 {
                    return;
                }
                let slice = &self.pending[self.pending_pos..self.pending_pos + avail];
                let (pushed, _) = self.producer.push_partial_slice(slice);
                self.pending_pos += pushed.len();
                self.pushed += (pushed.len() / CHANNELS) as u64;
                if self.pending_pos < self.pending.len() {
                    return; // ring full
                }
                self.pending.clear();
                self.pending_pos = 0;
            }

            let Some(active) = self.decoding.as_mut() else {
                return;
            };
            self.read_buf.clear();
            let status = match active.source.read(&mut self.read_buf) {
                Ok(s) => s,
                Err(e) => {
                    let ev = EngineEvent::SourceError {
                        token: active.token,
                        message: e.to_string(),
                    };
                    let _ = self.events.send(ev);
                    ReadStatus::End
                }
            };
            match status {
                ReadStatus::Data => {
                    let spec = active.source.spec();
                    if spec.sample_rate != active.cond.input_rate() {
                        // Chained stream changed rate: drain the old resampler, start a new one.
                        active.cond.finish(&mut self.pending);
                        active.cond = Conditioner::new(spec.sample_rate, self.output.sample_rate);
                    }
                    active
                        .cond
                        .process(&self.read_buf, spec.channels, &mut self.pending);
                }
                ReadStatus::End => self.end_of_stream(),
            }
        }
    }

    fn end_of_stream(&mut self) {
        let Some(mut done) = self.decoding.take() else {
            return;
        };
        done.cond.finish(&mut self.pending);
        let end = self.pushed + ((self.pending.len() - self.pending_pos) / CHANNELS) as u64;
        if let Some(seg) = self
            .segments
            .iter_mut()
            .rev()
            .find(|s| s.token == done.token)
        {
            seg.end = Some(end);
        }
        let token = done.token;
        self.retired = Some(done);
        match self.preload.take() {
            Some(next) => {
                self.start_segment(&next, end, Duration::ZERO);
                self.decoding = Some(next);
            }
            None => self.emit(EngineEvent::AboutToFinish { token }),
        }
    }

    fn track_boundaries(&mut self) {
        let consumed = self.output.shared.consumed.load(Ordering::Acquire);
        while let Some(front) = self.segments.front_mut() {
            // `>`: the device must have actually taken at least one frame of this track.
            if !front.started && consumed > front.start && front.end.is_none_or(|e| e > front.start)
            {
                front.started = true;
                let ev = EngineEvent::TrackStarted {
                    token: front.token,
                    duration: front.duration,
                };
                self.emit(ev);
            }
            let front = self.segments.front().expect("checked");
            match front.end {
                Some(end) if consumed >= end => {
                    let token = front.token;
                    if !front.started {
                        // Zero-length or skipped-over track: still report it consistently.
                        self.emit(EngineEvent::TrackStarted {
                            token,
                            duration: front.duration,
                        });
                    }
                    self.segments.pop_front();
                    if self.retired.as_ref().is_some_and(|r| r.token == token) {
                        self.retired = None;
                    }
                    self.emit(EngineEvent::TrackFinished { token });
                }
                _ => break,
            }
        }
        if self.segments.is_empty()
            && self.decoding.is_none()
            && self.state != PlaybackState::Stopped
        {
            self.set_state(PlaybackState::Stopped);
            self.publish_status(true);
        }
    }

    fn publish_status(&mut self, force: bool) {
        if !force && self.last_status.elapsed() < STATUS_INTERVAL {
            return;
        }
        self.last_status = Instant::now();
        let rate = self.output.sample_rate;
        let consumed = self.output.shared.consumed.load(Ordering::Acquire);
        let frames_to_dur = |f: u64| Duration::from_secs_f64(f as f64 / rate as f64);
        let (token, position, duration) = match self.audible() {
            Some(seg) => {
                let played = frames_to_dur(consumed.saturating_sub(seg.start));
                let mut pos = seg.offset + played;
                if let Some(d) = seg.duration {
                    pos = pos.min(d);
                }
                (Some(seg.token), pos, seg.duration)
            }
            None => (None, Duration::ZERO, None),
        };
        self.status.store(Arc::new(EngineStatus {
            state: self.state,
            token,
            position,
            duration,
            volume: self.volume,
            buffered: frames_to_dur(self.pushed.saturating_sub(consumed)),
            sample_rate: rate,
            device: self.output.device_name.clone(),
        }));
    }
}
