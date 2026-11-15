//! The GUI's state and views, built immediate-mode every frame.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use khoostic_audio::{AnalysisFrame, BANDS, PlaybackState};
use khoostic_config::Config;
use khoostic_core::{RepeatMode, Track, TrackLocator};
use khoostic_library::Library;
use khoostic_player::browse::{Browser, EntryKind};
use khoostic_player::{Player, PlayerCommand as Cmd, PlayerSnapshot};

use crate::canvas::{Canvas, Color, Rect, Weight};
use crate::text::TextSystem;
use crate::ui::{Input, Key, Theme, Ui};

const SIDEBAR_W: f32 = 212.0;
const BAR_H: f32 = 92.0;
const ROW_H: f32 = 42.0;
const PAD: f32 = 28.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Queue,
    Browse,
    Visualizer,
}

const VIEWS: [(View, &str); 3] = [
    (View::Queue, "Queue"),
    (View::Browse, "Browse"),
    (View::Visualizer, "Visualizer"),
];

#[derive(Debug, Default, Clone)]
struct ListState {
    selected: usize,
    scroll: f32,
    target: f32,
    /// Visible height of the list, from the last frame.
    height: f32,
}

impl ListState {
    fn clamp(&mut self, len: usize) {
        self.selected = self.selected.min(len.saturating_sub(1));
        let max = (len as f32 * ROW_H - self.height).max(0.0);
        self.target = self.target.clamp(0.0, max);
    }

    fn reveal(&mut self, index: usize) {
        let top = index as f32 * ROW_H;
        if top < self.target {
            self.target = top;
        } else if top + ROW_H > self.target + self.height {
            self.target = top + ROW_H - self.height;
        }
    }

