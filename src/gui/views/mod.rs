//! Pages: which one is open, its header, page keys, and small shared pieces.

// Row loops index by position: the index drives layout and selection, not just access.
#![allow(clippy::needless_range_loop)]

pub mod artist;
pub mod files;
pub mod grid;
pub mod library;
pub mod lists;
pub mod lyrics;
pub mod page;
pub mod playlists;
pub mod search;
pub mod settings;
pub mod visualizer;

use crate::audio::AnalysisFrame;
use crate::gui::app::{Gui, ROW_H, View};
use crate::gui::render::{Color, Rect, Weight};
use crate::gui::ui::icons::Icon;
use crate::gui::ui::{Key, KeyPress, Ui};
use crate::library::Album;
use crate::model::Track;
use crate::player::{Player, PlayerCommand as Cmd, PlayerSnapshot};

impl Gui {
    pub(crate) fn content(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        player: &Player,
        snap: &PlayerSnapshot,
        a: &AnalysisFrame,
        dt: f32,
    ) {
        // Home: your library's sections as chips, Spotify style.
        let r = if self.view.section() == View::Albums {
            self.home_tabs(ui, r)
        } else {
            r
        };
        match self.view.clone() {
            View::Albums => self.albums_view(ui, r, player),
            View::Album { name, artist } => {
                self.album_view(ui, r, player, snap, a, &name, artist.as_deref())
            }
            View::Artists => self.artists_view(ui, r),
            View::Artist(name) => self.artist_view(ui, r, player, &name),
            View::Songs => self.songs_view(ui, r, player, snap, a),
            View::Search => self.search_view(ui, r, player, snap, a),
            View::Files => self.files_view(ui, r, player),
            View::Visualizer => self.visualizer(ui, r, a, dt),
            View::Playlists => self.playlists_view(ui, r, player),
            View::LocalPlaylist(id) => self.local_playlist_view(ui, r, player, snap, a, id),
            View::SpotifyPlaylist { id, name } => {
                self.spotify_playlist_view(ui, r, player, snap, a, &id, &name)
            }
            View::SpotifyAlbum { id, name } => {
                self.spotify_album_view(ui, r, player, snap, a, &id, &name)
            }
            View::Liked => self.liked_view(ui, r, player, snap, a),
            View::SpotifyArtist { id, name } => {
                self.spotify_artist_view(ui, r, player, snap, a, &id, &name)
            }
            View::Lyrics => self.lyrics_view(ui, r, player, snap),
            View::Settings => self.settings_view(ui, r, player, snap),
        }
    }

    pub(crate) fn header(&mut self, ui: &mut Ui, r: Rect, title: &str, subtitle: &str) -> Rect {
        let t = ui.theme;
        ui.label(
            title,
            r.x,
            Rect::new(r.x, r.y, r.w, 40.0),
            r.w * 0.6,
            28.0,
            Weight::Bold,
            t.fg,
        );
        ui.label(
            subtitle,
            r.x,
            Rect::new(r.x, r.y + 40.0, r.w, 22.0),
            r.w * 0.6,
            13.0,
            Weight::Regular,
            t.dim,
        );
        Rect::new(r.x, r.y + 82.0, r.w, (r.h - 82.0).max(0.0))
    }

    /// Right-aligned header buttons; returns the index of the one clicked.
    pub(crate) fn header_buttons(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        labels: &[&str],
    ) -> Option<usize> {
        let t = ui.theme;
        let mut x = r.right();
        let mut clicked = None;
        for (i, label) in labels.iter().enumerate().rev() {
            let icon = button_icon(label);
            let w = ui.measure(label, t.font_size - 1.0, Weight::Medium)
                + 32.0
                + if icon.is_some() { 22.0 } else { 0.0 };
            x -= w;
            if ui.pill_icon(label, icon, x, r.y + 4.0, false).0 {
                clicked = Some(i);
            }
            x -= 8.0;
        }
        clicked
    }

