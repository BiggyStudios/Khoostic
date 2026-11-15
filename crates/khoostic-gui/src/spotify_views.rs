//! Spotify and playlists in the GUI: connection status, playlists (Khoostic + Spotify), playlist
//! / liked-songs / Spotify-album pages, Spotify search results, and the track context menu with
//! "Add to playlist" (local tracks are refused for Spotify playlists).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use khoostic_core::{CoverRef, EntryId, Track, TrackLocator};
use khoostic_player::{Player, PlayerCommand as Cmd, PlayerSnapshot};
use khoostic_spotify::{AddOutcome, Pending, Playlist, SearchResults, Status};

use crate::anim::{self, ease_out};
use crate::app::{Gui, View, count, fmt_long};
use crate::canvas::{Color, Rect, Weight};
use crate::icons::{self, Icon};
use crate::ui::Ui;
use crate::views::Tile;

/// Debounce for Spotify search-as-you-type.
const SEARCH_DELAY: Duration = Duration::from_millis(350);

#[derive(Default)]
pub(crate) struct SpState {
    pub status_version: Option<u64>,
    pub playlists: Option<Arc<Vec<Playlist>>>,
    playlists_pending: Option<Pending<Vec<Playlist>>>,
    /// Track lists by key: "liked", "pl:<id>", "al:<id>".
    pub tracks: HashMap<String, Arc<Vec<Track>>>,
    tracks_pending: HashMap<String, Pending<Vec<Track>>>,
    pub search: Option<(String, Arc<SearchResults>)>,
    search_pending: Option<(String, Pending<SearchResults>)>,
    search_due: Option<(String, Instant)>,
    adds: Vec<(String, Pending<AddOutcome>)>,
    /// Khoostic (local DB) playlists with their tracks, refreshed with the library.
    pub local: Vec<(khoostic_library::Playlist, Arc<Vec<Track>>)>,
    local_version: Option<u64>,
}

/// What clicking the Spotify sidebar row does in the current state.
type SpotifyAction = fn(&khoostic_spotify::Spotify);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuSource {
    Queue(EntryId),
    LocalPlaylist { id: i64, index: usize },
    Other,
}

pub(crate) struct ContextMenu {
    pos: (f32, f32),
    tracks: Vec<Track>,
    source: MenuSource,
    opened: Instant,
}

enum MenuItem {
    PlayNext,
    AddToQueue,
    GoToAlbum,
    RemoveFromQueue(EntryId),
    RemoveFromPlaylist { id: i64, index: usize },
    ToLocal(i64),
    ToSpotify(String),
}

impl Gui {
    pub(crate) fn open_menu(&mut self, tracks: Vec<Track>, source: MenuSource) {
        self.menu = Some(ContextMenu {
            pos: self.mouse,
            tracks,
            source,
            opened: Instant::now(),
        });
    }

