//! The GUI's state, navigation, chrome (sidebar, now-playing bar) and input handling.
//! Content pages live in `views.rs`, `spotify_views.rs` and `settings.rs`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use khoostic_audio::{AnalysisFrame, PlaybackState};
use khoostic_config::Config;
use khoostic_core::{EntryId, RepeatMode, Track, TrackLocator};
use khoostic_library::{Album, Artist, CoverSize, Library, TrackOrder};
use khoostic_player::browse::Browser;
use khoostic_player::{Player, PlayerCommand as Cmd, PlayerSnapshot};

use crate::anim::{self, Anims, ease_out};
use crate::canvas::{Canvas, Color, Rect, Weight};
use crate::icons::{self, Icon};
use crate::images::ImageStore;
use crate::text::TextSystem;
use crate::ui::{Input, Key, TextField, Theme, Ui};

pub(crate) const SIDEBAR_W: f32 = 236.0;
pub(crate) const BAR_H: f32 = 96.0;
pub(crate) const ROW_H: f32 = 48.0;
pub(crate) const PAD: f32 = 32.0;
/// Space between the content panel and the window edges.
const GAP: f32 = 8.0;
/// How long a page takes to fade in after navigating.
const PAGE_FADE: f32 = 0.22;

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
    Playlists,
    LocalPlaylist(i64),
    SpotifyPlaylist {
        id: String,
        name: String,
    },
    SpotifyAlbum {
        id: String,
        name: String,
    },
    Liked,
    Settings,
}

impl View {
    /// The sidebar entry this view belongs to.
    fn section(&self) -> View {
        match self {
            View::Album { .. } => View::Albums,
            View::Artist(_) => View::Artists,
            View::LocalPlaylist(_) | View::SpotifyPlaylist { .. } => View::Playlists,
            View::SpotifyAlbum { .. } => View::Search,
            v => v.clone(),
        }
    }
}