    pub(crate) fn view_key(&mut self, k: KeyPress, player: &Player) {
        let (key, len, grid): (&'static str, usize, bool) = match &self.view {
            View::Albums => ("albums", self.cache.albums.len(), true),
            View::Artist(name) => {
                let n = self
                    .cache
                    .albums
                    .iter()
                    .filter(|al| al.artist.as_deref() == Some(name.as_str()))
                    .count();
                ("artist", n, true)
            }
            View::Album { .. } => (
                "album",
                self.cache.detail.as_ref().map_or(0, |d| d.1.len()),
                false,
            ),
            View::Artists => ("artists", self.cache.artists.len(), false),
            View::Songs => ("songs", self.cache.songs.len(), false),
            View::Search => (
                "search",
                self.cache.search_shown.as_ref().map_or(0, |s| s.len()),
                false,
            ),
            View::Files => ("files", self.browser.entries().len(), false),
            View::Visualizer
            | View::Playlists
            | View::Settings
            | View::SpotifyArtist { .. }
            | View::Lyrics => return,
            View::LocalPlaylist(_)
            | View::SpotifyPlaylist { .. }
            | View::SpotifyAlbum { .. }
            | View::Liked => (
                "collection",
                self.collection_tracks().map_or(0, |t| t.len()),
                false,
            ),
        };
        if len == 0 {
            return;
        }
        let ls = self.list(key);
        let step = if grid { ls.cols.max(1) } else { 1 };
        let page = ((ls.height / ROW_H) as usize).max(1) * step;
        let sel = ls.selected;
        let moved = match k.key {
            Key::Down | Key::Char('j') => Some(sel + step),
            Key::Up | Key::Char('k') => Some(sel.saturating_sub(step)),
            Key::Char('l') if grid => Some(sel + 1),
            Key::Char('h') if grid => Some(sel.saturating_sub(1)),
            Key::PageDown => Some(sel + page),
            Key::PageUp => Some(sel.saturating_sub(page)),
            Key::Home | Key::Char('g') => Some(0),
            Key::End | Key::Char('G') => Some(len - 1),
            _ => None,
        };
        if let Some(i) = moved {
            self.kb_nav = true;
            let i = i.min(len - 1);
            let ls = self.list(key);
            ls.selected = i;
            let (row, row_h) = if grid {
                (i / step, ls.height.max(1.0) / 3.0)
            } else {
                (i, ROW_H)
            };
            ls.reveal(row, row_h);
            return;
        }
        match (k.key, &self.view) {
            (Key::Enter, View::Albums) => {
                let al = self.cache.albums[sel].clone();
                self.navigate(View::Album {
                    name: al.name,
                    artist: al.artist,
                });
            }
            (Key::Enter, View::Artists) => {
                let name = self.cache.artists[sel].name.clone();
                self.navigate(View::Artist(name));
            }
            (Key::Enter | Key::Char('l'), View::Files) => self.files_open(sel, player),
            (Key::Backspace | Key::Char('h'), View::Files) => self.files_up(),
            (Key::Char('a'), View::Files) => self.files_add(sel, player, false),
            (Key::Char('A'), View::Files) => self.files_add(sel, player, true),
            (Key::Enter | Key::Char('a') | Key::Char('A'), _) => {
                let tracks = match &self.view {
                    View::Album { .. } => self.cache.detail.as_ref().map(|d| d.1.clone()),
                    View::Songs => Some(self.cache.songs.clone()),
                    View::Search => self.cache.search_shown.clone(),
                    _ => self.collection_tracks(),
                };
                let Some(tracks) = tracks else { return };
                match k.key {
                    Key::Enter => player.send(Cmd::PlayTracks {
                        tracks: tracks.to_vec(),
                        start: sel,
                    }),
                    Key::Char('A') => player.send(Cmd::PlayNext(vec![tracks[sel].clone()])),
                    _ => {
                        self.toast(format!("Added “{}” to the queue", tracks[sel].title));
                        player.send(Cmd::Enqueue(vec![tracks[sel].clone()]));
                    }
                }
            }
            _ => {}
        }
    }

    fn play_album(&mut self, al: &Album, player: &Player, shuffle: bool) {
        let tracks = self.library.album_tracks(&al.name, al.artist.as_deref());
        self.play_tracks(tracks, player, shuffle);
    }
}

impl Gui {
    /// Plays a list, optionally shuffled.
    pub(crate) fn play_tracks(&mut self, tracks: Vec<Track>, player: &Player, shuffle: bool) {
        if tracks.is_empty() {
            return;
        }
        player.send(Cmd::SetShuffle(shuffle));
        let start = if shuffle {
            fastrand::usize(..tracks.len())
        } else {
            0
        };
        player.send(Cmd::PlayTracks { tracks, start });
    }
}

/// The accent-filled main action button.
pub(crate) fn primary_button(ui: &mut Ui, r: Rect, label: &str) -> bool {
    ui.primary_button(r, label)
}

pub(crate) fn empty_state(ui: &mut Ui, r: Rect, title: &str, hint: &str) {
    let t = ui.theme;
    let mid = Rect::new(r.x, r.y + r.h * 0.28, r.w, 30.0);
    ui.label_center(title, mid, 18.0, Weight::Medium, t.fg.alpha(0.85));
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

/// The icon that goes with a common action label.
pub(crate) fn button_icon(label: &str) -> Option<Icon> {
    Some(match label {
        "Shuffle" | "Shuffle all" => Icon::Shuffle,
        "Play all" => Icon::Play,
        "Add to queue" | "Add all" => Icon::Queue,
        "Clear" | "Delete" => Icon::Close,
        "Music" => Icon::Folder,
        "Up" => Icon::Up,
        "New playlist" | "Copy to Khoostic" => Icon::Plus,
        _ => return None,
    })
}

pub(crate) fn spotify_green() -> Color {
    Color::rgba8(30, 215, 96, 255)
}

const HOME_TABS: [(View, &str); 5] = [
    (View::Albums, "Albums"),
    (View::Artists, "Artists"),
    (View::Songs, "Songs"),
    (View::Playlists, "Playlists"),
    (View::Files, "Files"),
];

impl Gui {
    /// The chips at the top of the home pages; returns the page area below them.
    fn home_tabs(&mut self, ui: &mut Ui, r: Rect) -> Rect {
        let labels: Vec<&str> = HOME_TABS.iter().map(|(_, l)| *l).collect();
        let current = match &self.view {
            View::Album { .. } => View::Albums,
            View::Artist(_) => View::Artists,
            v => v.clone(),
        };
        let active = HOME_TABS
            .iter()
            .position(|(v, _)| *v == current)
            .unwrap_or(0);
        if let Some(i) = page::chips(ui, r.x, r.y, &labels, active) {
            self.switch(HOME_TABS[i].0.clone());
        }
        Rect::new(r.x, r.y + 56.0, r.w, (r.h - 56.0).max(0.0))
    }
}