    /// Background results arrive here once per frame.
    pub(crate) fn poll_spotify(&mut self) {
        // Khoostic playlists follow the library version.
        let v = self.library.version();
        if self.sp.local_version != Some(v) {
            self.sp.local_version = Some(v);
            self.sp.local = self
                .library
                .playlists()
                .into_iter()
                .map(|p| {
                    let tracks = Arc::new(self.library.playlist_tracks(p.id));
                    (p, tracks)
                })
                .collect();
        }
        let Some(sp) = self.spotify.clone() else {
            return;
        };
        if self.sp.status_version != Some(sp.version()) {
            self.sp.status_version = Some(sp.version());
            if !sp.is_connected() {
                self.sp.playlists = None;
                self.sp.tracks.clear();
                self.sp.search = None;
            }
        }
        if let Some(p) = &self.sp.playlists_pending
            && let Some(r) = p.poll()
        {
            self.sp.playlists_pending = None;
            match r {
                Ok(list) => self.sp.playlists = Some(Arc::new(list)),
                Err(e) => self.toast(format!("Spotify: {e:#}")),
            }
        }
        let done: Vec<(String, anyhow::Result<Vec<Track>>)> = self
            .sp
            .tracks_pending
            .iter()
            .filter_map(|(k, p)| p.poll().map(|r| (k.clone(), r)))
            .collect();
        for (k, r) in done {
            self.sp.tracks_pending.remove(&k);
            match r {
                Ok(t) => {
                    self.sp.tracks.insert(k, Arc::new(t));
                }
                Err(e) => self.toast(format!("Spotify: {e:#}")),
            }
        }
        if let Some((q, p)) = &self.sp.search_pending
            && let Some(r) = p.poll()
        {
            let q = q.clone();
            self.sp.search_pending = None;
            match r {
                Ok(res) => self.sp.search = Some((q, Arc::new(res))),
                Err(e) => self.toast(format!("Spotify search: {e:#}")),
            }
        }
        if let Some((q, due)) = &self.sp.search_due
            && Instant::now() >= *due
            && sp.is_connected()
        {
            let q = q.clone();
            self.sp.search_due = None;
            self.sp.search_pending = Some((q.clone(), sp.search(&q, 30)));
        }
        let mut finished = Vec::new();
        self.sp.adds.retain(|(name, p)| match p.poll() {
            Some(r) => {
                finished.push((name.clone(), r));
                false
            }
            None => true,
        });
        for (name, r) in finished {
            match r {
                Ok(o) if o.skipped_local > 0 && o.added == 0 => {
                    self.toast("Local tracks can't be added to Spotify playlists");
                }
                Ok(o) if o.skipped_local > 0 => self.toast(format!(
                    "Added {} to {name}; skipped {} local",
                    count(o.added, "song"),
                    o.skipped_local
                )),
                Ok(o) => self.toast(format!("Added {} to {name}", count(o.added, "song"))),
                Err(e) => self.toast(format!("Couldn't add to {name}: {e:#}")),
            }
            // The playlist changed on Spotify: refetch next time it's shown.
            self.sp.tracks.retain(|k, _| !k.starts_with("pl:"));
            self.sp.playlists = None;
        }
    }

    pub(crate) fn spotify_busy(&self) -> bool {
        self.sp.playlists_pending.is_some()
            || !self.sp.tracks_pending.is_empty()
            || self.sp.search_pending.is_some()
            || self.sp.search_due.is_some()
            || !self.sp.adds.is_empty()
            || self
                .spotify
                .as_ref()
                .is_some_and(|s| matches!(s.status(), Status::SigningIn | Status::Connecting))
    }

    fn ensure_playlists(&mut self) -> Option<Arc<Vec<Playlist>>> {
        let sp = self.spotify.clone()?;
        if !sp.is_connected() {
            return None;
        }
        if self.sp.playlists.is_none() && self.sp.playlists_pending.is_none() {
            self.sp.playlists_pending = Some(sp.playlists());
        }
        self.sp.playlists.clone()
    }

    fn ensure_tracks(&mut self, key: &str) -> Option<Arc<Vec<Track>>> {
        if let Some(t) = self.sp.tracks.get(key) {
            return Some(t.clone());
        }
        let sp = self.spotify.clone()?;
        if sp.is_connected() && !self.sp.tracks_pending.contains_key(key) {
            let pending = if key == "liked" {
                sp.saved_tracks()
            } else if let Some(id) = key.strip_prefix("pl:") {
                sp.playlist_tracks(id)
            } else {
                let id = key.strip_prefix("al:")?;
                sp.album_tracks(id)
            };
            self.sp.tracks_pending.insert(key.to_owned(), pending);
        }
        None
    }