const LIBRARY_NAV: [(View, &str, Icon); 4] = [
    (View::Albums, "Albums", Icon::Album),
    (View::Artists, "Artists", Icon::Artist),
    (View::Songs, "Songs", Icon::Song),
    (View::Playlists, "Playlists", Icon::Playlist),
];
const PLAYER_NAV: [(View, &str, Icon); 3] = [
    (View::Queue, "Queue", Icon::Queue),
    (View::Files, "Files", Icon::Folder),
    (View::Visualizer, "Visualizer", Icon::Visualizer),
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
        self.scroll += diff * (1.0 - (-16.0 * dt).exp());
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Drag {
    Seek(f32),
    Volume(f32),
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

/// The playback position as shown: continuous between the engine's (buffer-sized) updates,
/// and held at a seek target until the engine gets there, so the bar never snaps back.
#[derive(Default)]
pub(crate) struct PlayClock {
    entry: Option<EntryId>,
    pos: f64,
    hold: Option<(f64, Instant)>,
    /// Drawn fill fraction of the seek bar; glides across jumps.
    pub bar: f32,
}

impl PlayClock {
    fn update(&mut self, snap: &PlayerSnapshot, dt: f32) {
        let id = snap.current.as_ref().map(|e| e.id);
        let actual = snap.position.as_secs_f64();
        if id != self.entry {
            self.entry = id;
            self.pos = actual;
            self.hold = None;
            return;
        }
        if let Some((target, at)) = self.hold {
            if (actual - target).abs() < 0.75 || at.elapsed() > Duration::from_secs(2) {
                self.hold = None;
            } else {
                self.pos = target;
                return;
            }
        }
        if snap.state == PlaybackState::Playing {
            self.pos += dt as f64;
            let err = actual - self.pos;
            if err.abs() > 0.4 {
                self.pos = actual; // a seek or a stall: jump (the bar eases separately)
            } else {
                self.pos += err * (dt as f64 * 6.0).min(1.0);
            }
        } else {
            self.pos = actual;
        }
        if let Some(d) = snap.duration {
            self.pos = self.pos.clamp(0.0, d.as_secs_f64());
        }
    }

    pub fn position(&self) -> Duration {
        Duration::from_secs_f64(self.pos.max(0.0))
    }

    fn seeked(&mut self, to: Duration) {
        self.pos = to.as_secs_f64();
        self.hold = Some((self.pos, Instant::now()));
    }
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
    pub spotify: Option<Arc<khoostic_spotify::Spotify>>,
    fetch: Option<crate::ui::UrlFetcher>,
    pub(crate) sp: crate::spotify_views::SpState,
    pub(crate) menu: Option<crate::spotify_views::ContextMenu>,
    /// Last known mouse position (for anchoring menus).
    pub(crate) mouse: (f32, f32),
    /// Search shows Spotify results instead of the library.
    pub(crate) search_spotify: bool,
    anims: Anims,
    /// The view the page transition last started for.
    shown_view: View,
    view_since: Instant,
    pub(crate) clock: PlayClock,
    /// Volume to restore when unmuting from the speaker button.
    unmute_to: f32,
    /// Frame rates (focused, unfocused) the window loop paces to.
    pub fps: u32,
    pub background_fps: u32,
    pub(crate) config_path: Option<PathBuf>,
    /// Measured frames per second while animating.
    pub(crate) measured_fps: f32,
    /// The keyboard moved the selection last (show focus rings); cleared by clicking.
    pub(crate) kb_nav: bool,
}

pub struct FrameResult {
    /// Something is moving; keep rendering at full rate.
    pub animating: bool,
    /// The pointer is over something clickable.
    pub pointer: bool,
}

/// Mutable rendering services a frame needs.
pub struct Gfx<'a> {
    pub text: &'a mut TextSystem,
    pub images: &'a mut ImageStore,
}

impl Gui {
    pub fn new(
        config: &Config,
        library: Arc<Library>,
        spotify: Option<Arc<khoostic_spotify::Spotify>>,
    ) -> (Self, Vec<String>) {
        let fetch = spotify
            .clone()
            .map(|s| -> crate::ui::UrlFetcher { Arc::new(move |url: &str| s.fetch_bytes(url)) });
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
            spotify,
            fetch,
            sp: Default::default(),
            menu: None,
            mouse: (0.0, 0.0),
            search_spotify: false,
            anims: Anims::default(),
            shown_view: View::Albums,
            view_since: Instant::now() - Duration::from_secs(1),
            clock: PlayClock::default(),
            unmute_to: 0.8,
            fps: g.fps.clamp(10, 360),
            background_fps: g.background_fps.clamp(1, 360),
            config_path: config.source.clone(),
            measured_fps: 0.0,
            kb_nav: false,
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
    pub(crate) fn switch(&mut self, view: View) {
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
        if dt > 0.0 && dt < 0.2 {
            let inst = 1.0 / dt;
            self.measured_fps += (inst - self.measured_fps) * 0.1;
        }
        let images_changed = gfx.images.poll();
        self.refresh_library();
        self.poll_spotify();
        if let Some(m) = input.mouse {
            self.mouse = m;
        }
        if input.pressed {
            self.kb_nav = false;
        }

        let snap = player.snapshot();
        let analysis = player.analysis();
        self.clock.update(&snap, dt);
        if !self.search.focused && self.menu.is_none() {
            self.handle_keys(input, player);
        }
        if input.back_pressed {
            self.back();
        }
        self.handle_drops(input, player);
        if self.view != self.shown_view {
            self.shown_view = self.view.clone();
            self.view_since = now;
        }

        let theme = self.theme.clone();
        let library = self.library.clone();
        let fetch = self.fetch.clone();
        let mut anims = std::mem::take(&mut self.anims);
        anims.begin(dt);
        let mut ui = Ui {
            c: canvas,
            text: gfx.text,
            images: gfx.images,
            library: &library,
            fetch: fetch.as_ref(),
            input,
            theme: &theme,
            anim: anims,
            pointer: false,
        };
        // While a menu is open, the page underneath must not react to its clicks.
        let masked = Input {
            pressed: false,
            right_pressed: false,
            double_click: false,
            scroll: 0.0,
            keys: Vec::new(),
            ..input.clone()
        };
        if self.menu.is_some() {
            ui.input = &masked;
        }
        let full = ui.c.bounds();
        let (body, bar) = full.split_bottom(BAR_H);
        let (sidebar, rest) = body.split_left(SIDEBAR_W);
        let panel = Rect::new(rest.x, rest.y + GAP, rest.w - GAP, rest.h - GAP);

        self.sidebar(&mut ui, sidebar, player);
        ui.c.rect(panel, theme.surface, 16.0);
        let main = Rect::new(
            panel.x + PAD,
            panel.y + PAD - 4.0,
            panel.w - 2.0 * PAD,
            panel.h - PAD - 4.0,
        );
        // New pages fade in and settle up into place.
        let age = self.view_since.elapsed().as_secs_f32();
        let e = ease_out(age / PAGE_FADE);
        if e < 1.0 {
            ui.anim.keep_alive();
        }
        ui.c.push_clip(panel);
        ui.c.push_fx(0.0, (1.0 - e) * 14.0, e);
        self.content(&mut ui, main, player, &snap, &analysis, dt);
        ui.c.pop_fx();
        ui.c.pop_clip();
        self.now_playing(&mut ui, bar, player, &snap, &analysis);
        ui.input = input;
        self.draw_menu(&mut ui, player);
        self.draw_toast(&mut ui, panel);

        let mut scrolling = false;
        for l in self.lists.values_mut() {
            scrolling |= l.step(dt);
        }
        let pointer = ui.pointer;
        let animating = snap.state == PlaybackState::Playing
            || ui.anim.moving()
            || scrolling
            || images_changed
            || ui.images.is_loading()
            || self.drag.is_some()
            || self.toast.is_some()
            || self.library.is_scanning()
            || self.spotify_busy()
            || analysis.bands.iter().any(|&b| b > 0.0);
        self.anims = std::mem::take(&mut ui.anim);
        if input.released {
            self.drag = None;
        }
        if std::mem::take(&mut self.focus_search) {
            self.search.focused = true;
        }
        FrameResult { animating, pointer }
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
                Key::Char(',') if k.ctrl => self.switch(View::Settings),
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

    // ---- sidebar ----------------------------------------------------------------------------

    fn sidebar(&mut self, ui: &mut Ui, r: Rect, player: &Player) {
        let t = ui.theme.clone();
        let mark = Rect::new(r.x + 24.0, r.y + 18.0, r.w, 34.0);
        ui.c.circle(mark.x + 4.0, mark.y + mark.h / 2.0, 4.0, t.accent);
        ui.label(
            "khoostic",
            mark.x + 16.0,
            mark,
            r.w,
            18.0,
            Weight::Bold,
            t.fg,
        );

        let field = Rect::new(r.x + 16.0, r.y + 66.0, r.w - 24.0, 38.0);
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

        // Lay out every row first so the selection indicator can glide between them.
        struct NavRow {
            view: View,
            label: String,
            icon: Icon,
            rect: Rect,
            badge: Option<String>,
        }
        let queue_len = player.queue().read().len();
        let mut rows: Vec<NavRow> = Vec::new();
        let mut headers: Vec<(&str, f32)> = Vec::new();
        let mut y = r.y + 124.0;
        for (title, items) in [("Library", &LIBRARY_NAV[..]), ("Playing", &PLAYER_NAV[..])] {
            headers.push((title, y));
            y += 26.0;
            for (view, name, icon) in items {
                let badge = match view {
                    View::Queue if queue_len > 0 => Some(queue_len.to_string()),
                    _ => None,
                };
                rows.push(NavRow {
                    view: view.clone(),
                    label: name.to_string(),
                    icon: *icon,
                    rect: Rect::new(r.x + 12.0, y, r.w - 20.0, 36.0),
                    badge,
                });
                y += 38.0;
            }
            y += 16.0;
        }
        let spotify_y = y;
        let settings = Rect::new(r.x + 12.0, r.bottom() - 52.0, r.w - 20.0, 36.0);
        rows.push(NavRow {
            view: View::Settings,
            label: "Settings".into(),
            icon: Icon::Settings,
            rect: settings,
            badge: None,
        });
        let active = self.view.section();
        let liked_rect = Rect::new(r.x + 12.0, spotify_y + 26.0, r.w - 20.0, 36.0);
        let target = rows
            .iter()
            .find(|row| row.view == active)
            .map(|row| row.rect.y)
            .or((active == View::Liked).then_some(liked_rect.y));
        if let Some(ty) = target {
            let iy = ui.anim.to(anim::id("nav-y"), ty, 18.0);
            ui.c.rect(
                Rect::new(r.x + 12.0, iy, r.w - 20.0, 36.0),
                t.wash(0.075),
                t.radius,
            );
        }
        for (title, hy) in headers {
            ui.label(
                title,
                r.x + 26.0,
                Rect::new(r.x, hy, r.w, 22.0),
                r.w,
                11.5,
                Weight::Medium,
                t.dim.alpha(0.75),
            );
        }
        for row in &rows {
            let is_active = row.view == active;
            let id = anim::id(("nav", &row.label));
            let h = ui.hover_t(id, row.rect);
            if !is_active {
                ui.c.rect(row.rect, t.wash(0.04 * h), t.radius);
            }
            let on = ui.anim.to(id ^ 3, is_active as u8 as f32, anim::NORMAL);
            let icon_color = t.dim.lerp(t.fg, h * 0.6).lerp(t.accent, on);
            icons::draw(
                ui.c,
                row.icon,
                Rect::new(row.rect.x + 12.0, row.rect.y + 9.0, 18.0, 18.0),
                icon_color,
            );
            ui.label(
                &row.label,
                row.rect.x + 42.0,
                row.rect,
                row.rect.w - 80.0,
                t.font_size,
                Weight::Medium,
                t.dim.lerp(t.fg, (h * 0.7 + on).min(1.0)),
            );
            if let Some(b) = &row.badge {
                ui.label_right(
                    b,
                    row.rect.right() - 12.0,
                    row.rect,
                    11.5,
                    Weight::Medium,
                    t.dim.alpha(0.8),
                );
            }
            if ui.clicked(row.rect) {
                self.search.focused = false;
                self.switch(row.view.clone());
            }
        }
        self.sidebar_spotify(ui, r, spotify_y);
    }

    // ---- now playing bar ----------------------------------------------------------------------

    fn now_playing(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        player: &Player,
        snap: &PlayerSnapshot,
        a: &AnalysisFrame,
    ) {
        let t = ui.theme.clone();

        // Left: cover + title/artist (click to open the album).
        let art = Rect::new(r.x + 18.0, r.y + (r.h - 60.0) / 2.0, 60.0, 60.0);
        let info_w = (r.w * 0.26).min(300.0);
        let tx = art.right() + 14.0;
        match &snap.current {
            Some(e) => {
                let seed = e.track.album.as_deref().unwrap_or(&e.track.title);
                let clickable = Rect::new(art.x, art.y, info_w + 76.0, art.h);
                let h = ui.hover_t(anim::id("np-art"), clickable);
                let beat = ui.anim.to(anim::id("np-bass"), a.bass, 18.0);
                ui.c.glow(art, t.gradient(0.5).alpha(0.08 + 0.3 * beat), 10.0, 12.0);
                ui.cover(art, e.track.cover.as_ref(), CoverSize::Large, 8.0, seed);
                ui.label(
                    &e.track.title,
                    tx,
                    Rect::new(tx, r.y + 26.0, info_w, 22.0),
                    info_w,
                    t.font_size + 0.5,
                    Weight::Medium,
                    t.fg,
                );
                let sub = match e.track.artist_line() {
                    s if s.is_empty() => crate::views::album_of(&e.track),
                    s => s,
                };
                ui.label(
                    &sub,
                    tx,
                    Rect::new(tx, r.y + 49.0, info_w, 20.0),
                    info_w,
                    t.font_size - 1.5,
                    Weight::Regular,
                    t.dim.lerp(t.fg, 0.4 * h),
                );
                if ui.clicked(clickable) {
                    let track = (*e.track).clone();
                    self.open_album_of(&track);
                }
            }
            None => {
                ui.c.rect(art, t.wash(0.05), 8.0);
                icons::draw(
                    ui.c,
                    Icon::Song,
                    Rect::new(art.x + 19.0, art.y + 19.0, 22.0, 22.0),
                    t.dim.alpha(0.6),
                );
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

        // Center: transport.
        let cx = r.x + r.w / 2.0;
        let cy = r.y + 32.0;
        let playing = snap.state == PlaybackState::Playing;
        let play = Rect::new(cx - 20.0, cy - 20.0, 40.0, 40.0);
        let pid = anim::id("np-play");
        let h = ui.hover_t(pid, play);
        let p = ui.press_t(pid, play);
        let rad = 19.0 * (1.0 + 0.06 * h - 0.08 * p);
        ui.c.circle(cx, cy, rad, t.fg);
        let morph = ui.anim.to(pid ^ 5, playing as u8 as f32, 20.0);
        let s = 18.0 * (1.0 - 0.08 * p);
        let icon_r = Rect::new(cx - s / 2.0, cy - s / 2.0, s, s);
        let ink = t.background;
        if morph < 0.999 {
            ui.c.push_fx(0.0, 0.0, 1.0 - morph);
            icons::draw(ui.c, Icon::Play, icon_r, ink);
            ui.c.pop_fx();
        }
        if morph > 0.001 {
            ui.c.push_fx(0.0, 0.0, morph);
            icons::draw(ui.c, Icon::Pause, icon_r, ink);
            ui.c.pop_fx();
        }
        if ui.clicked(play) {
            player.send(Cmd::TogglePause);
        }
        let (dim, fg, accent) = (t.dim, t.fg, t.accent);
        if ui.icon_button(
            anim::id("np-prev"),
            Icon::Prev,
            (cx - 50.0, cy),
            20.0,
            fg.alpha(0.85),
            fg,
        ) {
            player.send(Cmd::Prev);
        }
        if ui.icon_button(
            anim::id("np-next"),
            Icon::Next,
            (cx + 50.0, cy),
            20.0,
            fg.alpha(0.85),
            fg,
        ) {
            player.send(Cmd::Next);
        }
        let toggles = [
            (Icon::Shuffle, snap.shuffle, -96.0, Cmd::ToggleShuffle),
            (
                if snap.repeat == RepeatMode::One {
                    Icon::RepeatOne
                } else {
                    Icon::Repeat
                },
                snap.repeat != RepeatMode::Off,
                96.0,
                Cmd::CycleRepeat,
            ),
        ];
        for (icon, on, dx, cmd) in toggles {
            let id = anim::id(("np-toggle", dx as i32));
            let o = ui.anim.to(id ^ 9, on as u8 as f32, anim::NORMAL);
            let base = dim.lerp(accent, o);
            if ui.icon_button(id, icon, (cx + dx, cy), 18.0, base, fg.lerp(accent, o)) {
                player.send(cmd);
            }
            if o > 0.01 {
                ui.c.circle(cx + dx, cy + 15.0, 2.0 * o, accent.alpha(o));
            }
        }

        // Seek bar.
        let bar_w = (r.w - 2.0 * (info_w + 140.0)).clamp(240.0, 560.0);
        let seek = Rect::new(cx - bar_w / 2.0, r.y + 62.0, bar_w, 18.0);
        let dur = snap.duration.unwrap_or_default();
        let frac_at = |x: f32| ((x - seek.x) / seek.w).clamp(0.0, 1.0);
        if ui.clicked(seek) && !dur.is_zero() {
            self.drag = Some(Drag::Seek(frac_at(ui.input.mouse.unwrap_or_default().0)));
        }
        if let (Some(Drag::Seek(_)), Some((mx, _))) = (self.drag, ui.input.mouse) {
            self.drag = Some(Drag::Seek(frac_at(mx)));
            if ui.input.released {
                let to = dur.mul_f32(frac_at(mx));
                player.send(Cmd::Seek(to));
                self.clock.seeked(to);
            }
        }
        let (frac, shown) = match self.drag {
            Some(Drag::Seek(f)) => (f, dur.mul_f32(f)),
            _ if dur.is_zero() => (0.0, Duration::ZERO),
            _ => {
                let pos = self.clock.position().min(dur);
                (pos.as_secs_f32() / dur.as_secs_f32(), pos)
            }
        };
        // Follow small, continuous motion exactly; glide across jumps (seeks, track changes).
        let dragging = matches!(self.drag, Some(Drag::Seek(_)));
        let jump = (frac - self.clock.bar).abs() * seek.w;
        if dragging || jump < 2.0 {
            self.clock.bar = frac;
        } else {
            self.clock.bar += (frac - self.clock.bar) * (1.0 - (-18.0 * ui.anim.dt()).exp());
            ui.anim.keep_alive();
        }
        let sid = anim::id("np-seek");
        let hot = ui.hovered(seek) || dragging;
        ui.pointer |= hot;
        let hs = ui.anim.to(sid, hot as u8 as f32, anim::FAST);
        draw_slider(ui, seek, self.clock.bar, hs, t.fg.lerp(t.accent, hs));
        let times = Rect::new(seek.x - 54.0, seek.y, 44.0, seek.h);
        ui.label_right(
            &fmt_time(shown),
            times.right(),
            times,
            11.5,
            Weight::Regular,
            t.dim,
        );
        let end = if dur.is_zero() {
            "–:––".to_string()
        } else {
            fmt_time(dur)
        };
        ui.label(
            &end,
            seek.right() + 10.0,
            seek,
            50.0,
            11.5,
            Weight::Regular,
            t.dim,
        );

        // Right: queue, volume.
        let vol = Rect::new(r.right() - 132.0, r.y + (r.h - 18.0) / 2.0, 104.0, 18.0);
        let vid = anim::id("np-vol");
        if ui.clicked(vol) {
            self.drag = Some(Drag::Volume(frac_vol(vol, ui.input.mouse)));
        }
        if let Some(Drag::Volume(_)) = self.drag {
            let v = frac_vol(vol, ui.input.mouse);
            self.drag = Some(Drag::Volume(v));
            player.send(Cmd::SetVolume(v));
        }
        if ui.hovered(vol) && ui.input.scroll != 0.0 {
            player.send(Cmd::VolumeBy(-ui.input.scroll.signum() * 0.05));
        }
        let volume = match self.drag {
            Some(Drag::Volume(v)) => v,
            _ => snap.volume,
        };
        let shown_vol = ui.anim.to(vid ^ 4, volume, 24.0);
        let hot = ui.hovered(vol) || matches!(self.drag, Some(Drag::Volume(_)));
        ui.pointer |= hot;
        let hv = ui.anim.to(vid, hot as u8 as f32, anim::FAST);
        draw_slider(ui, vol, shown_vol, hv, t.fg.lerp(t.accent, hv));
        if ui.icon_button(
            anim::id("np-mute"),
            Icon::Volume(volume),
            (vol.x - 22.0, vol.y + vol.h / 2.0),
            18.0,
            dim,
            fg,
        ) {
            if volume > 0.001 {
                self.unmute_to = volume;
                player.send(Cmd::SetVolume(0.0));
            } else {
                player.send(Cmd::SetVolume(self.unmute_to.max(0.05)));
            }
        }
        let on_queue = self.view == View::Queue;
        let qc = if on_queue { accent } else { dim };
        if ui.icon_button(
            anim::id("np-queue"),
            Icon::Queue,
            (vol.x - 64.0, vol.y + vol.h / 2.0),
            18.0,
            qc,
            if on_queue { accent } else { fg },
        ) {
            if on_queue {
                self.back();
            } else {
                self.navigate(View::Queue);
            }
        }
    }

    fn draw_toast(&mut self, ui: &mut Ui, panel: Rect) {
        let Some((msg, at)) = &self.toast else { return };
        let age = at.elapsed().as_secs_f32();
        const LIFE: f32 = 3.2;
        if age > LIFE {
            self.toast = None;
            return;
        }
        let appear = ease_out(age / 0.25);
        let alpha = appear * ((LIFE - age) / 0.35).min(1.0);
        let t = ui.theme;
        let w = ui.measure(msg, t.font_size, Weight::Medium) + 40.0;
        let r = Rect::new(
            panel.x + (panel.w - w) / 2.0,
            panel.bottom() - 64.0 + (1.0 - appear) * 16.0,
            w,
            42.0,
        );
        ui.c.set_layer(1);
        ui.c.glow(r, Color::BLACK.alpha(0.45 * alpha), 21.0, 18.0);
        ui.c.rect(r, t.surface_hover.alpha(alpha), 21.0);
        ui.c.outline(r, t.line().alpha(alpha), 21.0, 1.0);
        let msg = msg.clone();
        ui.label_center(&msg, r, t.font_size, Weight::Medium, t.fg.alpha(alpha));
        ui.c.set_layer(0);
    }
}

fn frac_vol(vol: Rect, mouse: Option<(f32, f32)>) -> f32 {
    ((mouse.unwrap_or_default().0 - vol.x) / vol.w).clamp(0.0, 1.0)
}

/// A thin track with a fill and a knob that grows in on hover (`hot` 0..1).
pub(crate) fn draw_slider(ui: &mut Ui, r: Rect, frac: f32, hot: f32, fill: Color) {
    let t = ui.theme;
    let th = 4.0 + 2.0 * hot;
    let track = Rect::new(r.x, r.y + (r.h - th) / 2.0, r.w, th);
    ui.c.rect(track, t.wash(0.14), th / 2.0);
    let fw = (track.w * frac.clamp(0.0, 1.0)).max(th);
    ui.c.rect(Rect { w: fw, ..track }, fill, th / 2.0);
    if hot > 0.01 {
        let x = track.x + track.w * frac.clamp(0.0, 1.0);
        let cy = track.y + th / 2.0;
        ui.c.glow(
            Rect::new(x - 6.0 * hot, cy - 6.0 * hot, 12.0 * hot, 12.0 * hot),
            Color::BLACK.alpha(0.4 * hot),
            6.0,
            4.0,
        );
        ui.c.circle(x, cy, 6.5 * hot, t.fg);
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

#[cfg(test)]
mod tests {
    use super::*;
    use khoostic_core::QueueEntry;

    fn snap(pos: f64, state: PlaybackState, id: u64) -> PlayerSnapshot {
        PlayerSnapshot {
            state,
            current: Some(QueueEntry {
                id: EntryId(id),
                track: Arc::new(Track::default()),
            }),
            position: Duration::from_secs_f64(pos),
            duration: Some(Duration::from_secs(200)),
            volume: 1.0,
            shuffle: false,
            repeat: RepeatMode::Off,
            buffered: Duration::ZERO,
            sample_rate: 48_000,
            device: String::new(),
        }
    }

    #[test]
    fn clock_is_smooth_between_coarse_updates() {
        let mut c = PlayClock::default();
        c.update(&snap(10.0, PlaybackState::Playing, 1), 0.0);
        // The engine reports in 20 ms steps; we render at 120 fps.
        let mut last = c.pos;
        for i in 1..=60 {
            let engine = 10.0 + ((i as f64 / 120.0) / 0.02).floor() * 0.02;
            c.update(&snap(engine, PlaybackState::Playing, 1), 1.0 / 120.0);
            assert!(c.pos > last, "position must keep moving forward");
            assert!((c.pos - engine).abs() < 0.05);
            last = c.pos;
        }
    }

    #[test]
    fn seek_target_is_held_until_the_engine_arrives() {
        let mut c = PlayClock::default();
        c.update(&snap(10.0, PlaybackState::Playing, 1), 0.0);
        c.seeked(Duration::from_secs(120));
        // The engine still reports the old position for a few frames.
        c.update(&snap(10.02, PlaybackState::Playing, 1), 0.016);
        assert_eq!(c.position(), Duration::from_secs(120));
        c.update(&snap(120.01, PlaybackState::Playing, 1), 0.016);
        assert!(c.hold.is_none());
        assert!((c.pos - 120.0).abs() < 0.1);
    }

    #[test]
    fn new_track_resets_the_clock() {
        let mut c = PlayClock::default();
        c.update(&snap(150.0, PlaybackState::Playing, 1), 0.0);
        c.update(&snap(0.0, PlaybackState::Playing, 2), 0.016);
        assert!(c.pos < 0.01);
    }
}
