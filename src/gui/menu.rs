//! Right-click menus: songs (play next, queue, radio, like, go to artist/album, add to
//! playlists) and Your Library items (play, hide, unfollow, delete).

use std::time::{Duration, Instant};

use crate::gui::app::{Gui, View, count};
use crate::gui::render::{Color, Rect, Weight};
use crate::gui::sidebar::LibItem;
use crate::gui::ui::Ui;
use crate::gui::ui::anim::{self, ease_out};
use crate::model::{EntryId, Track, TrackLocator};
use crate::player::{Player, PlayerCommand as Cmd};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuSource {
    Queue(EntryId),
    LocalPlaylist { id: i64, index: usize },
    Other,
}

/// A right-clicked Your Library item.
#[derive(Clone)]
pub(crate) struct LibraryMenu {
    pub item: LibItem,
    pub hidden: bool,
}

pub(crate) struct ContextMenu {
    pub(crate) pos: (f32, f32),
    pub(crate) tracks: Vec<Track>,
    pub(crate) source: MenuSource,
    pub(crate) opened: Instant,
    /// Set for Your Library items (then `tracks` is empty).
    pub(crate) library: Option<LibraryMenu>,
}

enum MenuItem {
    Radio,
    PlayNext,
    AddToQueue,
    Like(bool),
    GoToArtist { name: String, id: Option<String> },
    GoToAlbum,
    RemoveFromQueue(EntryId),
    RemoveFromPlaylist { id: i64, index: usize },
    ToLocal(i64),
    ToSpotify(String),
    LibPlay,
    LibHide(bool),
    LibUnfollow,
    LibDelete(i64),
}

impl Gui {
    pub(crate) fn open_menu(&mut self, tracks: Vec<Track>, source: MenuSource) {
        self.menu = Some(ContextMenu {
            pos: self.mouse,
            tracks,
            source,
            opened: Instant::now(),
            library: None,
        });
    }

    pub(crate) fn open_library_menu(&mut self, m: LibraryMenu) {
        self.menu = Some(ContextMenu {
            pos: self.mouse,
            tracks: Vec::new(),
            source: MenuSource::Other,
            opened: Instant::now(),
            library: Some(m),
        });
    }

    fn library_menu_items(m: &LibraryMenu) -> Vec<(String, Option<MenuItem>)> {
        let mut items = vec![("Play".to_string(), Some(MenuItem::LibPlay))];
        if m.item.key != "liked" {
            items.push(if m.hidden {
                (
                    "Show in Your Library".into(),
                    Some(MenuItem::LibHide(false)),
                )
            } else {
                (
                    "Hide from Your Library".into(),
                    Some(MenuItem::LibHide(true)),
                )
            });
        }
        if m.item.artist.is_some() {
            items.push(("Unfollow".into(), Some(MenuItem::LibUnfollow)));
        }
        if let View::LocalPlaylist(id) = m.item.view {
            items.push(("Delete playlist".into(), Some(MenuItem::LibDelete(id))));
        }
        items
    }

