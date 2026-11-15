//! Content pages: library (albums, album, artists, artist, songs, search), queue, files,
//! visualizer. Shared building blocks: track lists and cover grids.

// Row loops index by position: the index drives layout and selection, not just access.
#![allow(clippy::needless_range_loop)]

use std::sync::Arc;

use khoostic_audio::{AnalysisFrame, BANDS, PlaybackState};
use khoostic_core::{Track, TrackLocator};
use khoostic_library::{Album, CoverSize};
use khoostic_player::browse::EntryKind;
use khoostic_player::{Player, PlayerCommand as Cmd, PlayerSnapshot};

use crate::app::{Gui, ListState, ROW_H, View, count, fmt_long, fmt_time, local_path};
use crate::canvas::{Color, Rect, Weight};
use crate::ui::{Key, KeyPress, Ui};

/// What a click/key did to a row in a track list.
enum RowAction {
    None,
    Play(usize),
    Enqueue(usize),
}

struct TrackListStyle {
    covers: bool,
    album_column: bool,
    /// Show tag track numbers instead of list positions (album pages).
    track_numbers: bool,
}

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
        match self.view.clone() {
            View::Albums => self.albums_view(ui, r, player),
            View::Album { name, artist } => {
                self.album_view(ui, r, player, snap, a, &name, artist.as_deref())
            }
            View::Artists => self.artists_view(ui, r),
            View::Artist(name) => self.artist_view(ui, r, player, &name),
            View::Songs => self.songs_view(ui, r, player, snap, a),
            View::Search => self.search_view(ui, r, player, snap, a),
            View::Queue => self.queue_view(ui, r, player, snap, a),
            View::Files => self.files_view(ui, r, player),
            View::Visualizer => self.visualizer(ui, r, a, dt),
        }
    }

    // ---- page header -------------------------------------------------------------------

    fn header(&mut self, ui: &mut Ui, r: Rect, title: &str, subtitle: &str) -> Rect {
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
    fn header_buttons(&mut self, ui: &mut Ui, r: Rect, labels: &[&str]) -> Option<usize> {
        let t = ui.theme;
        let mut x = r.right();
        let mut clicked = None;
        for (i, label) in labels.iter().enumerate().rev() {
            let w = ui.measure(label, t.font_size - 1.0, Weight::Medium) + 24.0;
            x -= w;
            if ui.pill(label, x, r.y + 6.0, false).0 {
                clicked = Some(i);
            }
            x -= 8.0;
        }
        clicked
    }

    // ---- shared: track lists -------------------------------------------------------------

    fn track_list(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        key: &'static str,
        tracks: &[Track],
        style: TrackListStyle,
        snap: &PlayerSnapshot,
        a: &AnalysisFrame,
    ) -> RowAction {
        let t = ui.theme.clone();
        let mut ls = self.lists.get(key).cloned().unwrap_or_default();
        ls.height = r.h;
        ls.clamp(tracks.len(), ROW_H);
        ls.selected = ls.selected.min(tracks.len().saturating_sub(1));
        if ui.hovered(r) {
            ls.target += ui.input.scroll;
        }
        let playing = snap.current.as_ref().map(|e| &e.track.locator);
        let mut action = RowAction::None;
        ui.c.push_clip(r);
        let first = (ls.scroll / ROW_H).floor().max(0.0) as usize;
        let rows = (r.h / ROW_H).ceil() as usize + 2;
        let cols = Cols::new(r.w, style.covers, style.album_column);
        for i in first..(first + rows).min(tracks.len()) {
            let tr = &tracks[i];
            let row = Rect::new(r.x, r.y + i as f32 * ROW_H - ls.scroll, r.w, ROW_H - 2.0);
            let hover = ui.hovered(row) && ui.hovered(r);
            let is_playing = playing == Some(&tr.locator);
            if i == ls.selected {
                ui.c.rect(row, t.surface_hover, t.radius);
            } else if hover {
                ui.c.rect(row, t.surface_hover.alpha(0.5), t.radius);
            }
            let num = if style.track_numbers {
                tr.track_number.map(|n| n.to_string()).unwrap_or_default()
            } else {
                (i + 1).to_string()
            };
            draw_track_row(ui, row, tr, &num, is_playing, snap.state, a, &cols);
            if hover && ui.input.pressed {
                ls.selected = i;
                if ui.input.double_click {
                    action = RowAction::Play(i);
                }
            }
            if hover && ui.input.right_pressed {
                action = RowAction::Enqueue(i);
            }
        }
        ui.c.pop_clip();
        scrollbar(ui, r, &ls, tracks.len());
        self.lists.insert(key, ls);
        action
    }

    fn apply_row_action(&mut self, action: RowAction, tracks: &Arc<Vec<Track>>, player: &Player) {
        match action {
            RowAction::None => {}
            RowAction::Play(i) => player.send(Cmd::PlayTracks {
                tracks: tracks.to_vec(),
                start: i,
            }),
            RowAction::Enqueue(i) => {
                if let Some(t) = tracks.get(i) {
                    self.toast(format!("Added “{}” to the queue", t.title));
                    player.send(Cmd::Enqueue(vec![t.clone()]));
                }
            }
        }
    }

    // ---- shared: cover grids ------------------------------------------------------------

    /// A responsive grid of album tiles. Returns (clicked album, play-button album).
    fn album_grid(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        key: &'static str,
        albums: &[Album],
    ) -> (Option<usize>, Option<usize>) {
        let t = ui.theme.clone();
        const MIN_TILE: f32 = 168.0;
        const GAP: f32 = 22.0;
        let cols = (((r.w + GAP) / (MIN_TILE + GAP)).floor() as usize).max(1);
        let tile = (r.w - GAP * (cols - 1) as f32) / cols as f32;
        let row_h = tile + 64.0;
        let rows = albums.len().div_ceil(cols);

        let mut ls = self.lists.get(key).cloned().unwrap_or_default();
        ls.cols = cols;
        ls.height = r.h;
        ls.clamp(rows, row_h);
        ls.selected = ls.selected.min(albums.len().saturating_sub(1));
        if ui.hovered(r) {
            ls.target += ui.input.scroll;
        }
        let (mut open, mut play) = (None, None);
        // Clip a little wider than the grid so selection halos aren't cut off.
        ui.c.push_clip(Rect::new(r.x - 10.0, r.y - 10.0, r.w + 20.0, r.h + 10.0));
        let first_row = (ls.scroll / row_h).floor().max(0.0) as usize;
        let vis_rows = (r.h / row_h).ceil() as usize + 2;
        for i in first_row * cols..((first_row + vis_rows) * cols).min(albums.len()) {
            let al = &albums[i];
            let (row, col) = (i / cols, i % cols);
            let x = r.x + col as f32 * (tile + GAP);
            let y = r.y + row as f32 * row_h - ls.scroll;
            let cover = Rect::new(x, y, tile, tile);
            let tile_rect = Rect::new(x, y, tile, row_h - 10.0);
            let hover = ui.hovered(tile_rect) && ui.hovered(r);
            if i == ls.selected {
                ui.c.rect(
                    Rect::new(x - 8.0, y - 8.0, tile + 16.0, row_h - 2.0),
                    t.surface_hover.alpha(0.8),
                    t.radius + 4.0,
                );
            }
            if hover {
                ui.c.glow(cover, Color::BLACK.alpha(0.5), t.radius, 12.0);
            }
            ui.cover(
                cover,
                al.cover.as_ref(),
                CoverSize::Large,
                t.radius,
                &al.name,
            );
            ui.label(
                &al.name,
                x,
                Rect::new(x, y + tile + 8.0, tile, 22.0),
                tile,
                t.font_size,
                Weight::Medium,
                t.fg,
            );
            let sub = match (&al.artist, al.year) {
                (Some(a), Some(y)) => format!("{a} · {y}"),
                (Some(a), None) => a.clone(),
                (None, Some(y)) => y.to_string(),
                (None, None) => count(al.tracks as usize, "song"),
            };
            ui.label(
                &sub,
                x,
                Rect::new(x, y + tile + 30.0, tile, 20.0),
                tile,
                t.font_size - 1.5,
                Weight::Regular,
                t.dim,
            );
            let mut on_play = false;
            if hover {
                // Hover play button in the cover's corner.
                let b = Rect::new(cover.right() - 54.0, cover.bottom() - 54.0, 44.0, 44.0);
                let bh = ui.hovered(b);
                ui.c.glow(b, Color::BLACK.alpha(0.45), 22.0, 8.0);
                ui.c.circle(
                    b.x + 22.0,
                    b.y + 22.0,
                    22.0,
                    if bh { t.accent } else { t.accent.alpha(0.92) },
                );
                ui.c.triangle(
                    Rect::new(b.x + 17.0, b.y + 14.0, 14.0, 16.0),
                    t.background,
                    true,
                    1.5,
                );
                on_play = bh;
            }
            if hover && ui.input.pressed {
                ls.selected = i;
                if on_play {
                    play = Some(i);
                } else {
                    open = Some(i);
                }
            }
        }
        ui.c.pop_clip();
        scrollbar_rows(ui, r, &ls, rows, row_h);
        self.lists.insert(key, ls);
        (open, play)
    }

    fn play_album(&mut self, al: &Album, player: &Player, shuffle: bool) {
        let tracks = self.library.album_tracks(&al.name, al.artist.as_deref());
        self.play_list(tracks, player, shuffle);
    }

    fn play_list(&mut self, tracks: Vec<Track>, player: &Player, shuffle: bool) {
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

    // ---- library pages ------------------------------------------------------------------

    fn albums_view(&mut self, ui: &mut Ui, r: Rect, player: &Player) {
        let albums = self.cache.albums.clone();
        let sub = if self.library.is_scanning() && albums.is_empty() {
            "Scanning your music…".to_string()
        } else {
            count(albums.len(), "album")
        };
        let body = self.header(ui, r, "Albums", &sub);
        if albums.is_empty() {
            return empty_state(
                ui,
                body,
                "No albums yet",
                "Add folders under [library] music_dirs in config.toml, or browse Files",
            );
        }
        let (open, play) = self.album_grid(ui, body, "albums", &albums);
        if let Some(i) = play {
            self.play_album(&albums[i], player, false);
        } else if let Some(i) = open {
            let al = &albums[i];
            self.navigate(View::Album {
                name: al.name.clone(),
                artist: al.artist.clone(),
            });
        }
    }

    fn detail_tracks(
        &mut self,
        view: &View,
        load: impl FnOnce(&Gui) -> Vec<Track>,
    ) -> Arc<Vec<Track>> {
        if let Some((v, tracks)) = &self.cache.detail
            && v == view {
                return tracks.clone();
            }
        let tracks = Arc::new(load(self));
        self.cache.detail = Some((view.clone(), tracks.clone()));
        self.lists.remove("album");
        tracks
    }

    #[allow(clippy::too_many_arguments)]
    fn album_view(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        player: &Player,
        snap: &PlayerSnapshot,
        a: &AnalysisFrame,
        name: &str,
        artist: Option<&str>,
    ) {
        let t = ui.theme.clone();
        let view = self.view.clone();
        let tracks = self.detail_tracks(&view, |g| g.library.album_tracks(name, artist));
        let album = self
            .cache
            .albums
            .iter()
            .find(|al| al.name.eq_ignore_ascii_case(name) && al.artist.as_deref() == artist)
            .cloned();
        let cover_ref = album
            .as_ref()
            .and_then(|al| al.cover.clone())
            .or_else(|| tracks.first().and_then(|t| t.cover.clone()));

        let size = 196.0f32.min(r.w * 0.3);
        let cover = Rect::new(r.x, r.y, size, size);
        ui.c.glow(cover, Color::BLACK.alpha(0.55), t.radius, 22.0);
        ui.cover(
            cover,
            cover_ref.as_ref(),
            CoverSize::Large,
            t.radius + 2.0,
            name,
        );
        let x = cover.right() + 28.0;
        let w = r.right() - x;
        ui.label(
            "ALBUM",
            x,
            Rect::new(x, r.y + 30.0, w, 18.0),
            w,
            11.0,
            Weight::Bold,
            t.dim,
        );
        ui.label(
            name,
            x,
            Rect::new(x, r.y + 50.0, w, 48.0),
            w,
            34.0,
            Weight::Bold,
            t.fg,
        );
        let total: std::time::Duration = tracks.iter().filter_map(|t| t.duration).sum();
        let mut meta = Vec::new();
        if let Some(a) = artist {
            meta.push(a.to_string());
        }
        if let Some(y) = album.as_ref().and_then(|al| al.year) {
            meta.push(y.to_string());
        }
        meta.push(format!(
            "{}, {}",
            count(tracks.len(), "song"),
            fmt_long(total)
        ));
        let meta_row = Rect::new(x, r.y + 104.0, w, 22.0);
        ui.label(
            &meta.join("  ·  "),
            x,
            meta_row,
            w,
            t.font_size,
            Weight::Regular,
            t.dim,
        );
        if let Some(a) = artist {
            let aw = ui.measure(a, t.font_size, Weight::Regular);
            let link = Rect::new(x, meta_row.y, aw, meta_row.h);
            if ui.hovered(link) {
                ui.c.rect(Rect::new(x, meta_row.bottom() - 3.0, aw, 1.0), t.dim, 0.0);
            }
            if ui.clicked(link) {
                self.navigate(View::Artist(a.to_string()));
            }
        }
        // Buttons
        let by = r.y + 142.0;
        if primary_button(ui, Rect::new(x, by, 108.0, 40.0), "Play") {
            self.play_list(tracks.to_vec(), player, false);
        }
        let mut bx = x + 120.0;
        for (i, label) in ["Shuffle", "Add to queue"].iter().enumerate() {
            let (clicked, bw) = ui.pill(label, bx, by + 5.0, false);
            bx += bw + 8.0;
            if clicked {
                if i == 0 {
                    self.play_list(tracks.to_vec(), player, true);
                } else {
                    self.toast(format!(
                        "Added {} to the queue",
                        count(tracks.len(), "song")
                    ));
                    player.send(Cmd::Enqueue(tracks.to_vec()));
                }
            }
        }

        let list = Rect::new(r.x, r.y + size + 28.0, r.w, (r.h - size - 28.0).max(0.0));
        let style = TrackListStyle {
            covers: false,
            album_column: false,
            track_numbers: true,
        };
        let action = self.track_list(ui, list, "album", &tracks, style, snap, a);
        self.apply_row_action(action, &tracks, player);
    }

    fn artists_view(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.theme.clone();
        let artists = self.cache.artists.clone();
        let body = self.header(ui, r, "Artists", &count(artists.len(), "artist"));
        if artists.is_empty() {
            return empty_state(ui, body, "No artists yet", "Tagged music will show up here");
        }
        const RH: f32 = 64.0;
        let mut ls = self.lists.get("artists").cloned().unwrap_or_default();
        ls.height = body.h;
        ls.clamp(artists.len(), RH);
        if ui.hovered(body) {
            ls.target += ui.input.scroll;
        }
        let mut open = None;
        ui.c.push_clip(body);
        let first = (ls.scroll / RH).floor().max(0.0) as usize;
        for i in first..(first + (body.h / RH) as usize + 2).min(artists.len()) {
            let ar = &artists[i];
            let row = Rect::new(body.x, body.y + i as f32 * RH - ls.scroll, body.w, RH - 4.0);
            let hover = ui.hovered(row) && ui.hovered(body);
            if i == ls.selected {
                ui.c.rect(row, t.surface_hover, t.radius);
            } else if hover {
                ui.c.rect(row, t.surface_hover.alpha(0.5), t.radius);
            }
            let avatar = Rect::new(row.x + 10.0, row.y + 6.0, 48.0, 48.0);
            ui.cover(avatar, ar.cover.as_ref(), CoverSize::Small, 24.0, &ar.name);
            let tx = avatar.right() + 16.0;
            ui.label(
                &ar.name,
                tx,
                Rect::new(tx, row.y + 8.0, row.w, 24.0),
                row.w - 100.0,
                t.font_size + 1.0,
                Weight::Medium,
                t.fg,
            );
            let sub = match ar.albums {
                0 => count(ar.tracks as usize, "song"),
                n => format!(
                    "{} · {}",
                    count(n as usize, "album"),
                    count(ar.tracks as usize, "song")
                ),
            };
            ui.label(
                &sub,
                tx,
                Rect::new(tx, row.y + 31.0, row.w, 20.0),
                row.w - 100.0,
                12.5,
                Weight::Regular,
                t.dim,
            );
            if hover && ui.input.pressed {
                ls.selected = i;
                open = Some(i);
            }
        }
        ui.c.pop_clip();
        scrollbar_rows(ui, body, &ls, artists.len(), RH);
        self.lists.insert("artists", ls);
        if let Some(i) = open {
            self.navigate(View::Artist(artists[i].name.clone()));
        }
    }

    fn artist_view(&mut self, ui: &mut Ui, r: Rect, player: &Player, name: &str) {
        let t = ui.theme.clone();
        let albums: Vec<Album> = self
            .cache
            .albums
            .iter()
            .filter(|al| {
                al.artist
                    .as_deref()
                    .is_some_and(|a| a.eq_ignore_ascii_case(name))
            })
            .cloned()
            .collect();
        let info = self
            .cache
            .artists
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(name))
            .cloned();

        let avatar = Rect::new(r.x, r.y, 132.0, 132.0);
        ui.c.glow(avatar, Color::BLACK.alpha(0.5), 66.0, 18.0);
        ui.cover(
            avatar,
            info.as_ref().and_then(|i| i.cover.as_ref()),
            CoverSize::Large,
            66.0,
            name,
        );
        let x = avatar.right() + 28.0;
        let w = r.right() - x;
        ui.label(
            "ARTIST",
            x,
            Rect::new(x, r.y + 14.0, w, 18.0),
            w,
            11.0,
            Weight::Bold,
            t.dim,
        );
        ui.label(
            name,
            x,
            Rect::new(x, r.y + 34.0, w, 48.0),
            w,
            34.0,
            Weight::Bold,
            t.fg,
        );
        if let Some(i) = &info {
            let sub = format!(
                "{} · {}",
                count(i.albums as usize, "album"),
                count(i.tracks as usize, "song")
            );
            ui.label(
                &sub,
                x,
                Rect::new(x, r.y + 84.0, w, 20.0),
                w,
                t.font_size,
                Weight::Regular,
                t.dim,
            );
        }
        let by = r.y + 110.0;
        let play = primary_button(ui, Rect::new(x, by, 108.0, 40.0), "Play");
        let (shuffle, _) = ui.pill("Shuffle", x + 120.0, by + 5.0, false);
        if play || shuffle {
            let tracks = self.library.artist_tracks(name);
            self.play_list(tracks, player, shuffle);
        }

        let grid = Rect::new(r.x, r.y + 166.0, r.w, (r.h - 166.0).max(0.0));
        let (open, play) = self.album_grid(ui, grid, "artist", &albums);
        if let Some(i) = play {
            self.play_album(&albums[i], player, false);
        } else if let Some(i) = open {
            self.navigate(View::Album {
                name: albums[i].name.clone(),
                artist: albums[i].artist.clone(),
            });
        }
    }

    fn songs_view(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        player: &Player,
        snap: &PlayerSnapshot,
        a: &AnalysisFrame,
    ) {
        let songs = self.cache.songs.clone();
        let total: std::time::Duration = songs.iter().filter_map(|t| t.duration).sum();
        let sub = format!("{}, {}", count(songs.len(), "song"), fmt_long(total));
        let body = self.header(ui, r, "Songs", &sub);
        if songs.is_empty() {
            return empty_state(ui, body, "No songs yet", "Your scanned music shows up here");
        }
        match self.header_buttons(ui, r, &["Shuffle all", "Play all"]) {
            Some(0) => self.play_list(songs.to_vec(), player, true),
            Some(_) => self.play_list(songs.to_vec(), player, false),
            None => {}
        }
        let list = column_header(ui, body, true, true);
        let style = TrackListStyle {
            covers: true,
            album_column: true,
            track_numbers: false,
        };
        let action = self.track_list(ui, list, "songs", &songs, style, snap, a);
        self.apply_row_action(action, &songs, player);
    }

    fn search_view(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        player: &Player,
        snap: &PlayerSnapshot,
        a: &AnalysisFrame,
    ) {
        let t = ui.theme.clone();
        let q = self.search.text.trim().to_string();
        let results = match &self.cache.search {
            Some((cached_q, res)) if *cached_q == q => res.clone(),
            _ => {
                let res = Arc::new(self.library.search(&q, 300));
                self.cache.search = Some((q.clone(), res.clone()));
                res
            }
        };
        let ql = q.to_lowercase();
        let albums: Vec<Album> = self
            .cache
            .albums
            .iter()
            .filter(|al| {
                al.name.to_lowercase().contains(&ql)
                    || al
                        .artist
                        .as_deref()
                        .is_some_and(|a| a.to_lowercase().contains(&ql))
            })
            .take(12)
            .cloned()
            .collect();
        let body = self.header(
            ui,
            r,
            &format!("“{q}”"),
            &format!(
                "{} · {}",
                count(albums.len(), "album"),
                count(results.len(), "song")
            ),
        );
        if results.is_empty() && albums.is_empty() {
            return empty_state(
                ui,
                body,
                "No matches",
                "Search looks at titles, artists and albums",
            );
        }
        let mut list = body;
        if !albums.is_empty() {
            // One row of matching albums, then the songs.
            let strip_h = 236.0f32.min(body.h * 0.45);
            let strip = Rect::new(body.x, body.y, body.w, strip_h);
            const TILE: f32 = 150.0;
            ui.c.push_clip(strip);
            for (i, al) in albums.iter().enumerate() {
                let x = strip.x + i as f32 * (TILE + 18.0);
                if x > strip.right() {
                    break;
                }
                let cover = Rect::new(x, strip.y, TILE, TILE);
                let hover = ui.hovered(Rect::new(x, strip.y, TILE, TILE + 48.0));
                if hover {
                    ui.c.glow(cover, Color::BLACK.alpha(0.5), t.radius, 12.0);
                }
                ui.cover(
                    cover,
                    al.cover.as_ref(),
                    CoverSize::Large,
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
                    self.navigate(View::Album {
                        name: al.name.clone(),
                        artist: al.artist.clone(),
                    });
                }
            }
            ui.c.pop_clip();
            list = Rect::new(body.x, body.y + strip_h, body.w, body.h - strip_h);
        }
        if !results.is_empty() {
            let list = column_header(ui, list, true, true);
            let style = TrackListStyle {
                covers: true,
                album_column: true,
                track_numbers: false,
            };
            let action = self.track_list(ui, list, "search", &results, style, snap, a);
            self.apply_row_action(action, &results, player);
        }
    }

    // ---- player pages -------------------------------------------------------------------

    fn queue_view(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        player: &Player,
        snap: &PlayerSnapshot,
        a: &AnalysisFrame,
    ) {
        let t = ui.theme.clone();
        let entries: Vec<_> = player.queue().read().entries().to_vec();
        let total: std::time::Duration = entries.iter().filter_map(|e| e.track.duration).sum();
        let sub = if entries.is_empty() {
            String::new()
        } else {
            format!("{}, {}", count(entries.len(), "track"), fmt_long(total))
        };
        let body = self.header(ui, r, "Queue", &sub);
        if entries.is_empty() {
            return empty_state(
                ui,
                body,
                "The queue is empty",
                "Double-click a song to play it, or right-click to queue it",
            );
        }
        if self.header_buttons(ui, r, &["Clear"]).is_some() {
            player.send(Cmd::Clear);
        }
        let list = column_header(ui, body, true, true);
        let mut ls = self.lists.get("queue").cloned().unwrap_or_default();
        ls.height = list.h;
        ls.clamp(entries.len(), ROW_H);
        ls.selected = ls.selected.min(entries.len().saturating_sub(1));
        if ui.hovered(list) {
            ls.target += ui.input.scroll;
        }
        let playing = snap.current.as_ref().map(|e| e.id);
        let cols = Cols::new(list.w, true, true);
        let mut clicked = None;
        ui.c.push_clip(list);
        let first = (ls.scroll / ROW_H).floor().max(0.0) as usize;
        for i in first..(first + (list.h / ROW_H) as usize + 2).min(entries.len()) {
            let e = &entries[i];
            let row = Rect::new(
                list.x,
                list.y + i as f32 * ROW_H - ls.scroll,
                list.w,
                ROW_H - 2.0,
            );
            let hover = ui.hovered(row) && ui.hovered(list);
            if i == ls.selected {
                ui.c.rect(row, t.surface_hover, t.radius);
            } else if hover {
                ui.c.rect(row, t.surface_hover.alpha(0.5), t.radius);
            }
            let is_playing = Some(e.id) == playing;
            let mut track = (*e.track).clone();
            if is_playing && track.duration.is_none() {
                track.duration = snap.duration;
            }
            draw_track_row(
                ui,
                row,
                &track,
                &(i + 1).to_string(),
                is_playing,
                snap.state,
                a,
                &cols,
            );
            if hover && ui.input.pressed {
                ls.selected = i;
                clicked = Some((e.id, ui.input.double_click));
            }
            if hover && ui.input.right_pressed {
                player.send(Cmd::Remove(vec![e.id]));
            }
        }
        ui.c.pop_clip();
        scrollbar(ui, list, &ls, entries.len());
        self.lists.insert("queue", ls);
        if let Some((id, true)) = clicked {
            player.send(Cmd::PlayEntry(id));
        }
    }

    fn files_view(&mut self, ui: &mut Ui, r: Rect, player: &Player) {
        let t = ui.theme.clone();
        let dir = self.browser.dir().display().to_string();
        let name = self
            .browser
            .dir()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.clone());
        let body = self.header(ui, r, &name, &dir);
        match self.header_buttons(ui, r, &["Music", "Up", "Add all"]) {
            Some(0) => {
                if let Some(d) = self.music_dirs.first().cloned()
                    && self.browser.open(&d).is_ok() {
                        self.lists.remove("files");
                    }
            }
            Some(1) => self.files_up(),
            Some(_) => {
                let tracks = self.library.tracks_for_paths(&[self.browser.dir()]);
                self.toast(format!(
                    "Added {} to the queue",
                    count(tracks.len(), "track")
                ));
                player.send(Cmd::Enqueue(tracks));
            }
            None => {}
        }
        let len = self.browser.entries().len();
        if len == 0 {
            let msg = self
                .browser
                .error()
                .unwrap_or("No folders or audio files here")
                .to_owned();
            return empty_state(ui, body, &msg, "Backspace goes up a folder");
        }
        let mut ls = self.lists.get("files").cloned().unwrap_or_default();
        ls.height = body.h;
        ls.clamp(len, ROW_H);
        ls.selected = ls.selected.min(len - 1);
        if ui.hovered(body) {
            ls.target += ui.input.scroll;
        }
        let mut clicked = None;
        ui.c.push_clip(body);
        let first = (ls.scroll / ROW_H).floor().max(0.0) as usize;
        for i in first..(first + (body.h / ROW_H) as usize + 2).min(len) {
            let e = &self.browser.entries()[i];
            let row = Rect::new(
                body.x,
                body.y + i as f32 * ROW_H - ls.scroll,
                body.w,
                ROW_H - 2.0,
            );
            let hover = ui.hovered(row) && ui.hovered(body);
            if i == ls.selected {
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
            ui.label(
                &e.name,
                row.x + 46.0,
                row,
                row.w - 60.0,
                t.font_size,
                Weight::Medium,
                t.fg,
            );
            if hover && ui.input.pressed {
                clicked = Some((i, ui.input.double_click, false));
            }
            if hover && ui.input.right_pressed {
                clicked = Some((i, false, true));
            }
        }
        ui.c.pop_clip();
        scrollbar(ui, body, &ls, len);
        self.lists.insert("files", ls);
        if let Some((i, dbl, right)) = clicked {
            self.list("files").selected = i;
            if right {
                self.files_add(i, player, false);
            } else if dbl {
                self.files_open(i, player);
            }
        }
    }

    fn files_open(&mut self, i: usize, player: &Player) {
        let Some(e) = self.browser.entries().get(i).cloned() else {
            return;
        };
        match e.kind {
            EntryKind::Dir => match self.browser.open(&e.path) {
                Ok(()) => {
                    self.lists.remove("files");
                }
                Err(err) => self.toast(err),
            },
            EntryKind::Audio => {
                let tracks = self.library.tracks_for_paths(&[&e.path]);
                player.send(Cmd::EnqueueAndPlay(tracks));
            }
        }
    }

    fn files_add(&mut self, i: usize, player: &Player, next: bool) {
        let Some(e) = self.browser.entries().get(i).cloned() else {
            return;
        };
        let tracks = self.library.tracks_for_paths(&[&e.path]);
        if tracks.is_empty() {
            return self.toast("Nothing playable there");
        }
        let n = count(tracks.len(), "track");
        self.toast(if next {
            format!("Playing {n} next")
        } else {
            format!("Added {n} to the queue")
        });
        player.send(if next {
            Cmd::PlayNext(tracks)
        } else {
            Cmd::Enqueue(tracks)
        });
    }

    fn files_up(&mut self) {
        if let Some(i) = self.browser.parent() {
            let l = self.list("files");
            l.selected = i;
            l.target = (i as f32 * ROW_H - l.height / 2.0).max(0.0);
        }
    }

    fn visualizer(&mut self, ui: &mut Ui, r: Rect, a: &AnalysisFrame, dt: f32) {
        let t = ui.theme.clone();
        self.pulse = (self.pulse - dt * 2.5).max(a.beat);
        let (cx, cy) = r.center();
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
            ui.c.gradient(
                Rect::new(x, base - h, bar_w, h),
                t.gradient(v),
                t.gradient(0.0).alpha(0.5),
                bar_w / 2.0,
            );
            if a.peaks[i] > 0.02 {
                let peak = base - a.peaks[i] * max_h - 6.0;
                ui.c.rect(
                    Rect::new(x, peak, bar_w, 3.0),
                    t.gradient(v).alpha(0.8),
                    1.5,
                );
            }
        }
    }

    // ---- keyboard for the current page --------------------------------------------------

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
                self.cache.search.as_ref().map_or(0, |s| s.1.len()),
                false,
            ),
            View::Queue => ("queue", player.queue().read().len(), false),
            View::Files => ("files", self.browser.entries().len(), false),
            View::Visualizer => return,
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
            let i = i.min(len - 1);
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
            (Key::Enter, View::Queue) => {
                let id = player.queue().read().entries().get(sel).map(|e| e.id);
                if let Some(id) = id {
                    player.send(Cmd::PlayEntry(id));
                }
            }
            (Key::Delete | Key::Char('x'), View::Queue) => {
                let id = player.queue().read().entries().get(sel).map(|e| e.id);
                if let Some(id) = id {
                    player.send(Cmd::Remove(vec![id]));
                }
            }
            (Key::Enter | Key::Char('l'), View::Files) => self.files_open(sel, player),
            (Key::Backspace | Key::Char('h'), View::Files) => self.files_up(),
            (Key::Char('a'), View::Files) => self.files_add(sel, player, false),
            (Key::Char('A'), View::Files) => self.files_add(sel, player, true),
            (Key::Enter | Key::Char('a') | Key::Char('A'), _) => {
                let tracks = match &self.view {
                    View::Album { .. } => self.cache.detail.as_ref().map(|d| d.1.clone()),
                    View::Songs => Some(self.cache.songs.clone()),
                    View::Search => self.cache.search.as_ref().map(|s| s.1.clone()),
                    _ => None,
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
}

// ---- drawing helpers --------------------------------------------------------------------

/// Column layout for track rows.
struct Cols {
    num: f32,
    cover: f32,
    title: f32,
    artist: f32,
    album: f32,
}

impl Cols {
    fn new(w: f32, covers: bool, album: bool) -> Self {
        let num = 36.0;
        let cover = if covers { 46.0 } else { 0.0 };
        let flex = (w - num - cover - 12.0 - 70.0).max(0.0);
        if album {
            Self {
                num,
                cover,
                title: flex * 0.42,
                artist: flex * 0.28,
                album: flex * 0.30,
            }
        } else {
            Self {
                num,
                cover,
                title: flex * 0.62,
                artist: flex * 0.38,
                album: 0.0,
            }
        }
    }
}

/// Column titles above a track list; returns the list area below them.
fn column_header(ui: &mut Ui, r: Rect, covers: bool, album: bool) -> Rect {
    let t = ui.theme;
    let cols = Cols::new(r.w, covers, album);
    let row = Rect::new(r.x, r.y, r.w, 24.0);
    let x = r.x + cols.num + cols.cover + 12.0;
    ui.label_right(
        "#",
        r.x + cols.num - 8.0,
        row,
        11.0,
        Weight::Bold,
        t.dim.alpha(0.8),
    );
    ui.label(
        "TITLE",
        x,
        row,
        cols.title,
        11.0,
        Weight::Bold,
        t.dim.alpha(0.8),
    );
    ui.label(
        "ARTIST",
        x + cols.title,
        row,
        cols.artist,
        11.0,
        Weight::Bold,
        t.dim.alpha(0.8),
    );
    if album {
        ui.label(
            "ALBUM",
            x + cols.title + cols.artist,
            row,
            cols.album,
            11.0,
            Weight::Bold,
            t.dim.alpha(0.8),
        );
    }
    ui.label_right(
        "TIME",
        r.right() - 12.0,
        row,
        11.0,
        Weight::Bold,
        t.dim.alpha(0.8),
    );
    ui.c.rect(Rect::new(r.x, r.y + 28.0, r.w, 1.0), t.surface_hover, 0.0);
    Rect::new(r.x, r.y + 36.0, r.w, (r.h - 36.0).max(0.0))
}

#[allow(clippy::too_many_arguments)]
fn draw_track_row(
    ui: &mut Ui,
    row: Rect,
    tr: &Track,
    num: &str,
    playing: bool,
    state: PlaybackState,
    a: &AnalysisFrame,
    cols: &Cols,
) {
    let t = ui.theme.clone();
    let numr = Rect::new(row.x, row.y, cols.num, row.h);
    if playing {
        eq_icon(ui, numr, a, state == PlaybackState::Playing, t.accent);
    } else {
        ui.label_right(num, numr.right() - 8.0, row, 12.0, Weight::Regular, t.dim);
    }
    let mut x = row.x + cols.num + 12.0;
    if cols.cover > 0.0 {
        let c = Rect::new(x - 2.0, row.y + (row.h - 34.0) / 2.0, 34.0, 34.0);
        let seed = tr.album.as_deref().unwrap_or(&tr.title);
        ui.cover(c, tr.cover.as_ref(), CoverSize::Small, 5.0, seed);
        x += cols.cover;
    }
    let title_color = if playing { t.accent } else { t.fg };
    ui.label(
        &tr.title,
        x,
        row,
        cols.title - 16.0,
        t.font_size,
        Weight::Medium,
        title_color,
    );
    x += cols.title;
    ui.label(
        &tr.artist_line(),
        x,
        row,
        cols.artist - 16.0,
        t.font_size - 1.0,
        Weight::Regular,
        t.dim,
    );
    x += cols.artist;
    if cols.album > 0.0 {
        ui.label(
            &album_of(tr),
            x,
            row,
            cols.album - 16.0,
            t.font_size - 1.0,
            Weight::Regular,
            t.dim,
        );
    }
    if let Some(d) = tr.duration {
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

/// The accent-filled main action button.
fn primary_button(ui: &mut Ui, r: Rect, label: &str) -> bool {
    let t = ui.theme;
    let hover = ui.hovered(r);
    ui.c.rect(
        r,
        if hover { t.accent } else { t.accent.alpha(0.9) },
        r.h / 2.0,
    );
    let tri = Rect::new(r.x + 20.0, r.y + r.h / 2.0 - 7.0, 12.0, 14.0);
    ui.c.triangle(tri, t.background, true, 1.0);
    ui.label(
        label,
        r.x + 42.0,
        r,
        r.w - 48.0,
        t.font_size,
        Weight::Bold,
        t.background,
    );
    ui.clicked(r)
}

fn scrollbar(ui: &mut Ui, r: Rect, ls: &ListState, rows: usize) {
    scrollbar_rows(ui, r, ls, rows, ROW_H);
}

fn scrollbar_rows(ui: &mut Ui, r: Rect, ls: &ListState, rows: usize, row_h: f32) {
    let content = rows as f32 * row_h;
    if content <= r.h {
        return;
    }
    let h = (r.h * r.h / content).max(24.0);
    let y = r.y + (ls.scroll / (content - r.h)) * (r.h - h);
    ui.c.rect(
        Rect::new(r.right() + 10.0, y, 4.0, h),
        ui.theme.dim.alpha(0.35),
        2.0,
    );
}

fn empty_state(ui: &mut Ui, r: Rect, title: &str, hint: &str) {
    let t = ui.theme;
    let mid = Rect::new(r.x, r.y + r.h * 0.28, r.w, 30.0);
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

pub(crate) fn nav_icon(ui: &mut Ui, view: &View, r: Rect, color: Color) {
    match view {
        View::Albums => {
            ui.c.outline(Rect::new(r.x, r.y, 16.0, 16.0), color, 3.0, 1.6);
            ui.c.circle(r.x + 8.0, r.y + 8.0, 2.6, color);
        }
        View::Artists => {
            ui.c.circle(r.x + 8.0, r.y + 5.0, 4.0, color);
            ui.c.rect(Rect::new(r.x + 1.5, r.y + 10.5, 13.0, 6.0), color, 3.0);
        }
        View::Songs => {
            ui.c.circle(r.x + 5.0, r.y + 12.5, 3.5, color);
            ui.c.rect(Rect::new(r.x + 7.0, r.y + 1.0, 2.0, 12.0), color, 1.0);
            ui.c.rect(Rect::new(r.x + 7.0, r.y + 1.0, 8.0, 2.5), color, 1.0);
        }
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
        View::Files => {
            ui.c.rect(Rect::new(r.x, r.y + 1.0, 7.0, 4.0), color, 1.5);
            ui.c.rect(Rect::new(r.x, r.y + 3.0, 16.0, 12.0), color, 3.0);
        }
        _ => {
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

pub(crate) fn speaker_icon(ui: &mut Ui, r: Rect, volume: f32, color: Color) {
    ui.c.rect(Rect::new(r.x, r.y + 5.0, 4.0, 6.0), color, 1.0);
    ui.c.triangle(
        Rect::new(r.x + 1.0, r.y + 2.0, 7.0, 12.0),
        color,
        false,
        0.5,
    );
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

pub(crate) fn album_of(track: &Track) -> String {
    if let Some(a) = &track.album {
        return a.clone();
    }
    // Untagged files: the containing folder is a decent stand-in.
    local_path(track)
        .and_then(|p| p.parent())
        .and_then(|d| d.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[allow(dead_code)]
fn is_local(t: &Track) -> bool {
    matches!(t.locator, TrackLocator::Local(_))
}
