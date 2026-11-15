//! TUI state and behavior. Rendering lives in `ui.rs`; this file decides what keys *do*.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::config::Config;
use crate::discovery::Discovery;
use crate::library::Library;
use crate::model::Track;
use crate::pending::Pending;
use crate::player::browse::{Browser, EntryKind};
use crate::player::{Player, PlayerCommand as Cmd, PlayerEvent};
use crate::tui::action::Action;
use crate::tui::command::{self, Adjust, SeekArg, View};
use crate::tui::input::{InputResult, LineInput};
use crate::tui::keymap::{Chord, KeyMode, Keymap, Outcome};
use crate::tui::library::LibState;
use crate::tui::playlists::PlState;
use crate::tui::theme::Theme;
use crate::tui::visualizer::Visualizer;

/// How long to wait for the rest of an ambiguous key sequence (vim's `timeoutlen`).
const KEY_TIMEOUT: Duration = Duration::from_millis(1000);
const MESSAGE_TTL: Duration = Duration::from_secs(5);
const SCROLLOFF: usize = 3;

pub enum Mode {
    Normal,
    Visual { anchor: usize },
    Command(LineInput),
    Search(LineInput),
}

pub struct Message {
    pub text: String,
    pub error: bool,
    at: Instant,
}

/// Cursor + scroll position of a list.
#[derive(Debug, Default, Clone)]
pub struct ListView {
    pub selected: usize,
    pub offset: usize,
    /// Visible rows, updated by the renderer.
    pub height: usize,
}

impl ListView {
    fn set(&mut self, index: usize, len: usize) {
        self.selected = if len == 0 { 0 } else { index.min(len - 1) };
    }

    fn move_by(&mut self, delta: isize, len: usize) {
        let target = (self.selected as isize + delta).max(0) as usize;
        self.set(target, len);
    }

    /// Keeps the cursor on screen with a vim-like scrolloff margin.
    pub fn scroll_into_view(&mut self, len: usize) {
        self.set(self.selected, len);
        let h = self.height.max(1);
        let so = SCROLLOFF.min(h.saturating_sub(1) / 2);
        if self.selected < self.offset + so {
            self.offset = self.selected.saturating_sub(so);
        }
        if self.selected + so >= self.offset + h {
            self.offset = self.selected + so + 1 - h;
        }
        self.offset = self.offset.min(len.saturating_sub(h));
    }
}

pub struct App {
    pub player: Player,
    pub library: Arc<Library>,
    pub theme: Theme,
    pub keymap: Keymap,
    pub view: View,
    pub mode: Mode,
    pub queue_view: ListView,
    pub browse_view: ListView,
    pub browser: Browser,
    pub lib: LibState,
    pub pl: PlState,
    pub discovery: Arc<Discovery>,
    /// A song radio being put together: (seed, suggestions).
    radio: Option<(Track, Pending<Vec<Track>>)>,
    pub vis: Visualizer,
    pub message: Option<Message>,
    /// Help overlay scroll offset when open.
    pub help: Option<usize>,
    pub quit: bool,
    music_dirs: Vec<PathBuf>,
    seek_step: i64,
    volume_step: f32,
    register: Vec<Track>,
    last_search: Option<String>,
    search_origin: usize,
    cmd_history: Vec<String>,
    search_history: Vec<String>,
    last_key: Instant,
}