    pub(crate) fn draw_menu(&mut self, ui: &mut Ui, player: &Player) {
        let Some(menu) = &self.menu else { return };
        let t = ui.theme.clone();
        let mut items: Vec<(String, Option<MenuItem>)> = Vec::new();
        if let Some(m) = &menu.library {
            items = Self::library_menu_items(m);
        } else {
            self.song_menu_items(menu, &mut items);
        }
        const W: f32 = 272.0;
        const IH: f32 = 32.0;
        let h = items.len() as f32 * IH + 12.0;
        let full = ui.c.bounds();
        let x = menu.pos.0.min(full.w - W - 8.0);
        let y = menu.pos.1.min(full.h - h - 8.0).max(8.0);
        let rect = Rect::new(x, y, W, h);
        // Opens with a quick fade and a short drop into place.
        let e = ease_out(menu.opened.elapsed().as_secs_f32() / 0.16);
        if e < 1.0 {
            ui.anim.keep_alive();
        }
        ui.c.set_layer(1);
        ui.c.push_fx(0.0, (e - 1.0) * 6.0, e);
        ui.c.glow(
            Rect::new(rect.x, rect.y + 6.0, rect.w, rect.h),
            Color::BLACK.alpha(0.5),
            12.0,
            22.0,
        );
        ui.c.rect(rect, t.surface_hover, 12.0);
        ui.c.outline(rect, t.line(), 12.0, 1.0);
        let mut chosen = None;
        for (i, (label, item)) in items.iter().enumerate() {
            let row = Rect::new(x + 6.0, y + 6.0 + i as f32 * IH, W - 12.0, IH);
            match item {
                None => {
                    ui.label(
                        label,
                        row.x + 12.0,
                        row,
                        row.w - 24.0,
                        11.5,
                        Weight::Medium,
                        t.dim.alpha(0.8),
                    );
                }
                Some(_) => {
                    let hv = ui.hover_t(anim::id(("menu", i)), row);
                    ui.c.rect(row, t.fg.alpha(0.06 * hv), 8.0);
                    ui.label(
                        label,
                        row.x + 12.0 + 2.0 * hv,
                        row,
                        row.w - 24.0,
                        t.font_size - 1.0,
                        Weight::Regular,
                        t.fg.alpha(0.82).lerp(t.fg, hv),
                    );
                    if ui.hovered(row) && ui.input.pressed {
                        chosen = Some(i);
                    }
                }
            }
        }
        ui.c.pop_fx();
        ui.c.set_layer(0);
        let clicked_outside = (ui.input.pressed || ui.input.right_pressed)
            && !ui.hovered(rect)
            && menu.opened.elapsed() > Duration::from_millis(50);
        let menu = self.menu.take().expect("checked");
        if let Some(i) = chosen {
            if let Some(item) = items.into_iter().nth(i).and_then(|(_, it)| it) {
                self.run_menu_item(item, menu.tracks, menu.library, player);
            }
        } else if !clicked_outside
            && !ui
                .input
                .keys
                .iter()
                .any(|k| k.key == crate::gui::ui::Key::Escape)
        {
            self.menu = Some(menu);
        }
    }

    fn song_menu_items(&self, menu: &ContextMenu, items: &mut Vec<(String, Option<MenuItem>)>) {
        let one = (menu.tracks.len() == 1).then(|| &menu.tracks[0]);
        if one.is_some() {
            items.push(("Start radio".into(), Some(MenuItem::Radio)));
        }
        items.extend([
            ("Play next".into(), Some(MenuItem::PlayNext)),
            ("Add to queue".into(), Some(MenuItem::AddToQueue)),
        ]);
        let all_liked = menu.tracks.iter().all(|t| self.is_liked(t));
        items.push(if all_liked {
            (
                "Remove from Liked Songs".into(),
                Some(MenuItem::Like(false)),
            )
        } else {
            ("Save to Liked Songs".into(), Some(MenuItem::Like(true)))
        });
        if let Some(t) = one {
            if let Some(name) = t.artists.first().filter(|a| !a.is_empty()) {
                items.push((
                    "Go to artist".into(),
                    Some(MenuItem::GoToArtist {
                        name: name.clone(),
                        id: t.artist_id(0).map(str::to_owned),
                    }),
                ));
            }
            let has_album = match &t.locator {
                TrackLocator::Spotify { .. } => t.album_id.is_some(),
                _ => t.album.is_some(),
            };
            if has_album {
                items.push(("Go to album".into(), Some(MenuItem::GoToAlbum)));
            }
        }
        match menu.source {
            MenuSource::Queue(id) => items.push((
                "Remove from queue".into(),
                Some(MenuItem::RemoveFromQueue(id)),
            )),
            MenuSource::LocalPlaylist { id, index } => items.push((
                "Remove from playlist".into(),
                Some(MenuItem::RemoveFromPlaylist { id, index }),
            )),
            MenuSource::Other => {}
        }
        items.push(("Add to playlist".into(), None));
        let before = items.len();
        for (p, _) in &self.sp.local {
            items.push((p.name.clone(), Some(MenuItem::ToLocal(p.id))));
        }
        let all_local = menu
            .tracks
            .iter()
            .all(|t| !matches!(t.locator, TrackLocator::Spotify { .. }));
        if let Some(sp) = &self.sp.playlists {
            for p in sp.iter().filter(|p| p.owned_by_me).take(14) {
                let label = if all_local {
                    format!("{} (Spotify · local not allowed)", p.name)
                } else {
                    format!("{} (Spotify)", p.name)
                };
                items.push((label, Some(MenuItem::ToSpotify(p.id.clone()))));
            }
        }
        if items.len() == before {
            items.push((
                "No playlists yet: make one with + in Your Library".into(),
                None,
            ));
        }
    }