    fn step(&mut self, dt: f32) -> bool {
        let diff = self.target - self.scroll;
        if diff.abs() < 0.5 {
            self.scroll = self.target;
            return false;
        }
        self.scroll += diff * (dt * 18.0).min(1.0);
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Drag {
    Seek(f32),
    Volume,
}

pub struct Gui {
    pub theme: Theme,
    pub library: Arc<Library>,
    pub view: View,
    queue: ListState,
    browse: ListState,
    browser: Browser,
    music_dirs: Vec<PathBuf>,
    drag: Option<Drag>,
    toast: Option<(String, Instant)>,
    last_frame: Instant,
    /// Smoothed level for the visualizer's beat pulse.
    pulse: f32,
}

pub struct FrameResult {
    /// Something is moving; keep rendering at full rate.
    pub animating: bool,
}

impl Gui {
    pub fn new(config: &Config, library: Arc<Library>) -> (Self, Vec<String>) {
        let g = &config.gui;
        let (theme, problems) = Theme::from_config(&g.theme, g.corner_radius, g.font_size);
        let music_dirs = config.library.music_dirs.clone();
        let start = music_dirs
            .first()
            .cloned()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let gui = Self {
            theme,
            library,
            view: View::Queue,
            queue: ListState::default(),
            browse: ListState::default(),
            browser: Browser::new(start),
            music_dirs,
            drag: None,
            toast: None,
            last_frame: Instant::now(),
            pulse: 0.0,
        };
        (gui, problems)
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    pub fn frame(
        &mut self,
        canvas: &mut Canvas,
        text: &mut TextSystem,
        input: &Input,
        player: &Player,
    ) -> FrameResult {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32().min(0.1);
        self.last_frame = now;

        let snap = player.snapshot();
        let analysis = player.analysis();
        self.handle_keys(input, player);
        self.handle_drops(input, player);

        let theme = self.theme.clone();
        let mut ui = Ui {
            c: canvas,
            text,
            input,
            theme: &theme,
        };
        let full = ui.c.bounds();
        let (body, bar) = full.split_bottom(BAR_H);
        let (sidebar, main) = body.split_left(SIDEBAR_W);

        self.sidebar(&mut ui, sidebar, &snap);
        let main = Rect::new(
            main.x + PAD,
            main.y + PAD,
            main.w - 2.0 * PAD,
            main.h - PAD - 12.0,
        );
        match self.view {
            View::Queue => self.queue_view(&mut ui, main, player, &snap, &analysis),
            View::Browse => self.browse_view(&mut ui, main, player),
            View::Visualizer => self.visualizer(&mut ui, main, &analysis, dt),
        }
        self.now_playing(&mut ui, bar, player, &snap, &analysis);
        self.draw_toast(&mut ui, full);

        let scrolling = self.queue.step(dt) | self.browse.step(dt);
        let animating = snap.state == PlaybackState::Playing
            || scrolling
            || self.drag.is_some()
            || self.toast.is_some()
            || analysis.bands.iter().any(|&b| b > 0.0);
        if input.released {
            self.drag = None;
        }
        FrameResult { animating }
    }

    fn handle_drops(&mut self, input: &Input, player: &Player) {
        if input.dropped.is_empty() {
            return;
        }
        let tracks = self.library.tracks_for_paths(&input.dropped);
        if tracks.is_empty() {
            self.toast("nothing playable in what was dropped");
        } else {
            self.toast(format!(
                "added {} track{}",
                tracks.len(),
                plural(tracks.len())
            ));
            player.send(Cmd::Enqueue(tracks));
        }
    }

    fn list_len(&self, player: &Player) -> usize {
        match self.view {
            View::Queue => player.queue().read().len(),
            View::Browse => self.browser.entries().len(),
            View::Visualizer => 0,
        }
    }

    fn list(&mut self) -> &mut ListState {
        match self.view {
            View::Browse => &mut self.browse,
            _ => &mut self.queue,
        }
    }

    fn handle_keys(&mut self, input: &Input, player: &Player) {
        for k in &input.keys {
            let len = self.list_len(player);
            let page = (self.list().height / ROW_H).max(1.0) as usize;
            let mut moved = None;
            let sel = self.list().selected;
            match k.key {
                Key::Space => player.send(Cmd::TogglePause),
                Key::Right => player.send(Cmd::SeekBy(if k.shift { 30_000 } else { 5_000 })),
                Key::Left => player.send(Cmd::SeekBy(if k.shift { -30_000 } else { -5_000 })),
                Key::Down | Key::Char('j') => moved = Some(sel + 1),
                Key::Up | Key::Char('k') => moved = Some(sel.saturating_sub(1)),
                Key::PageDown => moved = Some(sel + page),
                Key::PageUp => moved = Some(sel.saturating_sub(page)),
                Key::Home | Key::Char('g') => moved = Some(0),
                Key::End | Key::Char('G') => moved = Some(len.saturating_sub(1)),
                Key::Enter => self.activate(sel, player),
                Key::Char('l') if self.view == View::Browse => self.activate(sel, player),
                Key::Backspace | Key::Char('h') => self.go_up(),
                Key::Tab => {
                    let i = VIEWS.iter().position(|(v, _)| *v == self.view).unwrap_or(0);
                    let n = if k.shift { VIEWS.len() - 1 } else { 1 };
                    self.view = VIEWS[(i + n) % VIEWS.len()].0;
                }
                Key::Char('1') => self.view = View::Queue,
                Key::Char('2') => self.view = View::Browse,
                Key::Char('3') => self.view = View::Visualizer,
                Key::Char('n') | Key::Char('>') => player.send(Cmd::Next),
                Key::Char('p') | Key::Char('<') => player.send(Cmd::Prev),
                Key::Char('+') | Key::Char('=') => player.send(Cmd::VolumeBy(0.05)),
                Key::Char('-') => player.send(Cmd::VolumeBy(-0.05)),
                Key::Char('s') => player.send(Cmd::ToggleShuffle),
                Key::Char('r') => player.send(Cmd::CycleRepeat),
                Key::Char('o') => {
                    if let Some(i) = player.queue().read().current_index() {
                        self.view = View::Queue;
                        self.queue.selected = i;
                        self.queue.reveal(i);
                    }
                }
                Key::Char('a') if self.view == View::Browse => self.append(sel, player, false),
                Key::Char('A') if self.view == View::Browse => self.append(sel, player, true),
                Key::Delete | Key::Char('x') if self.view == View::Queue => {
                    let id = player.queue().read().entries().get(sel).map(|e| e.id);
                    if let Some(id) = id {
                        player.send(Cmd::Remove(vec![id]));
                    }
                }
                _ => {}
            }
            if let Some(i) = moved {
                let l = self.list();
                l.selected = i.min(len.saturating_sub(1));
                let s = l.selected;
                l.reveal(s);
            }
        }
    }

    fn activate(&mut self, index: usize, player: &Player) {
        match self.view {
            View::Queue => {
                let id = player.queue().read().entries().get(index).map(|e| e.id);
                if let Some(id) = id {
                    player.send(Cmd::PlayEntry(id));
                }
            }
            View::Browse => {
                let Some(e) = self.browser.entries().get(index).cloned() else {
                    return;
                };
                match e.kind {
                    EntryKind::Dir => match self.browser.open(&e.path) {
                        Ok(()) => {
                            self.browse = ListState {
                                height: self.browse.height,
                                ..Default::default()
                            }
                        }
                        Err(err) => self.toast(err),
                    },
                    EntryKind::Audio => {
                        player.send(Cmd::EnqueueAndPlay(vec![Track::from_path(&e.path)]))
                    }
                }
            }
            View::Visualizer => player.send(Cmd::TogglePause),
        }
    }

    fn go_up(&mut self) {
        if self.view == View::Browse
            && let Some(i) = self.browser.parent()
        {
            self.browse.selected = i;
            self.browse.target = (i as f32 * ROW_H - self.browse.height / 2.0).max(0.0);
        }
    }

    fn append(&mut self, index: usize, player: &Player, next: bool) {
        let Some(e) = self.browser.entries().get(index) else {
            return;
        };
        let tracks = self.library.tracks_for_paths(&[&e.path]);
        if tracks.is_empty() {
            return self.toast("nothing playable there");
        }
        self.toast(format!(
            "{} {} track{}",
            if next { "playing next:" } else { "added" },
            tracks.len(),
            plural(tracks.len())
        ));
        player.send(if next {
            Cmd::PlayNext(tracks)
        } else {
            Cmd::Enqueue(tracks)
        });
    }

    fn sidebar(&mut self, ui: &mut Ui, r: Rect, snap: &PlayerSnapshot) {
        let t = ui.theme;
        ui.c.rect(r, t.surface, 0.0);
        ui.label(
            "khoostic",
            r.x + 24.0,
            Rect::new(r.x, r.y + 18.0, r.w, 40.0),
            r.w,
            20.0,
            Weight::Bold,
            t.accent,
        );

        for (i, (view, name)) in VIEWS.iter().enumerate() {
            let row = Rect::new(r.x + 12.0, r.y + 80.0 + i as f32 * 40.0, r.w - 24.0, 36.0);
            let active = self.view == *view;
            let hover = ui.hovered(row);
            if active || hover {
                let bg = if active {
                    t.surface_hover
                } else {
                    t.surface_hover.alpha(0.6)
                };
                ui.c.rect(row, bg, t.radius);
            }
            let color = if active { t.fg } else { t.dim };
            nav_icon(
                ui,
                *view,
                Rect::new(row.x + 12.0, row.y + 10.0, 16.0, 16.0),
                if active { t.accent } else { t.dim },
            );
            ui.label(
                name,
                row.x + 40.0,
                row,
                row.w - 48.0,
                t.font_size,
                Weight::Medium,
                color,
            );
            if ui.clicked(row) {
                self.view = *view;
            }
        }

        let info = format!("{} Hz · Vulkan", snap.sample_rate);
        let foot = Rect::new(r.x + 24.0, r.bottom() - 56.0, r.w - 36.0, 20.0);
        ui.label(
            &snap.device,
            foot.x,
            foot,
            foot.w,
            12.0,
            Weight::Regular,
            t.dim,
        );
        ui.label(
            &info,
            foot.x,
            Rect {
                y: foot.y + 20.0,
                ..foot
            },
            foot.w,
            12.0,
            Weight::Regular,
            t.dim.alpha(0.7),
        );
    }

    fn header(&mut self, ui: &mut Ui, r: Rect, title: &str, subtitle: &str) -> Rect {
        let t = ui.theme;
        ui.label(
            title,
            r.x,
            Rect::new(r.x, r.y, r.w, 36.0),
            r.w * 0.6,
            26.0,
            Weight::Bold,
            t.fg,
        );
        ui.label(
            subtitle,
            r.x,
            Rect::new(r.x, r.y + 36.0, r.w, 22.0),
            r.w * 0.6,
            13.0,
            Weight::Regular,
            t.dim,
        );
        Rect::new(r.x, r.y + 76.0, r.w, (r.h - 76.0).max(0.0))
    }

    fn queue_view(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        player: &Player,
        snap: &PlayerSnapshot,
        a: &AnalysisFrame,
    ) {
        let t = ui.theme.clone();
        let len = player.queue().read().len();
        let subtitle = if len == 0 {
            String::new()
        } else {
            format!("{len} track{}", plural(len))
        };
        let list = self.header(ui, r, "Queue", &subtitle);
        if len > 0 {
            let (clear, _) = ui.pill("Clear", r.right() - 70.0, r.y + 4.0, false);
            if clear {
                player.send(Cmd::Clear);
            }
        }
        if len == 0 {
            empty_state(
                ui,
                list,
                "The queue is empty",
                "Browse your music (2) or drop files onto the window",
            );
            return;
        }

        self.queue.height = list.h;
        self.queue.clamp(len);
        let q = player.queue().read();
        let playing = snap.current.as_ref().map(|e| e.id);
        let (first, rows) = visible_rows(&self.queue, len, list);
        ui.c.push_clip(list);
        let mut clicked = None;
        let cols = QueueCols::new(list.w);
        for i in first..(first + rows).min(len) {
            let e = &q.entries()[i];
            let row = Rect::new(
                list.x,
                list.y + i as f32 * ROW_H - self.queue.scroll,
                list.w,
                ROW_H - 2.0,
            );
            let hover = ui.hovered(row) && ui.hovered(list);
            let is_playing = Some(e.id) == playing;
            if i == self.queue.selected {
                ui.c.rect(row, t.surface_hover, t.radius);
            } else if hover {
                ui.c.rect(row, t.surface_hover.alpha(0.5), t.radius);
            }
            if hover && ui.input.pressed {
                clicked = Some((i, ui.input.double_click));
            }
            let num = Rect::new(row.x, row.y, cols.num, row.h);
            if is_playing {
                eq_icon(ui, num, a, snap.state == PlaybackState::Playing, t.accent);
            } else {
                ui.label_right(
                    &(i + 1).to_string(),
                    num.right() - 8.0,
                    row,
                    12.0,
                    Weight::Regular,
                    t.dim,
                );
            }
            let title_color = if is_playing { t.accent } else { t.fg };
            let x = row.x + cols.num + 12.0;
            ui.label(
                &e.track.title,
                x,
                row,
                cols.title - 16.0,
                t.font_size,
                Weight::Medium,
                title_color,
            );
            let x = x + cols.title;
            ui.label(
                &e.track.artist_line(),
                x,
                row,
                cols.artist - 16.0,
                t.font_size - 1.0,
                Weight::Regular,
                t.dim,
            );
            let x = x + cols.artist;
            ui.label(
                &album_of(&e.track),
                x,
                row,
                cols.album - 16.0,
                t.font_size - 1.0,
                Weight::Regular,
                t.dim,
            );
            let dur = if is_playing {
                snap.duration
            } else {
                e.track.duration
            };
            if let Some(d) = dur {
                ui.label_right(
                    &fmt_time(d),
                    row.right() - 12.0,
                    row,
                    t.font_size - 1.0,
                    Weight::Regular,
                    t.dim,
                );
            }
        }
        ui.c.pop_clip();
        let id_for = |i: usize| q.entries().get(i).map(|e| e.id);
        let action = clicked.map(|(i, dbl)| (i, dbl, id_for(i)));
        drop(q);
        if let Some((i, dbl, id)) = action {
            self.queue.selected = i;
            if let (true, Some(id)) = (dbl, id) {
                player.send(Cmd::PlayEntry(id));
            }
        }
        if ui.hovered(list) {
            self.queue.target += ui.input.scroll;
        }
        scrollbar(ui, list, &self.queue, len);
    }

    fn browse_view(&mut self, ui: &mut Ui, r: Rect, player: &Player) {
        let t = ui.theme.clone();
        let dir = self.browser.dir().display().to_string();
        let name = self
            .browser
            .dir()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.clone());
        let list = self.header(ui, r, &name, &dir);
        let mut x = r.right();
        for (label, act) in [("Add all", 0), ("Up", 1), ("Music", 2)] {
            let w = ui.measure(label, t.font_size - 1.0, Weight::Medium) + 24.0;
            x -= w + 8.0;
            let (clicked, _) = ui.pill(label, x, r.y + 4.0, false);
            if clicked {
                match act {
                    0 => {
                        let tracks = self.library.tracks_for_paths(&[self.browser.dir()]);
                        self.toast(format!(
                            "added {} track{}",
                            tracks.len(),
                            plural(tracks.len())
                        ));
                        player.send(Cmd::Enqueue(tracks));
                    }
                    1 => {
                        let prev = self.view;
                        self.view = View::Browse;
                        self.go_up();
                        self.view = prev;
                    }
                    _ => {
                        if let Some(d) = self.music_dirs.first().cloned()
                            && self.browser.open(&d).is_ok()
                        {
                            self.browse = ListState {
                                height: self.browse.height,
                                ..Default::default()
                            };
                        }
                    }
                }
            }
        }

        let len = self.browser.entries().len();
        if len == 0 {
            let msg = self
                .browser
                .error()
                .unwrap_or("No folders or audio files here")
                .to_owned();
            empty_state(ui, list, &msg, "Backspace goes up a folder");
            return;
        }
        self.browse.height = list.h;
        self.browse.clamp(len);
        let (first, rows) = visible_rows(&self.browse, len, list);
        ui.c.push_clip(list);
        let mut clicked = None;
        for i in first..(first + rows).min(len) {
            let e = &self.browser.entries()[i];
            let row = Rect::new(
                list.x,
                list.y + i as f32 * ROW_H - self.browse.scroll,
                list.w,
                ROW_H - 2.0,
            );
            let hover = ui.hovered(row) && ui.hovered(list);
            if i == self.browse.selected {
                ui.c.rect(row, t.surface_hover, t.radius);
            } else if hover {
                ui.c.rect(row, t.surface_hover.alpha(0.5), t.radius);
            }
            let icon = Rect::new(row.x + 14.0, row.y + (row.h - 16.0) / 2.0, 18.0, 16.0);
            match e.kind {
                EntryKind::Dir => {
                    ui.c.rect(
                        Rect::new(icon.x, icon.y, 9.0, 4.0),
                        t.accent.alpha(0.8),
                        1.5,
                    );
                    ui.c.rect(
                        Rect::new(icon.x, icon.y + 2.5, icon.w, icon.h - 2.5),
                        t.accent.alpha(0.8),
                        3.0,
                    );
                }
                EntryKind::Audio => {
                    ui.c.circle(icon.x + 6.0, icon.y + 11.0, 4.5, t.dim);
                    ui.c.rect(Rect::new(icon.x + 9.0, icon.y, 2.0, 11.0), t.dim, 1.0);
                }
            }
            let color = if e.kind == EntryKind::Dir {
                t.fg
            } else {
                t.fg.alpha(0.85)
            };
            ui.label(
                &e.name,
                row.x + 46.0,
                row,
                row.w - 60.0,
                t.font_size,
                Weight::Medium,
                color,
            );
            if hover && ui.input.pressed {
                clicked = Some((i, ui.input.double_click, false));
            }
            if hover && ui.input.right_pressed {
                clicked = Some((i, false, true));
            }
        }
        ui.c.pop_clip();
        if let Some((i, dbl, right)) = clicked {
            self.browse.selected = i;
            if right {
                self.append(i, player, false);
            } else if dbl {
                self.activate(i, player);
            }
        }
        if ui.hovered(list) {
            self.browse.target += ui.input.scroll;
        }
        scrollbar(ui, list, &self.browse, len);
    }

    fn visualizer(&mut self, ui: &mut Ui, r: Rect, a: &AnalysisFrame, dt: f32) {
        let t = ui.theme.clone();
        self.pulse = (self.pulse - dt * 2.5).max(a.beat);
        let (cx, cy) = r.center();
        // Beat glow behind the bars.
        let glow = r.w.min(r.h) * (0.25 + 0.12 * self.pulse);
        ui.c.glow(
            Rect::new(cx - glow, cy - glow, glow * 2.0, glow * 2.0),
            t.gradient(0.6).alpha(0.10 + 0.25 * self.pulse),
            glow,
            glow * 0.6,
        );
        let gap = 4.0;
        let bar_w = ((r.w - gap * (BANDS as f32 - 1.0)) / BANDS as f32).max(1.0);
        let base = r.bottom() - 24.0;
        let max_h = r.h - 48.0;
        for i in 0..BANDS {
            let v = a.bands[i];
            let x = r.x + i as f32 * (bar_w + gap);
            let h = (v * max_h).max(bar_w);
            let top = t.gradient(v);
            let bottom = t.gradient(0.0).alpha(0.5);
            ui.c.gradient(Rect::new(x, base - h, bar_w, h), top, bottom, bar_w / 2.0);
            let peak = base - a.peaks[i] * max_h - 6.0;
            if a.peaks[i] > 0.02 {
                ui.c.rect(Rect::new(x, peak, bar_w, 3.0), top.alpha(0.8), 1.5);
            }
        }
        ui.label(
            "Spectrum preview · GPU particle visualizers come next",
            r.x,
            Rect::new(r.x, r.y, r.w, 20.0),
            r.w,
            12.0,
            Weight::Regular,
            t.dim,
        );
    }

    fn now_playing(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        player: &Player,
        snap: &PlayerSnapshot,
        a: &AnalysisFrame,
    ) {
        let t = ui.theme.clone();
        ui.c.rect(r, t.surface, 0.0);
        ui.c.rect(Rect::new(r.x, r.y, r.w, 1.0), t.background, 0.0);

        // Left: art placeholder + title/artist.
        let art = Rect::new(r.x + 18.0, r.y + (r.h - 58.0) / 2.0, 58.0, 58.0);
        let glow = 0.25 + 0.5 * a.bass;
        ui.c.gradient(
            art,
            t.gradient(0.0).alpha(0.35 + glow * 0.3),
            t.gradient(1.0).alpha(0.25 + glow * 0.3),
            t.radius,
        );
        let mid = Rect::new(art.x + 22.0, art.y + 20.0, 14.0, 18.0);
        ui.c.circle(mid.x + 5.0, mid.bottom() - 4.0, 5.0, t.fg.alpha(0.7));
        ui.c.rect(
            Rect::new(mid.x + 8.0, mid.y, 2.5, 14.0),
            t.fg.alpha(0.7),
            1.0,
        );

        let info_w = (r.w * 0.26).min(320.0);
        let tx = art.right() + 14.0;
        match &snap.current {
            Some(e) => {
                ui.label(
                    &e.track.title,
                    tx,
                    Rect::new(tx, r.y + 22.0, info_w, 24.0),
                    info_w,
                    t.font_size + 1.0,
                    Weight::Bold,
                    t.fg,
                );
                let sub = match e.track.artist_line() {
                    s if s.is_empty() => album_of(&e.track),
                    s => s,
                };
                ui.label(
                    &sub,
                    tx,
                    Rect::new(tx, r.y + 46.0, info_w, 22.0),
                    info_w,
                    t.font_size - 1.0,
                    Weight::Regular,
                    t.dim,
                );
            }
            None => ui.label(
                "Nothing playing",
                tx,
                Rect::new(tx, r.y, info_w, r.h),
                info_w,
                t.font_size,
                Weight::Medium,
                t.dim,
            ),
        }

        // Center: transport + seek bar.
        let center_w = (r.w - 2.0 * (info_w + 120.0)).clamp(240.0, 620.0);
        let cx = r.x + r.w / 2.0;
        let play = Rect::new(cx - 19.0, r.y + 12.0, 38.0, 38.0);
        let play_hover = ui.hovered(play);
        ui.c.circle(
            cx,
            play.y + 19.0,
            19.0,
            if play_hover { t.fg } else { t.fg.alpha(0.92) },
        );
        if snap.state == PlaybackState::Playing {
            ui.c.rect(
                Rect::new(cx - 6.0, play.y + 12.0, 4.0, 14.0),
                t.surface,
                1.5,
            );
            ui.c.rect(
                Rect::new(cx + 2.0, play.y + 12.0, 4.0, 14.0),
                t.surface,
                1.5,
            );
        } else {
            ui.c.triangle(
                Rect::new(cx - 5.0, play.y + 11.0, 14.0, 16.0),
                t.surface,
                true,
                1.5,
            );
        }
        if ui.clicked(play) {
            player.send(Cmd::TogglePause);
        }
        for (dir, dx) in [(false, -52.0), (true, 52.0)] {
            let b = Rect::new(cx + dx - 14.0, play.y + 5.0, 28.0, 28.0);
            let hover = ui.hovered(b);
            let col = if hover { t.fg } else { t.dim };
            let (tri, bar) = if dir {
                (
                    Rect::new(b.x + 7.0, b.y + 8.0, 11.0, 12.0),
                    Rect::new(b.x + 18.0, b.y + 8.0, 2.5, 12.0),
                )
            } else {
                (
                    Rect::new(b.x + 10.0, b.y + 8.0, 11.0, 12.0),
                    Rect::new(b.x + 7.5, b.y + 8.0, 2.5, 12.0),
                )
            };
            ui.c.triangle(tri, col, dir, 1.0);
            ui.c.rect(bar, col, 1.0);
            if ui.clicked(b) {
                player.send(if dir { Cmd::Next } else { Cmd::Prev });
            }
        }

        let seek = Rect::new(
            cx - center_w / 2.0 + 44.0,
            r.y + 62.0,
            center_w - 88.0,
            16.0,
        );
        let dur = snap.duration.unwrap_or_default();
        let frac_at = |x: f32| ((x - seek.x) / seek.w).clamp(0.0, 1.0);
        if ui.clicked(seek) && !dur.is_zero() {
            self.drag = Some(Drag::Seek(frac_at(ui.input.mouse.unwrap_or_default().0)));
        }
        if let (Some(Drag::Seek(_)), Some((mx, _))) = (self.drag, ui.input.mouse) {
            self.drag = Some(Drag::Seek(frac_at(mx)));
            if ui.input.released {
                player.send(Cmd::Seek(dur.mul_f32(frac_at(mx))));
            }
        }
        let frac = match self.drag {
            Some(Drag::Seek(f)) => f,
            _ if dur.is_zero() => 0.0,
            _ => (snap.position.as_secs_f32() / dur.as_secs_f32()).clamp(0.0, 1.0),
        };
        let shown_pos = match self.drag {
            Some(Drag::Seek(f)) => dur.mul_f32(f),
            _ => snap.position,
        };
        let track = Rect::new(seek.x, seek.y + 6.0, seek.w, 4.0);
        ui.c.rect(track, t.dim.alpha(0.3), 2.0);
        ui.c.rect(
            Rect {
                w: track.w * frac,
                ..track
            },
            t.accent,
            2.0,
        );
        if ui.hovered(seek) || matches!(self.drag, Some(Drag::Seek(_))) {
            ui.c.circle(track.x + track.w * frac, track.y + 2.0, 6.0, t.fg);
        }
        let times = Rect::new(seek.x - 50.0, seek.y, 42.0, seek.h);
        ui.label_right(
            &fmt_time(shown_pos),
            times.right(),
            times,
            11.0,
            Weight::Regular,
            t.dim,
        );
        let end = if dur.is_zero() {
            "--:--".to_string()
        } else {
            fmt_time(dur)
        };
        ui.label(
            &end,
            seek.right() + 10.0,
            seek,
            50.0,
            11.0,
            Weight::Regular,
            t.dim,
        );

        // Right: shuffle, repeat, volume.
        let vol = Rect::new(r.right() - 140.0, r.y + (r.h - 16.0) / 2.0, 110.0, 16.0);
        let vtrack = Rect::new(vol.x, vol.y + 6.0, vol.w, 4.0);
        if ui.clicked(vol) {
            self.drag = Some(Drag::Volume);
        }
        if let (Some(Drag::Volume), Some((mx, _))) = (self.drag, ui.input.mouse) {
            player.send(Cmd::SetVolume(((mx - vol.x) / vol.w).clamp(0.0, 1.0)));
        }
        if ui.hovered(vol) && ui.input.scroll != 0.0 {
            player.send(Cmd::VolumeBy(-ui.input.scroll.signum() * 0.05));
        }
        ui.c.rect(vtrack, t.dim.alpha(0.3), 2.0);
        ui.c.rect(
            Rect {
                w: vtrack.w * snap.volume,
                ..vtrack
            },
            t.fg.alpha(0.8),
            2.0,
        );
        if ui.hovered(vol) || self.drag == Some(Drag::Volume) {
            ui.c.circle(vtrack.x + vtrack.w * snap.volume, vtrack.y + 2.0, 6.0, t.fg);
        }
        speaker_icon(
            ui,
            Rect::new(vol.x - 28.0, vol.y, 16.0, 16.0),
            snap.volume,
            t.dim,
        );

        let mut x = vol.x - 40.0;
        let repeat = match snap.repeat {
            RepeatMode::Off => "Repeat",
            RepeatMode::All => "Repeat all",
            RepeatMode::One => "Repeat one",
        };
        for (label, active, cmd) in [
            (repeat, snap.repeat != RepeatMode::Off, Cmd::CycleRepeat),
            ("Shuffle", snap.shuffle, Cmd::ToggleShuffle),
        ] {
            let w = ui.measure(label, t.font_size - 1.0, Weight::Medium) + 24.0;
            x -= w + 6.0;
            let (clicked, _) = ui.pill(label, x, r.y + (r.h - 30.0) / 2.0, active);
            if clicked {
                player.send(cmd);
            }
        }
    }

    fn draw_toast(&mut self, ui: &mut Ui, full: Rect) {
        let Some((msg, at)) = &self.toast else { return };
        let age = at.elapsed().as_secs_f32();
        if age > 3.0 {
            self.toast = None;
            return;
        }
        let alpha = (3.0 - age).min(1.0) * (age * 6.0).min(1.0);
        let t = ui.theme;
        let w = ui.measure(msg, t.font_size, Weight::Medium) + 36.0;
        let r = Rect::new((full.w - w) / 2.0, full.h - BAR_H - 64.0, w, 40.0);
        ui.c.set_layer(1);
        ui.c.glow(r, Color::BLACK.alpha(0.5 * alpha), 20.0, 14.0);
        ui.c.rect(r, t.surface_hover.alpha(alpha), 20.0);
        let msg = msg.clone();
        ui.label_center(&msg, r, t.font_size, Weight::Medium, t.fg.alpha(alpha));
        ui.c.set_layer(0);
    }
}

struct QueueCols {
    num: f32,
    title: f32,
    artist: f32,
    album: f32,
}

impl QueueCols {
    fn new(w: f32) -> Self {
        let num = 40.0;
        let flex = (w - num - 12.0 - 70.0).max(0.0);
        Self {
            num,
            title: flex * 0.42,
            artist: flex * 0.28,
            album: flex * 0.30,
        }
    }
}

fn visible_rows(state: &ListState, len: usize, list: Rect) -> (usize, usize) {
    let first = (state.scroll / ROW_H).floor().max(0.0) as usize;
    let rows = (list.h / ROW_H).ceil() as usize + 2;
    (first.min(len), rows)
}

fn scrollbar(ui: &mut Ui, list: Rect, state: &ListState, len: usize) {
    let content = len as f32 * ROW_H;
    if content <= list.h {
        return;
    }
    let h = (list.h * list.h / content).max(24.0);
    let y = list.y + (state.scroll / (content - list.h)) * (list.h - h);
    ui.c.rect(
        Rect::new(list.right() + 8.0, y, 4.0, h),
        ui.theme.dim.alpha(0.35),
        2.0,
    );
}

fn empty_state(ui: &mut Ui, r: Rect, title: &str, hint: &str) {
    let t = ui.theme;
    let mid = Rect::new(r.x, r.y + r.h * 0.3, r.w, 30.0);
    ui.label_center(title, mid, 18.0, Weight::Medium, t.fg.alpha(0.8));
    ui.label_center(
        hint,
        Rect {
            y: mid.y + 32.0,
            ..mid
        },
        13.0,
        Weight::Regular,
        t.dim,
    );
}

fn nav_icon(ui: &mut Ui, view: View, r: Rect, color: Color) {
    match view {
        View::Queue => {
            for i in 0..3 {
                let w = if i == 2 { 10.0 } else { 16.0 };
                ui.c.rect(
                    Rect::new(r.x, r.y + 2.0 + i as f32 * 5.0, w, 2.5),
                    color,
                    1.25,
                );
            }
        }
        View::Browse => {
            ui.c.rect(Rect::new(r.x, r.y + 1.0, 7.0, 4.0), color, 1.5);
            ui.c.rect(Rect::new(r.x, r.y + 3.0, 16.0, 12.0), color, 3.0);
        }
        View::Visualizer => {
            for (i, h) in [8.0, 14.0, 5.0, 11.0].iter().enumerate() {
                ui.c.rect(
                    Rect::new(r.x + i as f32 * 4.5, r.bottom() - h, 2.5, *h),
                    color,
                    1.25,
                );
            }
        }
    }
}

/// Little animated equalizer shown next to the playing track.
fn eq_icon(ui: &mut Ui, r: Rect, a: &AnalysisFrame, playing: bool, color: Color) {
    let levels = if playing {
        [a.bass, a.mid, a.treble]
    } else {
        [0.15, 0.15, 0.15]
    };
    let base = r.y + r.h / 2.0 + 7.0;
    for (i, l) in levels.iter().enumerate() {
        let h = 3.0 + 11.0 * l.clamp(0.0, 1.0);
        ui.c.rect(
            Rect::new(r.right() - 22.0 + i as f32 * 5.0, base - h, 3.0, h),
            color,
            1.5,
        );
    }
}

fn speaker_icon(ui: &mut Ui, r: Rect, volume: f32, color: Color) {
    ui.c.rect(Rect::new(r.x, r.y + 5.0, 4.0, 6.0), color, 1.0);
    ui.c.triangle(
        Rect::new(r.x + 1.0, r.y + 2.0, 7.0, 12.0),
        color,
        false,
        0.5,
    );
    // Sound waves as short bars, lit according to the volume.
    for (i, th) in [0.05f32, 0.5].iter().enumerate() {
        let h = 6.0 + i as f32 * 5.0;
        let c = if volume > *th {
            color
        } else {
            color.alpha(0.25)
        };
        ui.c.rect(
            Rect::new(r.x + 10.0 + i as f32 * 3.5, r.y + 8.0 - h / 2.0, 2.0, h),
            c,
            1.0,
        );
    }
}

fn album_of(track: &Track) -> String {
    if let Some(a) = &track.album {
        return a.clone();
    }
    match &track.locator {
        TrackLocator::Local(p) => p
            .parent()
            .and_then(|d| d.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

pub fn fmt_time(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