impl App {
    pub fn new(
        player: Player,
        library: Arc<Library>,
        spotify: Option<Arc<crate::spotify::Spotify>>,
        discovery: Arc<Discovery>,
        config: &Config,
    ) -> Self {
        let (theme, mut problems) = Theme::from_config(&config.tui.theme);
        let (keymap, key_problems) = Keymap::default().with_overrides(&config.tui.keys);
        problems.extend(key_problems);
        let music_dirs = config.library.music_dirs.clone();
        let start = music_dirs
            .first()
            .cloned()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let mut app = Self {
            player,
            library,
            theme,
            keymap,
            view: View::Queue,
            mode: Mode::Normal,
            queue_view: ListView::default(),
            browse_view: ListView::default(),
            browser: Browser::new(start),
            lib: LibState::default(),
            pl: PlState::new(spotify),
            discovery,
            radio: None,
            vis: Visualizer::default(),
            message: None,
            help: None,
            quit: false,
            music_dirs,
            seek_step: i64::from(config.tui.seek_step.max(1)) * 1000,
            volume_step: f32::from(config.tui.volume_step.max(1)) / 100.0,
            register: Vec::new(),
            last_search: None,
            search_origin: 0,
            cmd_history: Vec::new(),
            search_history: Vec::new(),
            last_key: Instant::now(),
        };
        if !problems.is_empty() {
            app.error(format!("config: {}", problems.join("; ")));
        }
        app
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            error: false,
            at: Instant::now(),
        });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            error: true,
            at: Instant::now(),
        });
    }

    /// Periodic work: player events, key timeouts, message expiry, visualizer physics.
    pub fn tick(&mut self) {
        self.lib.refresh(&self.library);
        for (msg, is_err) in self.pl.tick(&self.library) {
            if is_err {
                self.error(msg);
            } else {
                self.info(msg);
            }
        }
        if let Some(result) = self.radio.as_ref().and_then(|r| r.1.poll()) {
            let (seed, _) = self.radio.take().expect("checked");
            match result {
                Ok(t) if !t.is_empty() => {
                    let n = t.len();
                    crate::discovery::queue_radio(&self.player.handle(), &seed, t);
                    self.info(format!("radio: {n} songs like “{}”", seed.title));
                }
                Ok(_) => self.error("couldn't find similar songs"),
                Err(e) => self.error(format!("radio: {e:#}")),
            }
        }
        while let Ok(ev) = self.player.events().try_recv() {
            if let PlayerEvent::Error(e) = ev {
                self.error(e);
            }
        }
        if self.keymap.is_pending() && self.last_key.elapsed() > KEY_TIMEOUT {
            let mode = self.key_mode();
            if let Some(Outcome::Action { action, count }) = self.keymap.timeout(mode) {
                self.perform(action, count);
            }
        }
        if self
            .message
            .as_ref()
            .is_some_and(|m| !m.error && m.at.elapsed() > MESSAGE_TTL)
        {
            self.message = None;
        }
    }

    fn key_mode(&self) -> KeyMode {
        match self.mode {
            Mode::Visual { .. } => KeyMode::Visual,
            _ => KeyMode::Normal,
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        self.last_key = Instant::now();
        if let Some(scroll) = self.help.as_mut() {
            match Chord::from(key).code {
                KeyCode::Char('j') | KeyCode::Down => *scroll += 1,
                KeyCode::Char('k') | KeyCode::Up => *scroll = scroll.saturating_sub(1),
                KeyCode::Char('d') => *scroll += 10,
                KeyCode::Char('u') => *scroll = scroll.saturating_sub(10),
                _ => self.help = None,
            }
            return;
        }
        match &mut self.mode {
            Mode::Command(input) => match input.handle(key, &self.cmd_history) {
                InputResult::Submit(line) => {
                    self.mode = Mode::Normal;
                    if !line.trim().is_empty() {
                        self.cmd_history.retain(|h| *h != line);
                        self.cmd_history.push(line.clone());
                        self.run_command(&line);
                    }
                }
                InputResult::Cancel => self.mode = Mode::Normal,
                InputResult::Edited | InputResult::Ignored => {}
            },
            Mode::Search(input) => match input.handle(key, &self.search_history) {
                InputResult::Submit(q) => {
                    self.mode = Mode::Normal;
                    if q.is_empty() {
                        return;
                    }
                    self.search_history.retain(|h| *h != q);
                    self.search_history.push(q.clone());
                    if self.find(&q, self.search_origin, true, true).is_none() {
                        self.error(format!("pattern not found: {q}"));
                    }
                    self.last_search = Some(q);
                }
                InputResult::Cancel => {
                    self.mode = Mode::Normal;
                    let origin = self.search_origin;
                    self.list_mut().selected = origin;
                }
                InputResult::Edited => {
                    // Incremental search from where the search started.
                    let q = input.text().to_owned();
                    let origin = self.search_origin;
                    if q.is_empty() || self.find(&q, origin, true, true).is_none() {
                        self.list_mut().selected = origin;
                    }
                }
                InputResult::Ignored => {}
            },
            Mode::Normal | Mode::Visual { .. } => {
                let mode = self.key_mode();
                if let Outcome::Action { action, count } = self.keymap.feed(mode, key.into()) {
                    // Like vim: the next action clears the previous message.
                    self.message = None;
                    self.perform(action, count);
                }
            }
        }
    }

    /// Paths pasted into the terminal (e.g. files dragged onto it) are added to the queue.
    pub fn on_paste(&mut self, text: &str) {
        if let Mode::Command(input) | Mode::Search(input) = &mut self.mode {
            for c in text.chars().filter(|c| !c.is_control()) {
                input.handle(KeyEvent::from(KeyCode::Char(c)), &[]);
            }
            return;
        }
        let paths: Vec<PathBuf> = split_pasted_paths(text)
            .into_iter()
            .filter(|p| p.exists())
            .collect();
        let tracks = self.library.tracks_for_paths(&paths);
        if tracks.is_empty() {
            self.error("pasted text contains no playable files");
        } else {
            self.info(format!("added {} tracks", tracks.len()));
            self.player.send(Cmd::Enqueue(tracks));
        }
    }

    pub fn list_len(&self) -> usize {
        match self.view {
            View::Queue => self.player.queue().read().len(),
            View::Albums => match &self.lib.open {
                Some((_, tracks)) => tracks.len(),
                None => self.lib.albums.len(),
            },
            View::Songs => self.lib.songs.len(),
            View::Playlists => match &self.pl.open {
                Some((_, t)) => t.len(),
                None => self.pl.entries().len(),
            },
            View::Browse => self.browser.entries().len(),
            View::Visualizer => 0,
        }
    }

    fn list_mut(&mut self) -> &mut ListView {
        match self.view {
            View::Browse => &mut self.browse_view,
            View::Albums if self.lib.open.is_some() => &mut self.lib.album_view,
            View::Albums => &mut self.lib.albums_view,
            View::Songs => &mut self.lib.songs_view,
            View::Playlists if self.pl.open.is_some() => &mut self.pl.tracks_view,
            View::Playlists => &mut self.pl.root_view,
            _ => &mut self.queue_view,
        }
    }

    pub fn list(&self) -> &ListView {
        match self.view {
            View::Browse => &self.browse_view,
            View::Albums if self.lib.open.is_some() => &self.lib.album_view,
            View::Albums => &self.lib.albums_view,
            View::Songs => &self.lib.songs_view,
            View::Playlists if self.pl.open.is_some() => &self.pl.tracks_view,
            View::Playlists => &self.pl.root_view,
            _ => &self.queue_view,
        }
    }

    /// The rows an operator applies to: the visual selection, or `count` rows from the cursor.
    pub fn selection(&self, count: usize) -> Range<usize> {
        let len = self.list_len();
        let sel = self.list().selected;
        let r = match self.mode {
            Mode::Visual { anchor } => anchor.min(sel)..anchor.max(sel) + 1,
            _ => sel..sel + count.max(1),
        };
        r.start.min(len)..r.end.min(len)
    }

    fn set_view(&mut self, view: View) {
        self.view = view;
        self.mode = Mode::Normal;
        self.keymap.reset();
    }

    pub fn perform(&mut self, action: Action, count: Option<u32>) {
        use Action::*;
        let n = count.unwrap_or(1).max(1) as usize;
        let len = self.list_len();
        let page = self.list().height.max(2);
        match action {
            MoveDown => self.list_mut().move_by(n as isize, len),
            MoveUp => self.list_mut().move_by(-(n as isize), len),
            Top => self
                .list_mut()
                .set(count.map_or(0, |c| c as usize - 1), len),
            Bottom => self
                .list_mut()
                .set(count.map_or(usize::MAX, |c| c as usize - 1), len),
            HalfPageDown => self.list_mut().move_by((page / 2 * n) as isize, len),
            HalfPageUp => self.list_mut().move_by(-((page / 2 * n) as isize), len),
            PageDown => self.list_mut().move_by((page * n) as isize, len),
            PageUp => self.list_mut().move_by(-((page * n) as isize), len),
            GotoPlaying => {
                let current = self.player.queue().read().current_index();
                match current {
                    Some(i) => {
                        self.set_view(View::Queue);
                        self.queue_view.set(i, len.max(i + 1));
                    }
                    None => self.info("nothing is playing"),
                }
            }

            Activate => self.activate(),
            Parent => {
                if self.view == View::Albums {
                    self.close_album();
                } else if self.view == View::Playlists {
                    self.pl.open = None;
                } else if self.view == View::Browse {
                    match self.browser.parent() {
                        Some(i) => self.browse_view.set(i, self.browser.entries().len()),
                        None => self.info("already at the top"),
                    }
                }
            }
            Open => {
                if self.view == View::Browse {
                    self.activate();
                }
            }
            Append | InsertNext => {
                let tracks = match self.view {
                    View::Browse => self.browser_tracks(n),
                    View::Albums | View::Songs | View::Playlists => self.lib_tracks(n),
                    _ => {
                        self.info("append/insert work in albums, songs and the browser");
                        return;
                    }
                };
                let was_visual = matches!(self.mode, Mode::Visual { .. });
                self.mode = Mode::Normal;
                if tracks.is_empty() {
                    self.error("no playable files there");
                    return;
                }
                let what = if action == Append {
                    "added"
                } else {
                    "playing next:"
                };
                self.info(format!(
                    "{what} {} track{}",
                    tracks.len(),
                    plural(tracks.len())
                ));
                self.player.send(if action == Append {
                    Cmd::Enqueue(tracks)
                } else {
                    Cmd::PlayNext(tracks)
                });
                if !was_visual {
                    // Like ncmpcpp: step past what was just added, ready for the next `a`.
                    self.list_mut().move_by(n as isize, len);
                }
            }
            Delete => {
                if self.view != View::Queue {
                    return;
                }
                let range = self.selection(n);
                let (ids, tracks): (Vec<_>, Vec<_>) = {
                    let q = self.player.queue().read();
                    q.entries()[range.clone()]
                        .iter()
                        .map(|e| (e.id, (*e.track).clone()))
                        .unzip()
                };
                if ids.is_empty() {
                    return;
                }
                let count = ids.len();
                self.register = tracks; // vim: deleting also yanks
                self.player.send(Cmd::Remove(ids));
                self.mode = Mode::Normal;
                self.queue_view.set(range.start, len - count);
                if count > 2 {
                    self.info(format!("{count} fewer tracks"));
                }
            }
            Yank => {
                let tracks = match self.view {
                    View::Queue => {
                        let q = self.player.queue().read();
                        let r = self.selection(n);
                        q.entries()[r].iter().map(|e| (*e.track).clone()).collect()
                    }
                    View::Browse => self.browser_tracks(n),
                    View::Albums | View::Songs | View::Playlists => self.lib_tracks(n),
                    View::Visualizer => Vec::new(),
                };
                if let Mode::Visual { anchor } = self.mode {
                    let start = anchor.min(self.list().selected);
                    self.list_mut().selected = start;
                }
                self.mode = Mode::Normal;
                self.info(format!(
                    "{} track{} yanked",
                    tracks.len(),
                    plural(tracks.len())
                ));
                self.register = tracks;
            }
            PasteAfter | PasteBefore => {
                if self.view != View::Queue {
                    self.info("paste works in the queue (gq)");
                    return;
                }
                if self.register.is_empty() {
                    self.error("nothing yanked");
                    return;
                }
                let at = match (action, len) {
                    (_, 0) => 0,
                    (PasteAfter, _) => self.queue_view.selected + 1,
                    _ => self.queue_view.selected,
                };
                let tracks: Vec<Track> =
                    (0..n).flat_map(|_| self.register.iter().cloned()).collect();
                let added = tracks.len();
                self.player.send(Cmd::Insert { index: at, tracks });
                self.queue_view.set(at, len + added);
            }
            MoveItemDown | MoveItemUp => {
                if self.view != View::Queue || len == 0 {
                    return;
                }
                let from = self.queue_view.selected;
                let to = if action == MoveItemDown {
                    (from + n).min(len - 1)
                } else {
                    from.saturating_sub(n)
                };
                self.player.send(Cmd::Move { from, to });
                self.queue_view.set(to, len);
            }
            VisualMode => match self.mode {
                Mode::Visual { .. } => self.mode = Mode::Normal,
                _ if self.view != View::Visualizer && len > 0 => {
                    self.mode = Mode::Visual {
                        anchor: self.list().selected,
                    }
                }
                _ => {}
            },
            Escape => {
                self.mode = Mode::Normal;
                self.message = None;
            }

            TogglePause => self.player.send(Cmd::TogglePause),
            Stop => self.player.send(Cmd::Stop),
            NextTrack => (0..n).for_each(|_| self.player.send(Cmd::Next)),
            PrevTrack => (0..n).for_each(|_| self.player.send(Cmd::Prev)),
            SeekForward => self.player.send(Cmd::SeekBy(self.seek_step * n as i64)),
            SeekBackward => self.player.send(Cmd::SeekBy(-self.seek_step * n as i64)),
            SeekForwardBig => self.player.send(Cmd::SeekBy(self.seek_step * 6 * n as i64)),
            SeekBackwardBig => self
                .player
                .send(Cmd::SeekBy(-self.seek_step * 6 * n as i64)),
            SeekStart => self.player.send(Cmd::Seek(Duration::ZERO)),
            VolumeUp => self.player.send(Cmd::VolumeBy(self.volume_step * n as f32)),
            VolumeDown => self
                .player
                .send(Cmd::VolumeBy(-self.volume_step * n as f32)),
            ToggleShuffle => self.player.send(Cmd::ToggleShuffle),
            CycleRepeat => self.player.send(Cmd::CycleRepeat),

            CommandMode => self.mode = Mode::Command(LineInput::default()),
            Search => {
                if self.view == View::Visualizer {
                    return;
                }
                self.search_origin = self.list().selected;
                self.mode = Mode::Search(LineInput::default());
            }
            SearchNext | SearchPrev => {
                let Some(q) = self.last_search.clone() else {
                    self.error("no previous search");
                    return;
                };
                let forward = action == SearchNext;
                for _ in 0..n {
                    let from = self.list().selected;
                    let start = if forward {
                        from + 1
                    } else {
                        from + len.max(1) - 1
                    };
                    if self.find(&q, start % len.max(1), forward, false).is_none() {
                        self.error(format!("pattern not found: {q}"));
                        break;
                    }
                }
            }
            NextView | PrevView => {
                let i = View::ALL.iter().position(|v| *v == self.view).unwrap_or(0);
                let step = if action == NextView {
                    1
                } else {
                    View::ALL.len() - 1
                };
                self.set_view(View::ALL[(i + step) % View::ALL.len()]);
            }
            ViewQueue => self.set_view(View::Queue),
            ViewAlbums => {
                if self.view == View::Albums {
                    self.close_album();
                }
                self.set_view(View::Albums);
            }
            ViewSongs => self.set_view(View::Songs),
            ViewPlaylists => {
                if self.view == View::Playlists {
                    self.pl.open = None;
                }
                self.set_view(View::Playlists);
            }
            ViewBrowse => self.set_view(View::Browse),
            ViewVisualizer => self.set_view(View::Visualizer),
            CycleVisualizer => {
                self.vis.cycle();
                self.info(format!("visualizer: {}", self.vis.style.name()));
            }
            Help => self.help = Some(0),
            Quit => self.quit = true,
        }
    }

    fn activate(&mut self) {
        match self.view {
            View::Queue => {
                let id = {
                    let q = self.player.queue().read();
                    q.entries().get(self.queue_view.selected).map(|e| e.id)
                };
                if let Some(id) = id {
                    self.player.send(Cmd::PlayEntry(id));
                }
            }
            View::Browse => {
                let Some(entry) = self
                    .browser
                    .entries()
                    .get(self.browse_view.selected)
                    .cloned()
                else {
                    return;
                };
                match entry.kind {
                    EntryKind::Dir => match self.browser.open(&entry.path) {
                        Ok(()) => {
                            self.browse_view = ListView {
                                height: self.browse_view.height,
                                ..Default::default()
                            }
                        }
                        Err(e) => self.error(e),
                    },
                    EntryKind::Audio => {
                        self.info(format!("playing {}", entry.name));
                        self.player
                            .send(Cmd::EnqueueAndPlay(vec![Track::from_path(&entry.path)]));
                    }
                }
            }
            View::Albums => {
                let sel = self.lib.album_view.selected;
                match &self.lib.open {
                    Some((_, tracks)) if !tracks.is_empty() => {
                        let tracks = tracks.clone();
                        self.player.send(Cmd::PlayTracks { tracks, start: sel });
                    }
                    Some(_) => {}
                    None => {
                        let i = self.lib.albums_view.selected;
                        self.lib.open_album(&self.library, i);
                    }
                }
            }
            View::Songs => {
                if !self.lib.songs.is_empty() {
                    let tracks = self.lib.songs.clone();
                    let start = self.lib.songs_view.selected;
                    self.player.send(Cmd::PlayTracks { tracks, start });
                }
            }
            View::Playlists => {
                let sel = self.pl.tracks_view.selected;
                match &self.pl.open {
                    Some((_, tracks)) if !tracks.is_empty() => {
                        let tracks = tracks.clone();
                        self.player.send(Cmd::PlayTracks { tracks, start: sel });
                    }
                    Some(_) => {}
                    None => {
                        let i = self.pl.root_view.selected;
                        self.pl.open_entry(i);
                    }
                }
            }
            View::Visualizer => self.player.send(Cmd::TogglePause),
        }
    }

    fn close_album(&mut self) {
        if let Some((al, _)) = self.lib.open.take()
            && let Some(i) = self.lib.albums.iter().position(|a| *a == al)
        {
            let len = self.lib.albums.len();
            self.lib.albums_view.set(i, len);
        }
    }

    /// Tracks for the selected rows of the albums/songs views (albums expand to their tracks).
    fn lib_tracks(&self, count: usize) -> Vec<Track> {
        let range = self.selection(count);
        match (self.view, &self.lib.open) {
            (View::Albums, Some((_, tracks))) => tracks[range].to_vec(),
            (View::Albums, None) => self.lib.albums[range]
                .iter()
                .flat_map(|al| self.library.album_tracks(&al.name, al.artist.as_deref()))
                .collect(),
            (View::Songs, _) => self.lib.songs[range].to_vec(),
            (View::Playlists, _) => self
                .pl
                .open
                .as_ref()
                .map(|(_, t)| t[range].to_vec())
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// Tracks under the selected browser entries (folders expand recursively).
    fn browser_tracks(&self, count: usize) -> Vec<Track> {
        let range = self.selection(count);
        let paths: Vec<PathBuf> = self.browser.entries()[range]
            .iter()
            .map(|e| e.path.clone())
            .collect();
        self.library.tracks_for_paths(&paths)
    }

    /// Text that search matches against, per row of the current view.
    fn row_text(&self, index: usize) -> Option<String> {
        match self.view {
            View::Queue => {
                let q = self.player.queue().read();
                let t = &q.entries().get(index)?.track;
                let file = match &t.locator {
                    crate::model::TrackLocator::Local(p) => p.to_string_lossy().into_owned(),
                    _ => String::new(),
                };
                Some(format!(
                    "{} {} {} {file}",
                    t.title,
                    t.artist_line(),
                    t.album.as_deref().unwrap_or("")
                ))
            }
            View::Browse => self.browser.entries().get(index).map(|e| e.name.clone()),
            View::Albums => match &self.lib.open {
                Some((_, tracks)) => tracks.get(index).map(track_text),
                None => self
                    .lib
                    .albums
                    .get(index)
                    .map(|al| format!("{} {}", al.name, al.artist.as_deref().unwrap_or(""))),
            },
            View::Songs => self.lib.songs.get(index).map(track_text),
            View::Playlists => match &self.pl.open {
                Some((_, t)) => t.get(index).map(track_text),
                None => self.pl.entries().get(index).map(|e| e.title()),
            },
            View::Visualizer => None,
        }
    }

    /// Moves the cursor to the next row matching `query` starting at `start` (inclusive),
    /// wrapping around. Smartcase: case-sensitive only if the query has uppercase letters.
    fn find(&mut self, query: &str, start: usize, forward: bool, quiet: bool) -> Option<usize> {
        let len = self.list_len();
        if len == 0 {
            return None;
        }
        let sensitive = query.chars().any(char::is_uppercase);
        let needle = if sensitive {
            query.to_owned()
        } else {
            query.to_lowercase()
        };
        for i in 0..len {
            let idx = if forward {
                (start + i) % len
            } else {
                (start + len - i) % len
            };
            let Some(text) = self.row_text(idx) else {
                continue;
            };
            let hay = if sensitive { text } else { text.to_lowercase() };
            if hay.contains(&needle) {
                let wrapped = if forward { idx < start } else { idx > start };
                if wrapped && !quiet {
                    self.info(if forward {
                        "search hit BOTTOM, continuing at TOP"
                    } else {
                        "search hit TOP, continuing at BOTTOM"
                    });
                }
                self.list_mut().selected = idx;
                return Some(idx);
            }
        }
        None
    }

    pub fn run_command(&mut self, line: &str) {
        let cmd = match command::parse(line) {
            Ok(c) => c,
            Err(e) => return self.error(e),
        };
        use command::Cmd as C;
        match cmd {
            C::Quit => self.quit = true,
            C::Run(a) => self.perform(a, None),
            C::Add(path) => {
                let tracks = self.library.tracks_for_paths(&[&path]);
                if tracks.is_empty() {
                    self.error(format!("no playable files at {}", path.display()));
                } else {
                    self.info(format!(
                        "added {} track{}",
                        tracks.len(),
                        plural(tracks.len())
                    ));
                    self.player.send(Cmd::Enqueue(tracks));
                }
            }
            C::Browse(path) => {
                let Some(dir) = path.or_else(|| self.music_dirs.first().cloned()) else {
                    return self.error("no music folder configured; use :e <path>");
                };
                match self.browser.open(&dir) {
                    Ok(()) => {
                        self.set_view(View::Browse);
                        self.browse_view.selected = 0;
                    }
                    Err(e) => self.error(e),
                }
            }
            C::Clear => {
                self.player.send(Cmd::Clear);
                self.queue_view.selected = 0;
            }
            C::Volume(Adjust::Set(v)) => self.player.send(Cmd::SetVolume(v)),
            C::Volume(Adjust::By(v)) => self.player.send(Cmd::VolumeBy(v)),
            C::Seek(SeekArg::To(d)) => self.player.send(Cmd::Seek(d)),
            C::Seek(SeekArg::By(ms)) => self.player.send(Cmd::SeekBy(ms)),
            C::Shuffle(None) => self.player.send(Cmd::ToggleShuffle),
            C::Shuffle(Some(on)) => self.player.send(Cmd::SetShuffle(on)),
            C::Repeat(None) => self.player.send(Cmd::CycleRepeat),
            C::Repeat(Some(m)) => self.player.send(Cmd::SetRepeat(m)),
            C::Goto(n) => {
                let len = self.list_len();
                self.list_mut().set(n.saturating_sub(1), len);
            }
            C::Map { mode, keys, action } => match self.keymap.bind_str(mode, &keys, &action) {
                Ok(()) => self.info(format!("mapped {keys} → {action}")),
                Err(e) => self.error(e),
            },
            C::Unmap { mode, keys } => match self.keymap.bind_str(mode, &keys, "none") {
                Ok(()) => self.info(format!("unmapped {keys}")),
                Err(e) => self.error(e),
            },
            C::View(v) => self.set_view(v),
            C::Help => self.help = Some(0),
            C::Spotify(cmd) => {
                let Some(sp) = self.pl.spotify.clone() else {
                    return self.error("Spotify isn't available");
                };
                match cmd {
                    command::SpotifyCmd::Login => {
                        sp.sign_in();
                        self.info("finish signing in to Spotify in your browser");
                    }
                    command::SpotifyCmd::Logout => {
                        sp.sign_out();
                        self.info("signed out of Spotify");
                    }
                    command::SpotifyCmd::Status => {
                        let s = self.pl.status_line();
                        self.info(if s.is_empty() {
                            "spotify off".to_string()
                        } else {
                            s
                        });
                    }
                }
            }
            C::SpotifySearch(q) => match self.pl.search(&q) {
                Ok(()) => {
                    self.info(format!("searching Spotify for “{q}”…"));
                    self.set_view(View::Playlists);
                }
                Err(e) => self.error(e),
            },
            C::AddTo(name) => {
                let tracks = self.selected_tracks();
                if tracks.is_empty() {
                    return self.error("select some tracks first");
                }
                self.mode = Mode::Normal;
                match self.pl.add_to(&self.library, &name, tracks) {
                    Ok(m) => self.info(m),
                    Err(e) => self.error(e),
                }
            }
            C::Radio => {
                let seed = self
                    .selected_tracks()
                    .into_iter()
                    .next()
                    .or_else(|| self.player.snapshot().current.map(|e| (*e.track).clone()));
                let Some(seed) = seed else {
                    return self.error("nothing to start a radio from");
                };
                self.mode = Mode::Normal;
                self.info(format!("finding songs like “{}”…", seed.title));
                let pending = self.discovery.radio(&seed, 40);
                self.radio = Some((seed, pending));
            }
            C::Autoplay(set) => {
                let on = set.unwrap_or(!self.discovery.autoplay());
                self.discovery.set_autoplay(on);
                self.info(if on { "autoplay on" } else { "autoplay off" });
            }
            C::Lastfm(cmd) => {
                use crate::lastfm::Status as L;
                let Some(lf) = self.discovery.lastfm().cloned() else {
                    return self.error("Last.fm isn't available");
                };
                match cmd {
                    command::SpotifyCmd::Login if lf.status() == L::NoApiKey => self.error(
                        "set [lastfm] api_key and api_secret in config.toml first (last.fm/api/account/create)",
                    ),
                    command::SpotifyCmd::Login => {
                        lf.sign_in();
                        self.info("allow Khoostic in the Last.fm tab that opened");
                    }
                    command::SpotifyCmd::Logout => {
                        lf.sign_out();
                        self.info("signed out of Last.fm");
                    }
                    command::SpotifyCmd::Status => self.info(match lf.status() {
                        L::Connected(n) => format!("scrobbling as {n} ({} queued)", lf.queued()),
                        L::SigningIn => "last.fm: finish in the browser".into(),
                        L::NoApiKey => "last.fm: no API key in config".into(),
                        L::Disconnected => "last.fm: not connected (:lastfm login)".into(),
                        L::Failed(e) => format!("last.fm: {e}"),
                    }),
                }
            }
            C::NewPlaylist(name) => match self.library.create_playlist(&name, &[]) {
                Ok(_) => self.info(format!("created “{name}”: add tracks with :addto {name}")),
                Err(e) => self.error(e.to_string()),
            },
            C::Config { init } => match Config::default_path() {
                None => self.error("no config directory on this system"),
                Some(p) if init && !p.exists() => match Config::write_default(&p) {
                    Ok(()) => self.info(format!("wrote {}", p.display())),
                    Err(e) => self.error(format!("{}: {e}", p.display())),
                },
                Some(p) => {
                    let state = if p.exists() {
                        ""
                    } else {
                        " (doesn't exist yet: :config init)"
                    };
                    self.info(format!("{}{state}", p.display()))
                }
            },
        }
    }

    /// Whether the screen changes on its own (so the loop should keep redrawing).
    pub fn animating(&self) -> bool {
        let s = self.player.snapshot();
        let a = self.player.analysis();
        s.state == crate::audio::PlaybackState::Playing
            || a.bands.iter().any(|&b| b > 0.0)
            || self.pl.busy()
            || self.radio.is_some()
    }

    /// Tracks under the cursor / visual selection in any track view (for :addto).
    fn selected_tracks(&self) -> Vec<Track> {
        match self.view {
            View::Queue => {
                let q = self.player.queue().read();
                let r = self.selection(1);
                q.entries()[r].iter().map(|e| (*e.track).clone()).collect()
            }
            View::Albums | View::Songs | View::Playlists => self.lib_tracks(1),
            View::Browse => self.browser_tracks(1),
            View::Visualizer => Vec::new(),
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Terminals paste dropped files as space/newline separated, sometimes quoted, paths.
fn split_pasted_paths(text: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let unquoted = line.trim_matches(|c| c == '"' || c == '\'');
        if std::path::Path::new(unquoted).exists() {
            out.push(PathBuf::from(unquoted));
            continue;
        }
        // Several quoted paths on one line: "a b.flac" "c.flac"
        let before = out.len();
        let mut rest = line;
        while let Some(start) = rest.find(['"', '\'']) {
            let q = rest.as_bytes()[start] as char;
            let Some(len) = rest[start + 1..].find(q) else {
                break;
            };
            out.push(PathBuf::from(&rest[start + 1..start + 1 + len]));
            rest = &rest[start + len + 2..];
        }
        if out.len() == before {
            out.extend(line.split_whitespace().map(PathBuf::from));
        }
    }
    out
}

/// Searchable text for a track row.
fn track_text(t: &Track) -> String {
    format!(
        "{} {} {}",
        t.title,
        t.artist_line(),
        t.album.as_deref().unwrap_or("")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrolloff_keeps_margin() {
        let mut v = ListView {
            height: 10,
            ..Default::default()
        };
        v.selected = 9;
        v.scroll_into_view(100);
        assert_eq!(v.offset, 3); // row 9 shown with 3 rows below it
        v.selected = 99;
        v.scroll_into_view(100);
        assert_eq!(v.offset, 90);
        v.selected = 91;
        v.scroll_into_view(100);
        assert_eq!(v.offset, 88);
    }

    #[test]
    fn pasted_paths() {
        let p = split_pasted_paths("\"C:\\a b.flac\" \"C:\\c.mp3\"");
        assert_eq!(
            p,
            vec![PathBuf::from("C:\\a b.flac"), PathBuf::from("C:\\c.mp3")]
        );
        let p = split_pasted_paths("/x/a.flac\n/x/b.flac\n");
        assert_eq!(
            p,
            vec![PathBuf::from("/x/a.flac"), PathBuf::from("/x/b.flac")]
        );
    }
}
