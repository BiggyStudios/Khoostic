//! The GUI's state and its frame: layout of sidebar, page and player bar, navigation,
//! global keys, drops and toasts. Pages live in `views`, widgets in `ui`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::audio::PlaybackState;
use crate::config::Config;
use crate::discovery::Discovery;
use crate::genres::Genres;
use crate::gui::player_bar::PlayClock;
use crate::gui::queue_panel::QUEUE_W;
use crate::gui::render::images::ImageStore;
use crate::gui::render::text::TextSystem;
use crate::gui::render::{Canvas, Rect, Weight};
use crate::gui::state::UiState;
use crate::gui::ui::anim::{self, Anims, ease_out};
use crate::gui::ui::{Input, Key, TextField, Theme, Ui};
use crate::library::{Album, Artist, Library, TrackOrder};
use crate::lyrics::LyricsService;
use crate::model::{Track, TrackLocator};
use crate::pending::Pending;
use crate::player::browse::Browser;
use crate::player::{Player, PlayerCommand as Cmd};

/// Library panel width, open and collapsed to covers (gap included).
pub(crate) const SIDEBAR_W: f32 = 304.0;
pub(crate) const SIDEBAR_MIN: f32 = 84.0;
pub(crate) const BAR_H: f32 = 96.0;
pub(crate) const ROW_H: f32 = 48.0;
pub(crate) const PAD: f32 = 32.0;
/// Height of the top bar (search, navigation).
pub(crate) const TOP_H: f32 = 64.0;

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
    /// A Spotify artist's profile.
    SpotifyArtist {
        id: String,
        name: String,
    },
    /// Lyrics of the playing song.
    Lyrics,
    Settings,
    /// Listening together.
    Jam,
}

impl View {
    /// The sidebar entry this view belongs to.
    pub(crate) fn section(&self) -> View {
        match self {
            View::Album { .. } => View::Albums,
            View::Artist(_) => View::Artists,
            View::LocalPlaylist(_) | View::SpotifyPlaylist { .. } => View::Playlists,
            View::SpotifyAlbum { .. } => View::Search,
            View::Songs | View::Files | View::Playlists => View::Albums,
            v => v.clone(),
        }
    }
}

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

/// Which results the search page shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchTab {
    All,
    Songs,
    Artists,
    Albums,
    /// Only what's in your library.
    Library,
}

/// Something the window should do (the app draws its own title bar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowCmd {
    Minimize,
    ToggleMaximize,
    Close,
    /// Start moving the window with the mouse.
    Drag,
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
    /// What the search page lists (library + Spotify), keyed by query and sources.
    pub search: Option<(String, Arc<Vec<Track>>)>,
    /// Library-only matches for the current query.
    pub lib_search: Option<(String, Arc<Vec<Track>>)>,
    /// The song list the search page shows now (keyboard navigation).
    pub search_shown: Option<Arc<Vec<Track>>>,
    /// Library songs by the artist whose Spotify page is open.
    pub artist_tracks: Option<(String, Arc<Vec<Track>>)>,
}

