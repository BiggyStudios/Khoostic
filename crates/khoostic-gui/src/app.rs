//! The GUI's state, navigation, chrome (sidebar, now-playing bar) and input handling.
//! Content pages live in `views.rs`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use khoostic_audio::{AnalysisFrame, PlaybackState};
use khoostic_config::Config;
use khoostic_core::{RepeatMode, Track, TrackLocator};
use khoostic_library::{Album, Artist, CoverSize, Library, TrackOrder};
use khoostic_player::browse::Browser;
use khoostic_player::{Player, PlayerCommand as Cmd, PlayerSnapshot};

use crate::canvas::{Canvas, Color, Rect, Weight};
use crate::images::ImageStore;
use crate::text::TextSystem;
use crate::ui::{Input, Key, TextField, Theme, Ui};

pub(crate) const SIDEBAR_W: f32 = 224.0;
pub(crate) const BAR_H: f32 = 92.0;
pub(crate) const ROW_H: f32 = 46.0;
pub(crate) const PAD: f32 = 32.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    Albums,
    Album {
        name: String,
        artist: Option<String>,
    },
    Artists,
    Artist(String),
    Songs,
    Search,
    Queue,
    Files,
    Visualizer,
}

impl View {
    /// The sidebar entry this view belongs to.
    fn section(&self) -> View {
        match self {
            View::Album { .. } => View::Albums,
            View::Artist(_) => View::Artists,
            v => v.clone(),
        }
    }
}

const LIBRARY_NAV: [(View, &str); 3] = [
    (View::Albums, "Albums"),
    (View::Artists, "Artists"),
    (View::Songs, "Songs"),
];
const PLAYER_NAV: [(View, &str); 3] = [
    (View::Queue, "Queue"),
    (View::Files, "Files"),
    (View::Visualizer, "Visualizer"),
];

/// Scroll position + selection of a list or grid. Rows have a fixed height per view.
#[derive(Debug, Default, Clone)]
pub(crate) struct ListState {
    pub selected: usize,
    pub scroll: f32,
    pub target: f32,
    /// Visible height, from the last frame.
    pub height: f32,
    /// Items per row (grids); 0 or 1 for lists.
    pub cols: usize,
}

impl ListState {
    pub fn clamp(&mut self, rows: usize, row_h: f32) {
        let max = (rows as f32 * row_h - self.height).max(0.0);
        self.target = self.target.clamp(0.0, max);
    }