    fn run_menu_item(
        &mut self,
        item: MenuItem,
        tracks: Vec<Track>,
        library: Option<LibraryMenu>,
        player: &Player,
    ) {
        let n = tracks.len();
        match item {
            MenuItem::Like(liked) => {
                for t in &tracks {
                    self.set_liked(t, liked);
                }
            }
            MenuItem::GoToArtist { name, id } => self.open_artist(&name, id.as_deref()),
            MenuItem::LibPlay => {
                if let Some(m) = library {
                    self.play_view(&m.item.view, player);
                }
            }
            MenuItem::LibHide(hide) => {
                if let Some(m) = library {
                    if hide {
                        self.ui_state.hidden.insert(m.item.key.clone());
                        self.toast(format!(
                            "Hid “{}”: “Show hidden” at the bottom of Your Library brings it back",
                            m.item.title
                        ));
                    } else {
                        self.ui_state.hidden.remove(&m.item.key);
                    }
                    self.save_ui_state();
                }
            }
            MenuItem::LibUnfollow => {
                if let Some(a) = library.and_then(|m| m.item.artist) {
                    self.toast(format!("Unfollowed {}", a.name));
                    self.set_following(a, false);
                }
            }
            MenuItem::LibDelete(id) => match self.library.delete_playlist(id) {
                Ok(()) => {
                    self.toast("Deleted the playlist");
                    if self.view == View::LocalPlaylist(id) {
                        self.switch(View::Albums);
                    }
                }
                Err(e) => self.toast(format!("Couldn't delete: {e:#}")),
            },
            MenuItem::Radio => {
                if let Some(t) = tracks.into_iter().next() {
                    self.start_radio(t);
                }
            }
            MenuItem::PlayNext => {
                self.toast(format!("Playing {} next", count(n, "song")));
                player.send(Cmd::PlayNext(tracks));
            }
            MenuItem::AddToQueue => {
                self.toast(format!("Added {} to the queue", count(n, "song")));
                player.send(Cmd::Enqueue(tracks));
            }
            MenuItem::GoToAlbum => {
                if let Some(t) = tracks.first() {
                    let t = t.clone();
                    self.open_album_of(&t);
                }
            }
            MenuItem::RemoveFromQueue(id) => player.send(Cmd::Remove(vec![id])),
            MenuItem::RemoveFromPlaylist { id, index } => {
                if let Some((_, existing)) = self.sp.local.iter().find(|(p, _)| p.id == id) {
                    let mut list = existing.to_vec();
                    if index < list.len() {
                        list.remove(index);
                        if let Err(e) = self.library.set_playlist_tracks(id, &list) {
                            self.toast(format!("Couldn't update playlist: {e:#}"));
                        }
                    }
                }
            }
            MenuItem::ToLocal(id) => {
                if let Some((p, existing)) = self.sp.local.iter().find(|(p, _)| p.id == id).cloned()
                {
                    let mut list = existing.to_vec();
                    list.extend(tracks);
                    match self.library.set_playlist_tracks(id, &list) {
                        Ok(()) => self.toast(format!("Added {} to {}", count(n, "song"), p.name)),
                        Err(e) => self.toast(format!("Couldn't update playlist: {e:#}")),
                    }
                }
            }
            MenuItem::ToSpotify(pid) => {
                let Some(sp) = self.spotify.clone() else {
                    return;
                };
                let name = self
                    .sp
                    .playlists
                    .as_ref()
                    .and_then(|ps| ps.iter().find(|p| p.id == pid).map(|p| p.name.clone()))
                    .unwrap_or_else(|| "playlist".into());
                if tracks
                    .iter()
                    .all(|t| !matches!(t.locator, TrackLocator::Spotify { .. }))
                {
                    // Spotify playlists can only hold Spotify tracks.
                    self.toast("Local tracks can't be added to Spotify playlists");
                    return;
                }
                self.sp.adds.push((name, sp.add_to_playlist(&pid, &tracks)));
            }
        }
    }
}