pub struct Gui {
    pub theme: Theme,
    pub library: Arc<Library>,
    pub view: View,
    pub(crate) history: Vec<View>,
    /// Views gone back from, for the forward button.
    pub(crate) forward: Vec<View>,
    /// The queue panel is showing on the right.
    pub(crate) queue_open: bool,
    pub(crate) lists: std::collections::HashMap<&'static str, ListState>,
    pub(crate) browser: Browser,
    pub(crate) music_dirs: Vec<PathBuf>,
    pub(crate) drag: Option<Drag>,
    pub(crate) search: TextField,
    /// Set by `/` or Ctrl+F; applied after this frame so the key itself isn't typed.
    pub(crate) focus_search: bool,
    pub(crate) cache: LibCache,
    pub(crate) toast: Option<(String, Instant)>,
    pub(crate) last_frame: Instant,
    pub(crate) pulse: f32,
    /// The visualizer page's 3D mode.
    pub(crate) viz3d: crate::gui::views::visualizer::Viz3d,
    /// The visualizer's knobs and the user's saved presets (`[visualizer]` in the config).
    pub(crate) viz_tuning: crate::config::VisualizerTuning,
    pub(crate) viz_presets: std::collections::BTreeMap<String, crate::config::VisualizerTuning>,
    /// Settings: which knob group is open, the slider being dragged, unsaved slider changes
    /// (written when the drag ends), and the name field for saving a preset.
    pub(crate) viz_group: crate::config::visualizer::Group,
    pub(crate) viz_drag: Option<usize>,
    pub(crate) viz_dirty: bool,
    pub(crate) preset_name: TextField,
    pub spotify: Option<Arc<crate::spotify::Spotify>>,
    pub(crate) fetch: Option<crate::gui::ui::UrlFetcher>,
    pub(crate) sp: crate::gui::spotify::SpState,
    pub(crate) menu: Option<crate::gui::menu::ContextMenu>,
    /// Last known mouse position (for anchoring menus).
    pub(crate) mouse: (f32, f32),
    /// Which results the search page shows.
    pub(crate) search_tab: SearchTab,
    pub discovery: Arc<Discovery>,
    /// A song radio being put together: (seed, suggestions).
    pub(crate) radio: Option<(Track, Pending<Vec<Track>>)>,
    /// Last.fm API key/secret inputs on the Settings page.
    pub(crate) lf_key: TextField,
    pub(crate) lf_secret: TextField,
    /// Discord status (None in headless runs).
    pub(crate) discord: Option<Arc<crate::discord::Discord>>,
    /// Listening together (None in headless runs).
    pub(crate) jams: Option<Arc<crate::jam::Jams>>,
    pub(crate) jam_invite: TextField,
    /// Angle of the ring around the play button while a song opens.
    pub(crate) load_spin: f32,
    pub(crate) anims: Anims,
    /// The view the page transition last started for.
    pub(crate) shown_view: View,
    pub(crate) view_since: Instant,
    pub(crate) clock: PlayClock,
    /// Volume to restore when unmuting from the speaker button.
    pub(crate) unmute_to: f32,
    /// Frame rates (focused, unfocused) the window loop paces to.
    pub fps: u32,
    pub background_fps: u32,
    /// Draw the spectrum ring around the player bar's cover.
    pub(crate) cover_visualizer: bool,
    pub(crate) ring: crate::gui::player_bar::CoverRing,
    /// Theme colors from the config, kept so a theme picked in Settings keeps the overrides.
    pub(crate) theme_config: crate::config::GuiTheme,
    /// A theme picked this frame; it takes over next frame, swiping in over a copy of this one.
    pub(crate) next_theme: Option<Theme>,
    /// When the current theme swipe started.
    pub(crate) swipe: Option<Instant>,
    pub(crate) config_path: Option<PathBuf>,
    /// Measured frames per second while animating.
    pub(crate) measured_fps: f32,
    /// The keyboard moved the selection last (show focus rings); cleared by clicking.
    pub(crate) kb_nav: bool,
    /// The window is maximized (set by the window each frame; picks the restore icon).
    pub maximized: bool,
    pub(crate) window_cmd: Option<WindowCmd>,
    /// Panel size, filter and hidden items; saved to `state_path` when changed.
    pub(crate) ui_state: UiState,
    pub(crate) state_path: Option<PathBuf>,
    /// Hidden library items are listed (dimmed) so they can be shown again.
    pub(crate) show_hidden: bool,
    pub(crate) lyrics: Arc<LyricsService>,
    pub(crate) genres: Arc<Genres>,
    /// The lyrics page stops following the song for a while after a manual scroll.
    pub(crate) lyrics_manual: Option<Instant>,
    /// The artist page shows all ten popular songs, not five.
    pub(crate) artist_more: bool,
    /// Discography tab on artist pages: 0 popular, 1 albums, 2 singles and EPs.
    pub(crate) discography: usize,
    pub(crate) lyrics_layout: Option<crate::gui::views::lyrics::LyricsLayout>,
    /// Text to show beside the hovered thing this frame: (text, left-middle anchor).
    pub(crate) tooltip: Option<(String, (f32, f32))>,
}