    /// Schedules a (debounced) Spotify search for `q`.
    pub(crate) fn spotify_search(&mut self, q: &str) -> Option<Arc<SearchResults>> {
        let connected = self.spotify.as_ref().is_some_and(|s| s.is_connected());
        if !connected || q.is_empty() {
            return None;
        }
        match &self.sp.search {
            Some((sq, res)) if sq == q => return Some(res.clone()),
            _ => {}
        }
        let queued = self.sp.search_due.as_ref().map(|d| d.0.as_str()) == Some(q)
            || self.sp.search_pending.as_ref().map(|p| p.0.as_str()) == Some(q);
        if !queued {
            self.sp.search_due = Some((q.to_owned(), Instant::now() + SEARCH_DELAY));
        }
        self.sp.search.as_ref().map(|s| s.1.clone())
    }

    // ---- sidebar ------------------------------------------------------------------------

    /// The Spotify block of the sidebar. Returns the next free y.
    pub(crate) fn sidebar_spotify(&mut self, ui: &mut Ui, r: Rect, mut y: f32) -> f32 {
        let Some(sp) = self.spotify.clone() else {
            return y;
        };
        let t = ui.theme.clone();
        let green = Color::hex("#1ed760").unwrap_or(t.accent);
        ui.label(
            "Spotify",
            r.x + 26.0,
            Rect::new(r.x, y, r.w, 22.0),
            r.w,
            11.5,
            Weight::Medium,
            t.dim.alpha(0.75),
        );
        y += 26.0;
        let row = Rect::new(r.x + 12.0, y, r.w - 20.0, 36.0);
        let status = sp.status();
        let (label, sub, action): (&str, Option<String>, Option<SpotifyAction>) = match &status {
            Status::Disconnected => ("Connect Spotify", None, Some(|s| s.sign_in())),
            Status::Failed(e) => ("Connect Spotify", Some(e.clone()), Some(|s| s.sign_in())),
            Status::SigningIn => (
                "Waiting for browser…",
                Some("Click to cancel".into()),
                Some(|s| s.cancel_sign_in()),
            ),
            Status::Connecting => ("Connecting…", None, None),
            Status::Connected(_) => ("Liked Songs", None, None),
        };
        let connected = matches!(status, Status::Connected(_));
        let busy = matches!(status, Status::SigningIn | Status::Connecting);
        let active = self.view == View::Liked;
        let id = anim::id("nav-spotify");
        let h = ui.hover_t(id, row);
        if !active {
            ui.c.rect(row, t.wash(0.04 * h), t.radius);
        }
        let on = ui.anim.to(id ^ 3, active as u8 as f32, anim::NORMAL);
        let ic = Rect::new(row.x + 12.0, row.y + 9.0, 18.0, 18.0);
        if busy {
            // A spinning arc while the browser / connection is in progress.
            ui.anim.keep_alive();
            let a = self.last_spin();
            ui.c.arc(ic.x + 9.0, ic.y + 9.0, 7.0, a, a + 4.2, 2.0, t.accent);
        } else if connected {
            icons::draw(
                ui.c,
                Icon::Heart,
                ic,
                t.dim.lerp(t.fg, h * 0.6).lerp(green, on.max(0.0)),
            );
        } else {
            icons::draw(ui.c, Icon::Spotify, ic, t.dim.lerp(green, h));
        }
        let color = if busy {
            t.accent
        } else {
            t.dim.lerp(t.fg, (h * 0.7 + on).min(1.0))
        };
        ui.label(
            label,
            row.x + 42.0,
            row,
            row.w - 50.0,
            t.font_size,
            Weight::Medium,
            color,
        );
        if ui.clicked(row) {
            match (action, &status) {
                (Some(f), _) => f(&sp),
                (None, Status::Connected(_)) => self.switch(View::Liked),
                _ => {}
            }
        }
        y += 38.0;
        let note = match &status {
            Status::Connected(a) if !a.premium => Some(format!("{} · Premium needed", a.name)),
            Status::Connected(a) => Some(a.name.clone()),
            _ => sub,
        };
        if let Some(note) = note {
            ui.label(
                &note,
                r.x + 54.0,
                Rect::new(r.x, y - 6.0, r.w, 18.0),
                r.w - 66.0,
                11.5,
                Weight::Regular,
                t.dim.alpha(0.8),
            );
            y += 18.0;
        }
        y + 10.0
    }