    pub fn reveal(&mut self, row: usize, row_h: f32) {
        let top = row as f32 * row_h;
        if top < self.target {
            self.target = top;
        } else if top + row_h > self.target + self.height {
            self.target = top + row_h - self.height;
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
pub(crate) enum Drag {
    Seek(f32),
    Volume,
}

/// Library query results, refreshed when the library's version changes.
#[derive(Default)]
pub(crate) struct LibCache {
    pub version: Option<u64>,
    pub albums: Vec<Album>,
    pub artists: Vec<Artist>,
    pub songs: Arc<Vec<Track>>,
    /// Tracks of the currently open album/artist page.
    pub detail: Option<(View, Arc<Vec<Track>>)>,
    pub search: Option<(String, Arc<Vec<Track>>)>,
}

pub struct Gui {
    pub theme: Theme,
    pub library: Arc<Library>,
    pub view: View,
    history: Vec<View>,
    pub(crate) lists: std::collections::HashMap<&'static str, ListState>,
    pub(crate) browser: Browser,
    pub(crate) music_dirs: Vec<PathBuf>,
    pub(crate) drag: Option<Drag>,
    pub(crate) search: TextField,
    /// Set by `/` or Ctrl+F; applied after this frame so the key itself isn't typed.
    focus_search: bool,
    pub(crate) cache: LibCache,
    toast: Option<(String, Instant)>,
    last_frame: Instant,
    pub(crate) pulse: f32,
}

pub struct FrameResult {
    /// Something is moving; keep rendering at full rate.
    pub animating: bool,
}

/// Mutable rendering services a frame needs.
pub struct Gfx<'a> {
    pub text: &'a mut TextSystem,
    pub images: &'a mut ImageStore,
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
            view: View::Albums,
            history: Vec::new(),
            lists: Default::default(),
            browser: Browser::new(start),
            music_dirs,
            drag: None,
            search: TextField::default(),
            focus_search: false,
            cache: LibCache::default(),
            toast: None,
            last_frame: Instant::now(),
            pulse: 0.0,
        };
        (gui, problems)
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    /// Opens a view, remembering where we came from (for Backspace / mouse back).
    pub fn navigate(&mut self, view: View) {
        if view != self.view {
            let prev = std::mem::replace(&mut self.view, view);
            self.history.push(prev);
            if self.history.len() > 64 {
                self.history.remove(0);
            }
        }
    }

    /// Switches section from the sidebar: no history, like tabs.
    fn switch(&mut self, view: View) {
        self.history.clear();
        self.view = view;
    }

    fn back(&mut self) -> bool {
        match self.history.pop() {
            Some(v) => {
                self.view = v;
                true
            }
            None => false,
        }
    }

    pub(crate) fn list(&mut self, key: &'static str) -> &mut ListState {
        self.lists.entry(key).or_default()
    }

    fn refresh_library(&mut self) {
        let v = self.library.version();
        if self.cache.version == Some(v) {
            return;
        }
        self.cache.version = Some(v);
        self.cache.albums = self.library.albums();
        self.cache.artists = self.library.artists();
        self.cache.songs = Arc::new(self.library.all_tracks(TrackOrder::Artist));
        self.cache.detail = None;
        self.cache.search = None;
        // An empty library is better shown as files the first time.
        if self.cache.albums.is_empty()
            && self.cache.songs.is_empty()
            && self.view == View::Albums
            && !self.library.is_scanning()
        {
            self.view = View::Files;
        }
    }

    pub fn frame(
        &mut self,
        canvas: &mut Canvas,
        gfx: Gfx<'_>,
        input: &Input,
        player: &Player,
    ) -> FrameResult {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32().min(0.1);
        self.last_frame = now;
        let images_changed = gfx.images.poll();
        self.refresh_library();

        let snap = player.snapshot();
        let analysis = player.analysis();
        if !self.search.focused {
            self.handle_keys(input, player);
        }
        if input.back_pressed {
            self.back();
        }
        self.handle_drops(input, player);

        let theme = self.theme.clone();
        let library = self.library.clone();
        let mut ui = Ui {
            c: canvas,
            text: gfx.text,
            images: gfx.images,
            library: &library,
            input,
            theme: &theme,
        };
        let full = ui.c.bounds();
        let (body, bar) = full.split_bottom(BAR_H);
        let (sidebar, main) = body.split_left(SIDEBAR_W);

        self.sidebar(&mut ui, sidebar, player, &snap);
        let main = Rect::new(
            main.x + PAD,
            main.y + PAD - 4.0,
            main.w - 2.0 * PAD,
            main.h - PAD - 8.0,
        );
        self.content(&mut ui, main, player, &snap, &analysis, dt);
        self.now_playing(&mut ui, bar, player, &snap, &analysis);
        self.draw_toast(&mut ui, full);

        let mut scrolling = false;
        for l in self.lists.values_mut() {
            scrolling |= l.step(dt);
        }
        let animating = snap.state == PlaybackState::Playing
            || scrolling
            || images_changed
            || ui.images.is_loading()
            || self.drag.is_some()
            || self.toast.is_some()
            || self.search.focused
            || self.library.is_scanning()
            || analysis.bands.iter().any(|&b| b > 0.0);
        if input.released {
            self.drag = None;
        }
        if std::mem::take(&mut self.focus_search) {
            self.search.focused = true;
        }
        FrameResult {
            animating: animating || self.search.focused,
        }
    }

    fn handle_drops(&mut self, input: &Input, player: &Player) {
        if input.dropped.is_empty() {
            return;
        }
        let tracks = self.library.tracks_for_paths(&input.dropped);
        if tracks.is_empty() {
            self.toast("Nothing playable in what was dropped");
        } else {
            self.toast(format!(
                "Added {} to the queue",
                count(tracks.len(), "track")
            ));
            player.send(Cmd::Enqueue(tracks));
        }
    }

    fn handle_keys(&mut self, input: &Input, player: &Player) {
        for k in &input.keys {
            match k.key {
                Key::Space => player.send(Cmd::TogglePause),
                Key::Right => player.send(Cmd::SeekBy(if k.shift { 30_000 } else { 5_000 })),
                Key::Left => player.send(Cmd::SeekBy(if k.shift { -30_000 } else { -5_000 })),
                Key::Char('/') => self.focus_search = true,
                Key::Char('f') if k.ctrl => self.focus_search = true,
                Key::Backspace | Key::Escape if self.view != View::Files => {
                    self.back();
                }
                Key::Char('n') | Key::Char('>') => player.send(Cmd::Next),
                Key::Char('p') | Key::Char('<') => player.send(Cmd::Prev),
                Key::Char('+') | Key::Char('=') => player.send(Cmd::VolumeBy(0.05)),
                Key::Char('-') => player.send(Cmd::VolumeBy(-0.05)),
                Key::Char('s') => player.send(Cmd::ToggleShuffle),
                Key::Char('r') => player.send(Cmd::CycleRepeat),
                Key::Char('1') => self.switch(View::Albums),
                Key::Char('2') => self.switch(View::Artists),
                Key::Char('3') => self.switch(View::Songs),
                Key::Char('4') => self.switch(View::Queue),
                Key::Char('5') => self.switch(View::Files),
                Key::Char('6') => self.switch(View::Visualizer),
                Key::Char('o') => self.reveal_playing(player),
                _ => self.view_key(*k, player),
            }
        }
    }

    fn reveal_playing(&mut self, player: &Player) {
        if let Some(i) = player.queue().read().current_index() {
            self.switch(View::Queue);
            let l = self.list("queue");
            l.selected = i;
            l.reveal(i, ROW_H);
        }
    }

    /// Opens the album page for a track (clicking the now-playing cover).
    pub(crate) fn open_album_of(&mut self, track: &Track) {
        if let Some(album) = &track.album {
            self.navigate(View::Album {
                name: album.clone(),
                artist: track.primary_artist().map(str::to_owned),
            });
        }
    }

    fn sidebar(&mut self, ui: &mut Ui, r: Rect, player: &Player, snap: &PlayerSnapshot) {
        let t = ui.theme.clone();
        ui.c.rect(r, t.surface, 0.0);
        ui.label(
            "khoostic",
            r.x + 24.0,
            Rect::new(r.x, r.y + 16.0, r.w, 40.0),
            r.w,
            20.0,
            Weight::Bold,
            t.accent,
        );

        let field = Rect::new(r.x + 14.0, r.y + 64.0, r.w - 28.0, 36.0);
        let was_empty = self.search.text.is_empty();
        if ui.text_field(&mut self.search, field, "Search") {
            if self.search.text.trim().is_empty() {
                if self.view == View::Search {
                    self.back();
                }
            } else if self.view != View::Search {
                if was_empty {
                    self.navigate(View::Search);
                } else {
                    self.view = View::Search;
                }
            }
            self.list("search").selected = 0;
        }

        let mut y = r.y + 122.0;
        let queue_len = player.queue().read().len();
        for (title, items) in [("LIBRARY", &LIBRARY_NAV), ("PLAYING", &PLAYER_NAV)] {
            ui.label(
                title,
                r.x + 26.0,
                Rect::new(r.x, y, r.w, 22.0),
                r.w,
                10.5,
                Weight::Bold,
                t.dim.alpha(0.8),
            );
            y += 26.0;
            for (view, name) in items.iter() {
                let row = Rect::new(r.x + 12.0, y, r.w - 24.0, 36.0);
                let active = self.view.section() == *view;
                let hover = ui.hovered(row);
                if active || hover {
                    ui.c.rect(
                        row,
                        if active {
                            t.surface_hover
                        } else {
                            t.surface_hover.alpha(0.6)
                        },
                        t.radius,
                    );
                }
                let icon_color = if active { t.accent } else { t.dim };
                crate::views::nav_icon(
                    ui,
                    view,
                    Rect::new(row.x + 12.0, row.y + 10.0, 16.0, 16.0),
                    icon_color,
                );
                ui.label(
                    name,
                    row.x + 40.0,
                    row,
                    row.w - 80.0,
                    t.font_size,
                    Weight::Medium,
                    if active { t.fg } else { t.dim },
                );
                let badge = match view {
                    View::Queue if queue_len > 0 => Some(queue_len.to_string()),
                    View::Albums if !self.cache.albums.is_empty() => {
                        Some(self.cache.albums.len().to_string())
                    }
                    _ => None,
                };
                if let Some(b) = badge {
                    ui.label_right(
                        &b,
                        row.right() - 12.0,
                        row,
                        11.0,
                        Weight::Regular,
                        t.dim.alpha(0.8),
                    );
                }
                if ui.clicked(row) {
                    self.search.focused = false;
                    self.switch(view.clone());
                }
                y += 38.0;
            }
            y += 14.0;
        }

        let foot = Rect::new(r.x + 24.0, r.bottom() - 56.0, r.w - 36.0, 20.0);
        let scanning = self.library.is_scanning();
        let status = if scanning {
            "Scanning library…".to_string()
        } else {
            format!(
                "{} · {}",
                count(self.cache.songs.len(), "song"),
                count(self.cache.albums.len(), "album")
            )
        };
        ui.label(
            &status,
            foot.x,
            foot,
            foot.w,
            12.0,
            Weight::Regular,
            if scanning { t.accent } else { t.dim },
        );
        let info = format!("{} · {} Hz", snap.device, snap.sample_rate);
        ui.label(
            &info,
            foot.x,
            Rect {
                y: foot.y + 20.0,
                ..foot
            },
            foot.w,
            11.0,
            Weight::Regular,
            t.dim.alpha(0.7),
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

        // Left: cover + title/artist (click to open the album).
        let art = Rect::new(r.x + 16.0, r.y + (r.h - 62.0) / 2.0, 62.0, 62.0);
        let info_w = (r.w * 0.26).min(320.0);
        let tx = art.right() + 14.0;
        match &snap.current {
            Some(e) => {
                let seed = e.track.album.as_deref().unwrap_or(&e.track.title);
                ui.c.glow(
                    art,
                    t.gradient(0.5).alpha(0.15 + 0.35 * a.bass),
                    t.radius,
                    10.0,
                );
                ui.cover(
                    art,
                    e.track.cover.as_ref(),
                    CoverSize::Large,
                    t.radius,
                    seed,
                );
                let title_row = Rect::new(tx, r.y + 24.0, info_w, 24.0);
                ui.label(
                    &e.track.title,
                    tx,
                    title_row,
                    info_w,
                    t.font_size + 1.0,
                    Weight::Bold,
                    t.fg,
                );
                let sub = match e.track.artist_line() {
                    s if s.is_empty() => crate::views::album_of(&e.track),
                    s => s,
                };
                ui.label(
                    &sub,
                    tx,
                    Rect::new(tx, r.y + 48.0, info_w, 22.0),
                    info_w,
                    t.font_size - 1.0,
                    Weight::Regular,
                    t.dim,
                );
                let clickable = Rect::new(art.x, art.y, info_w + 76.0, art.h);
                if ui.clicked(clickable) {
                    let track = (*e.track).clone();
                    self.open_album_of(&track);
                }
            }
            None => {
                ui.cover(art, None, CoverSize::Large, t.radius, "khoostic");
                ui.label(
                    "Nothing playing",
                    tx,
                    Rect::new(tx, r.y, info_w, r.h),
                    info_w,
                    t.font_size,
                    Weight::Medium,
                    t.dim,
                );
            }
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
            let col = if ui.hovered(b) { t.fg } else { t.dim };
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
        let shown = match self.drag {
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
            &fmt_time(shown),
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
        crate::views::speaker_icon(
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
        let r = Rect::new(
            SIDEBAR_W + (full.w - SIDEBAR_W - w) / 2.0,
            full.h - BAR_H - 64.0,
            w,
            40.0,
        );
        ui.c.set_layer(1);
        ui.c.glow(r, Color::BLACK.alpha(0.5 * alpha), 20.0, 14.0);
        ui.c.rect(r, t.surface_hover.alpha(alpha), 20.0);
        let msg = msg.clone();
        ui.label_center(&msg, r, t.font_size, Weight::Medium, t.fg.alpha(alpha));
        ui.c.set_layer(0);
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

/// "1 track", "3 tracks".
pub fn count(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

/// "1 hr 3 min", "42 min".
pub fn fmt_long(d: Duration) -> String {
    let m = (d.as_secs() + 30) / 60;
    if m >= 60 {
        format!("{} hr {} min", m / 60, m % 60)
    } else {
        format!("{m} min")
    }
}

pub(crate) fn local_path(t: &Track) -> Option<&std::path::Path> {
    match &t.locator {
        TrackLocator::Local(p) => Some(p),
        _ => None,
    }
}