pub struct FrameResult {
    /// Something is moving; keep rendering at full rate.
    pub animating: bool,
    /// The pointer is over something clickable.
    pub pointer: bool,
    /// Text to put on the clipboard.
    pub copy: Option<String>,
    /// A window button was clicked, or the title bar dragged.
    pub window: Option<WindowCmd>,
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
        spotify: Option<Arc<crate::spotify::Spotify>>,
        discovery: Arc<Discovery>,
    ) -> (Self, Vec<String>) {
        let fetch = spotify.clone().map(|s| -> crate::gui::ui::UrlFetcher {
            Arc::new(move |url: &str| s.fetch_bytes(url))
        });
        let g = &config.gui;
        let (theme, mut problems) = Theme::from_config(&g.theme, g.corner_radius, g.font_size);
        let (lyrics, lyric_problems) = LyricsService::new(&config.lyrics, spotify.clone());
        problems.extend(lyric_problems);
        // Only a real config (not tests or examples) reads and writes the saved panel state.
        let state_path = config.source.as_ref().and_then(|_| UiState::default_path());
        let ui_state = state_path.as_deref().map(UiState::load).unwrap_or_default();
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
            forward: Vec::new(),
            queue_open: false,
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
            viz3d: Default::default(),
            viz_tuning: config.visualizer.tuning.clean(),
            viz_presets: config.visualizer.presets.clone(),
            viz_group: crate::config::visualizer::Group::Wave,
            viz_drag: None,
            viz_dirty: false,
            preset_name: TextField::default(),
            spotify,
            fetch,
            sp: Default::default(),
            menu: None,
            mouse: (0.0, 0.0),
            search_tab: SearchTab::All,
            discovery,
            radio: None,
            lf_key: TextField::default(),
            lf_secret: TextField::secret(),
            discord: None,
            jams: None,
            jam_invite: TextField::default(),
            load_spin: 0.0,
            anims: Anims::default(),
            shown_view: View::Albums,
            view_since: Instant::now() - Duration::from_secs(1),
            clock: PlayClock::default(),
            unmute_to: 0.8,
            fps: g.fps.clamp(10, 360),
            background_fps: g.background_fps.clamp(1, 360),
            cover_visualizer: g.cover_visualizer,
            ring: Default::default(),
            theme_config: g.theme.clone(),
            next_theme: None,
            swipe: None,
            config_path: config.source.clone(),
            measured_fps: 0.0,
            kb_nav: false,
            maximized: false,
            window_cmd: None,
            ui_state,
            state_path,
            show_hidden: false,
            lyrics,
            genres: Genres::new(),
            lyrics_manual: None,
            artist_more: false,
            discography: 0,
            lyrics_layout: None,
            tooltip: None,
        };
        (gui, problems)
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    /// Opens a view, remembering where we came from (for Backspace / mouse back).
    pub fn navigate(&mut self, view: View) {
        if view != self.view {
            self.forward.clear();
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
        self.forward.clear();
        self.view = view;
    }

    pub(crate) fn back(&mut self) -> bool {
        match self.history.pop() {
            Some(v) => {
                let from = std::mem::replace(&mut self.view, v);
                self.forward.push(from);
                true
            }
            None => false,
        }
    }

    pub(crate) fn go_forward(&mut self) {
        if let Some(v) = self.forward.pop() {
            let from = std::mem::replace(&mut self.view, v);
            self.history.push(from);
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
        self.cache.lib_search = None;
        self.cache.artist_tracks = None;
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
        if let Some(theme) = self.next_theme.take() {
            self.theme = theme;
            self.swipe = Some(now);
        }
        canvas.clear = self.theme.background;
        let images_changed = gfx.images.poll();
        self.refresh_library();
        self.poll_spotify(player);
        if let Some(m) = input.mouse {
            self.mouse = m;
        }
        if input.pressed {
            self.kb_nav = false;
        }

        let snap = player.snapshot();
        let analysis = player.analysis();
        self.clock.update(&snap, dt);
        self.poll_radio(player);
        if !self.typing() && self.menu.is_none() {
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
            copy: None,
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
        // Top bar, then three panels (library | page | queue), then the player bar.
        let full = ui.c.bounds();
        let (body, bar) = full.split_bottom(BAR_H);
        let (top, mid) = body.split_top(TOP_H);
        // The library panel shrinks to covers when collapsed (or when the window is narrow).
        let open_w = SIDEBAR_W.min(mid.w * 0.3).max(SIDEBAR_MIN);
        // Too narrow to be readable open: show the covers, like Spotify on small windows.
        let target_w = if self.ui_state.sidebar_collapsed || open_w < 220.0 {
            SIDEBAR_MIN
        } else {
            open_w
        };
        let side_w = ui.anim.to(anim::id("sidebar-w"), target_w, anim::NORMAL);
        let sidebar = Rect::new(mid.x + GAP, mid.y, side_w - GAP, mid.h);
        let queue_w = QUEUE_W.min((mid.w - side_w - 3.0 * GAP) * 0.42).max(0.0);
        let q = ui.anim.to(
            anim::id("queue-open"),
            self.queue_open as u8 as f32,
            anim::NORMAL,
        );
        let queue_space = (queue_w + GAP) * q;
        let panel_x = sidebar.right() + GAP;
        let panel = Rect::new(
            panel_x,
            mid.y,
            mid.right() - GAP - queue_space - panel_x,
            mid.h,
        );

        self.top_bar(&mut ui, top);
        ui.c.push_clip(sidebar);
        self.sidebar(&mut ui, sidebar, player);
        ui.c.pop_clip();
        if q > 0.005 {
            // Slides in from the right edge as the page makes room.
            let qr = Rect::new(panel.right() + GAP, mid.y, queue_w, mid.h);
            ui.c.push_clip(Rect::new(
                panel.right(),
                mid.y,
                mid.right() - panel.right(),
                mid.h,
            ));
            ui.c.push_fx(0.0, 0.0, q);
            self.queue_panel(&mut ui, qr, player, &snap, &analysis);
            ui.c.pop_fx();
            ui.c.pop_clip();
        }
        ui.c.rect(panel, theme.surface, 12.0);
        // A faint wash of the accent color at the top of the page.
        ui.c.gradient(
            Rect::new(panel.x, panel.y, panel.w, 220.0_f32.min(panel.h)),
            theme.accent.alpha(0.035),
            theme.accent.alpha(0.0),
            12.0,
        );
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
        self.draw_tooltip(&mut ui);
        self.draw_toast(&mut ui, panel);

        let mut scrolling = false;
        for l in self.lists.values_mut() {
            scrolling |= l.step(dt);
        }
        let pointer = ui.pointer;
        let copy = ui.copy.take();
        let swiping = self.swipe_frame(ui.c);
        let animating = snap.state == PlaybackState::Playing
            || ui.anim.moving()
            || scrolling
            || images_changed
            || ui.images.is_loading()
            || swiping
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
        FrameResult {
            animating,
            pointer,
            copy,
            window: self.window_cmd.take(),
        }
    }

    /// A text field has the keyboard (global shortcuts are off).
    fn typing(&self) -> bool {
        self.search.focused
            || self.lf_key.focused
            || self.lf_secret.focused
            || self.preset_name.focused
            || self.jam_invite.focused
    }

    /// Starts building a radio from `seed`; it replaces what's coming up when ready.
    pub(crate) fn start_radio(&mut self, seed: Track) {
        self.toast(format!("Finding songs like “{}”…", seed.title));
        let pending = self.discovery.radio(&seed, 40);
        self.radio = Some((seed, pending));
    }

    fn poll_radio(&mut self, player: &Player) {
        let Some(result) = self.radio.as_ref().and_then(|r| r.1.poll()) else {
            return;
        };
        let (seed, _) = self.radio.take().expect("checked");
        let tracks = match result {
            Ok(t) if !t.is_empty() => t,
            Ok(_) => return self.toast("Couldn't find similar songs"),
            Err(e) => return self.toast(format!("Radio: {e:#}")),
        };
        let n = tracks.len();
        crate::discovery::queue_radio(&player.handle(), &seed, tracks);
        self.toast(format!("Radio: {} like “{}”", count(n, "song"), seed.title));
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
                Key::Char('4') => self.queue_open = !self.queue_open,
                Key::Char('5') => self.switch(View::Files),
                Key::Char('6') => self.switch(View::Visualizer),
                Key::Char('L') => self.toggle_lyrics(),
                Key::Char('o') => self.reveal_playing(player),
                _ => self.view_key(*k, player),
            }
        }
    }

    fn reveal_playing(&mut self, player: &Player) {
        if player.queue().read().current_index().is_some() {
            self.queue_open = true;
            self.list("queue").target = 0.0;
        }
    }

    /// Opens the album page for a track (clicking the now-playing cover).
    pub(crate) fn open_album_of(&mut self, track: &Track) {
        if let (TrackLocator::Spotify { .. }, Some(id)) = (&track.locator, &track.album_id) {
            return self.navigate(View::SpotifyAlbum {
                id: id.clone(),
                name: track.album.clone().unwrap_or_default(),
            });
        }
        if let Some(album) = &track.album {
            self.navigate(View::Album {
                name: album.clone(),
                artist: track.primary_artist().map(str::to_owned),
            });
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
        ui.c.glow(r, ui.theme.shadow(0.45 * alpha), 21.0, 18.0);
        ui.c.rect(r, t.surface_hover.alpha(alpha), 21.0);
        ui.c.outline(r, t.line().alpha(alpha), 21.0, 1.0);
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

impl Gui {
    /// Opens the lyrics page, or goes back from it.
    pub(crate) fn toggle_lyrics(&mut self) {
        if self.view == View::Lyrics {
            if !self.back() {
                self.switch(View::Albums);
            }
        } else {
            self.lyrics_manual = None;
            self.navigate(View::Lyrics);
        }
    }

    pub(crate) fn save_ui_state(&self) {
        if let Some(p) = &self.state_path {
            self.ui_state.save(p);
        }
    }

    /// Opens an artist: the Spotify profile when the id is known, else the library page.
    pub(crate) fn open_artist(&mut self, name: &str, spotify_id: Option<&str>) {
        match spotify_id {
            Some(id) if self.spotify_connected() => {
                self.artist_more = false;
                self.discography = 0;
                self.navigate(View::SpotifyArtist {
                    id: id.to_owned(),
                    name: name.to_owned(),
                });
            }
            _ => self.navigate(View::Artist(name.to_owned())),
        }
    }
}

impl Gui {
    /// A label beside something hovered (anchored at its left-middle point), above everything.
    fn draw_tooltip(&mut self, ui: &mut Ui) {
        let Some((text, (x, y))) = self.tooltip.take() else {
            return;
        };
        let t = ui.theme;
        let w = ui.measure(&text, 13.0, Weight::Medium) + 24.0;
        let tip = Rect::new(x, y - 16.0, w, 32.0);
        ui.c.set_layer(1);
        ui.c.glow(tip, ui.theme.shadow(0.5), 6.0, 10.0);
        ui.c.rect(tip, t.surface_hover, 6.0);
        ui.label_center(&text, tip, 13.0, Weight::Medium, t.fg);
        ui.c.set_layer(0);
    }
}

/// How long a theme swipe takes to cross the window.
const SWIPE: Duration = Duration::from_millis(950);

impl Gui {
    /// Switches to the built-in theme `key` (keeping colors overridden in the config) with a
    /// swipe, and saves the choice.
    pub(crate) fn set_theme(&mut self, key: &str) {
        if self.theme.key == key && self.next_theme.is_none() {
            return;
        }
        let mut cfg = self.theme_config.clone();
        cfg.name = key.to_owned();
        let (theme, _) = Theme::from_config(&cfg, self.theme.radius, self.theme.font_size);
        self.theme_config = cfg;
        self.next_theme = Some(theme);
        self.save_str("gui.theme.name", key);
    }

    /// Asks the renderer for this frame's part of a theme swipe; true while one is running.
    fn swipe_frame(&mut self, canvas: &mut Canvas) -> bool {
        if self.next_theme.is_some() {
            // Keep this (old-theme) frame to swipe away over the next ones.
            canvas.snapshot = true;
            return true;
        }
        let Some(start) = self.swipe else {
            return false;
        };
        let t = start.elapsed().as_secs_f32() / SWIPE.as_secs_f32();
        if t >= 1.0 {
            self.swipe = None;
            return false;
        }
        // Slow out of the corner, fast across the middle, settling at the far side.
        let p = if t < 0.5 {
            4.0 * t * t * t
        } else {
            1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
        };
        canvas.wipe = Some(crate::gui::render::wipe::WipeFrame {
            progress: p,
            glow: self.theme.accent,
        });
        true
    }
}