    /// Angle for loading spinners: one turn every ~0.9 s.
    fn last_spin(&self) -> f32 {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() % 900) as f32;
        ms / 900.0 * std::f32::consts::TAU
    }

    // ---- pages --------------------------------------------------------------------------

    pub(crate) fn playlists_view(&mut self, ui: &mut Ui, r: Rect, player: &Player) {
        let spotify = self.ensure_playlists();
        let local = self.sp.local.clone();
        let n_sp = spotify.as_ref().map_or(0, |s| s.len());
        let body = self.header(
            ui,
            r,
            "Playlists",
            &format!("{} in Khoostic · {} on Spotify", local.len(), n_sp),
        );
        if self.header_buttons(ui, r, &["New playlist"]).is_some() {
            let name = format!("Playlist {}", local.len() + 1);
            match self.library.create_playlist(&name, &[]) {
                Ok(_) => self.toast(format!("Created “{name}” — add songs with right-click")),
                Err(e) => self.toast(format!("Couldn't create playlist: {e:#}")),
            }
        }
        let mut tiles: Vec<Tile> = local
            .iter()
            .map(|(p, tracks)| Tile {
                title: p.name.clone(),
                subtitle: format!("Khoostic · {}", count(p.tracks as usize, "song")),
                cover: tracks.iter().find_map(|t| t.cover.clone()),
            })
            .collect();
        if let Some(sp) = &spotify {
            tiles.extend(sp.iter().map(|p| Tile {
                title: p.name.clone(),
                subtitle: if p.owned_by_me {
                    format!("Spotify · {}", count(p.tracks as usize, "song"))
                } else {
                    format!("Spotify · by {}", p.owner)
                },
                cover: p.cover.clone(),
            }));
        }
        if tiles.is_empty() {
            let hint = if self.spotify.as_ref().is_some_and(|s| s.is_connected()) {
                "Loading your Spotify playlists…"
            } else {
                "Create one, or connect Spotify to see yours"
            };
            return crate::views::empty_state(ui, body, "No playlists yet", hint);
        }
        let (open, play) = self.tile_grid(ui, body, "playlists", &tiles);
        let pick = open.or(play);
        if let Some(i) = pick {
            if i < local.len() {
                let (p, tracks) = &local[i];
                if play.is_some() {
                    self.play_tracks((**tracks).clone(), player, false);
                } else {
                    self.navigate(View::LocalPlaylist(p.id));
                }
            } else if let Some(sp) = &spotify {
                let p = &sp[i - local.len()];
                self.navigate(View::SpotifyPlaylist {
                    id: p.id.clone(),
                    name: p.name.clone(),
                });
            }
        }
    }

    /// Shared layout for playlist-like pages: cover, title, meta, buttons, tracks.
    #[allow(clippy::too_many_arguments)]
    fn collection_page(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        player: &Player,
        snap: &PlayerSnapshot,
        a: &khoostic_audio::AnalysisFrame,
        kind: &str,
        title: &str,
        cover: Option<CoverRef>,
        tracks: Option<Arc<Vec<Track>>>,
        extra_buttons: &[&str],
    ) -> Option<usize> {
        let t = ui.theme.clone();
        let size = 180.0f32.min(r.w * 0.28);
        let cover_rect = Rect::new(r.x, r.y, size, size);
        ui.c.glow(cover_rect, Color::BLACK.alpha(0.55), t.radius, 22.0);
        let cover = cover.or_else(|| {
            tracks
                .as_ref()
                .and_then(|ts| ts.iter().find_map(|t| t.cover.clone()))
        });
        ui.cover(
            cover_rect,
            cover.as_ref(),
            khoostic_library::CoverSize::Large,
            t.radius + 2.0,
            title,
        );
        let x = cover_rect.right() + 28.0;
        let w = r.right() - x;
        ui.label(
            kind,
            x,
            Rect::new(x, r.y + 22.0, w, 18.0),
            w,
            11.0,
            Weight::Bold,
            t.dim,
        );
        ui.label(
            title,
            x,
            Rect::new(x, r.y + 42.0, w, 48.0),
            w,
            34.0,
            Weight::Bold,
            t.fg,
        );
        let meta = match &tracks {
            Some(ts) => {
                let total: Duration = ts.iter().filter_map(|t| t.duration).sum();
                format!("{}, {}", count(ts.len(), "song"), fmt_long(total))
            }
            None => "Loading…".into(),
        };
        ui.label(
            &meta,
            x,
            Rect::new(x, r.y + 94.0, w, 22.0),
            w,
            t.font_size,
            Weight::Regular,
            t.dim,
        );
        let by = r.y + 128.0;
        let mut extra_clicked = None;
        if let Some(ts) = &tracks {
            if crate::views::primary_button(ui, Rect::new(x, by, 108.0, 40.0), "Play") {
                self.play_tracks(ts.to_vec(), player, false);
            }
            let mut bx = x + 120.0;
            let mut labels = vec!["Shuffle", "Add to queue"];
            labels.extend_from_slice(extra_buttons);
            for (i, label) in labels.iter().enumerate() {
                let icon = match *label {
                    "Shuffle" => Some(Icon::Shuffle),
                    "Add to queue" => Some(Icon::Queue),
                    "Delete" => Some(Icon::Close),
                    _ => Some(Icon::Plus),
                };
                let (clicked, bw) = ui.pill_icon(label, icon, bx, by + 3.0, false);
                bx += bw + 8.0;
                if clicked {
                    match i {
                        0 => self.play_tracks(ts.to_vec(), player, true),
                        1 => {
                            self.toast(format!("Added {} to the queue", count(ts.len(), "song")));
                            player.send(Cmd::Enqueue(ts.to_vec()));
                        }
                        n => extra_clicked = Some(n - 2),
                    }
                }
            }
            let list = Rect::new(r.x, r.y + size + 28.0, r.w, (r.h - size - 28.0).max(0.0));
            let list = crate::views::column_header(ui, list, true, true);
            let style = crate::views::TrackListStyle {
                covers: true,
                album_column: true,
                track_numbers: false,
            };
            let action = self.track_list(ui, list, "collection", ts, style, snap, a);
            self.apply_row_action(action, ts, player);
        }
        extra_clicked
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spotify_playlist_view(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        player: &Player,
        snap: &PlayerSnapshot,
        a: &khoostic_audio::AnalysisFrame,
        id: &str,
        name: &str,
    ) {
        let key = format!("pl:{id}");
        let tracks = self.ensure_tracks(&key);
        let cover = self
            .sp
            .playlists
            .as_ref()
            .and_then(|ps| ps.iter().find(|p| p.id == id))
            .and_then(|p| p.cover.clone());
        if self
            .collection_page(
                ui,
                r,
                player,
                snap,
                a,
                "SPOTIFY PLAYLIST",
                name,
                cover,
                tracks.clone(),
                &["Copy to Khoostic"],
            )
            .is_some()
            && let Some(ts) = tracks
        {
            match self.library.create_playlist(name, &ts) {
                Ok(_) => self.toast(format!("Copied “{name}” to your Khoostic playlists")),
                Err(e) => self.toast(format!("Copy failed: {e:#}")),
            }
        }
    }

    pub(crate) fn liked_view(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        player: &Player,
        snap: &PlayerSnapshot,
        a: &khoostic_audio::AnalysisFrame,
    ) {
        let tracks = self.ensure_tracks("liked");
        if self
            .collection_page(
                ui,
                r,
                player,
                snap,
                a,
                "SPOTIFY",
                "Liked Songs",
                None,
                tracks.clone(),
                &["Copy to Khoostic"],
            )
            .is_some()
            && let Some(ts) = tracks
        {
            if let Err(e) = self.library.create_playlist("Liked Songs", &ts) {
                self.toast(format!("Copy failed: {e:#}"));
            } else {
                self.toast("Copied Liked Songs to your Khoostic playlists");
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spotify_album_view(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        player: &Player,
        snap: &PlayerSnapshot,
        a: &khoostic_audio::AnalysisFrame,
        id: &str,
        name: &str,
    ) {
        let tracks = self.ensure_tracks(&format!("al:{id}"));
        let cover = self.sp.search.as_ref().and_then(|s| {
            s.1.albums
                .iter()
                .find(|al| al.id == id)
                .and_then(|al| al.cover.clone())
        });
        self.collection_page(
            ui,
            r,
            player,
            snap,
            a,
            "ALBUM ON SPOTIFY",
            name,
            cover,
            tracks,
            &[],
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn local_playlist_view(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        player: &Player,
        snap: &PlayerSnapshot,
        a: &khoostic_audio::AnalysisFrame,
        id: i64,
    ) {
        let Some((p, tracks)) = self.sp.local.iter().find(|(p, _)| p.id == id).cloned() else {
            return crate::views::empty_state(
                ui,
                r,
                "Playlist not found",
                "It may have been deleted",
            );
        };
        if self
            .collection_page(
                ui,
                r,
                player,
                snap,
                a,
                "KHOOSTIC PLAYLIST",
                &p.name,
                None,
                Some(tracks),
                &["Delete"],
            )
            .is_some()
        {
            match self.library.delete_playlist(id) {
                Ok(()) => {
                    self.toast(format!("Deleted “{}”", p.name));
                    self.switch(View::Playlists);
                }
                Err(e) => self.toast(format!("Delete failed: {e:#}")),
            }
        }
    }

    // ---- context menu -------------------------------------------------------------------

    pub(crate) fn draw_menu(&mut self, ui: &mut Ui, player: &Player) {
        let Some(menu) = &self.menu else { return };
        let t = ui.theme.clone();
        let mut items: Vec<(String, Option<MenuItem>)> = vec![
            ("Play next".into(), Some(MenuItem::PlayNext)),
            ("Add to queue".into(), Some(MenuItem::AddToQueue)),
        ];
        if menu.tracks.len() == 1
            && menu.tracks[0].album.is_some()
            && matches!(menu.tracks[0].locator, TrackLocator::Local(_))
        {
            items.push(("Go to album".into(), Some(MenuItem::GoToAlbum)));
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
            items.push(("No playlists yet: create one in Playlists".into(), None));
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
                self.run_menu_item(item, menu.tracks, player);
            }
        } else if !clicked_outside
            && !ui
                .input
                .keys
                .iter()
                .any(|k| k.key == crate::ui::Key::Escape)
        {
            self.menu = Some(menu);
        }
    }

    fn run_menu_item(&mut self, item: MenuItem, tracks: Vec<Track>, player: &Player) {
        let n = tracks.len();
        match item {
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

impl Gui {
    /// Tracks of the open playlist / liked songs / Spotify album page, if loaded.
    pub(crate) fn collection_tracks(&self) -> Option<Arc<Vec<Track>>> {
        match &self.view {
            View::LocalPlaylist(id) => self
                .sp
                .local
                .iter()
                .find(|(p, _)| p.id == *id)
                .map(|(_, t)| t.clone()),
            View::SpotifyPlaylist { id, .. } => self.sp.tracks.get(&format!("pl:{id}")).cloned(),
            View::SpotifyAlbum { id, .. } => self.sp.tracks.get(&format!("al:{id}")).cloned(),
            View::Liked => self.sp.tracks.get("liked").cloned(),
            _ => None,
        }
    }

    /// "Library | Spotify" tabs on the search page (only when Spotify is connected).
    /// Returns true when the Spotify tab is active.
    pub(crate) fn search_tabs(&mut self, ui: &mut Ui, r: Rect) -> bool {
        if !self.spotify.as_ref().is_some_and(|s| s.is_connected()) {
            self.search_spotify = false;
            return false;
        }
        let t = ui.theme;
        let seg_w = ui
            .measure("Library", t.font_size - 1.0, Weight::Medium)
            .max(ui.measure("Spotify", t.font_size - 1.0, Weight::Medium))
            + 30.0;
        let x = r.right() - 2.0 * seg_w - 6.0;
        let sel = self.search_spotify as usize;
        if let Some(i) = ui.segmented(
            anim::id("search-tabs"),
            x,
            r.y + 4.0,
            &["Library", "Spotify"],
            sel,
        ) {
            self.search_spotify = i == 1;
        }
        self.search_spotify
    }

    pub(crate) fn spotify_search_view(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        player: &Player,
        snap: &PlayerSnapshot,
        a: &khoostic_audio::AnalysisFrame,
        q: &str,
    ) {
        let t = ui.theme.clone();
        let results = self.spotify_search(q);
        let stale = self.sp.search.as_ref().is_some_and(|s| s.0 != q);
        let sub = match (&results, stale) {
            (None, _) => "Searching Spotify…".to_string(),
            (Some(_), true) => "Updating…".to_string(),
            (Some(res), false) => format!(
                "{} · {}",
                count(res.albums.len(), "album"),
                count(res.tracks.len(), "song")
            ),
        };
        let body = self.header(ui, r, &format!("“{q}” on Spotify"), &sub);
        let Some(res) = results else { return };
        let mut list = body;
        if !res.albums.is_empty() {
            let strip_h = 236.0f32.min(body.h * 0.45);
            let strip = Rect::new(body.x, body.y, body.w, strip_h);
            const TILE: f32 = 150.0;
            ui.c.push_clip(strip);
            for (i, al) in res.albums.iter().enumerate() {
                let x = strip.x + i as f32 * (TILE + 18.0);
                if x > strip.right() {
                    break;
                }
                let hover = ui.hovered(Rect::new(x, strip.y, TILE, TILE + 48.0));
                let h = ui
                    .anim
                    .to(anim::id(("strip", i)), hover as u8 as f32, anim::FAST);
                ui.pointer |= hover;
                let cover = Rect::new(x, strip.y + 4.0 - 4.0 * h, TILE, TILE);
                if h > 0.01 {
                    ui.c.glow(
                        Rect::new(cover.x + 4.0, cover.y + 10.0, cover.w - 8.0, cover.h - 6.0),
                        Color::BLACK.alpha(0.55 * h),
                        t.radius,
                        16.0,
                    );
                }
                ui.cover(
                    cover,
                    al.cover.as_ref(),
                    khoostic_library::CoverSize::Large,
                    t.radius,
                    &al.name,
                );
                ui.label(
                    &al.name,
                    x,
                    Rect::new(x, strip.y + TILE + 6.0, TILE, 20.0),
                    TILE,
                    t.font_size - 0.5,
                    Weight::Medium,
                    t.fg,
                );
                let artist = al.artist.clone().unwrap_or_default();
                ui.label(
                    &artist,
                    x,
                    Rect::new(x, strip.y + TILE + 26.0, TILE, 18.0),
                    TILE,
                    12.0,
                    Weight::Regular,
                    t.dim,
                );
                if hover && ui.input.pressed {
                    self.navigate(View::SpotifyAlbum {
                        id: al.id.clone(),
                        name: al.name.clone(),
                    });
                }
            }
            ui.c.pop_clip();
            list = Rect::new(body.x, body.y + strip_h, body.w, body.h - strip_h);
        }
        if !res.tracks.is_empty() {
            let tracks = Arc::new(res.tracks.clone());
            let list = crate::views::column_header(ui, list, true, true);
            let style = crate::views::TrackListStyle {
                covers: true,
                album_column: true,
                track_numbers: false,
            };
            let action = self.track_list(ui, list, "sp-search", &tracks, style, snap, a);
            self.apply_row_action(action, &tracks, player);
        }
    }
}
