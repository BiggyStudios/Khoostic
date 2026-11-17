//! Library pages: albums, an album, artists, an artist, all songs.

use std::sync::Arc;

use crate::audio::AnalysisFrame;
use crate::gui::app::{Gui, View, count, fmt_long};
use crate::gui::render::{Rect, Weight};
use crate::gui::ui::icons::Icon;
use crate::gui::ui::{Ui, anim};
use crate::gui::views::lists::{TrackListStyle, column_header, row_bg, scrollbar_rows};
use crate::gui::views::{button_icon, empty_state, primary_button};
use crate::library::{Album, CoverSize};
use crate::model::Track;
use crate::player::{Player, PlayerCommand as Cmd, PlayerSnapshot};

impl Gui {
    pub(crate) fn albums_view(&mut self, ui: &mut Ui, r: Rect, player: &Player) {
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
            && v == view
        {
            return tracks.clone();
        }
        let tracks = Arc::new(load(self));
        self.cache.detail = Some((view.clone(), tracks.clone()));
        self.lists.remove("album");
        tracks
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn album_view(
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
        ui.c.glow(cover, ui.theme.shadow(0.55), t.radius, 22.0);
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
            self.play_tracks(tracks.to_vec(), player, false);
        }
        let mut bx = x + 120.0;
        for (i, label) in ["Shuffle", "Add to queue"].iter().enumerate() {
            let (clicked, bw) = ui.pill_icon(label, button_icon(label), bx, by + 3.0, false);
            bx += bw + 8.0;
            if clicked {
                if i == 0 {
                    self.play_tracks(tracks.to_vec(), player, true);
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

    pub(crate) fn artists_view(&mut self, ui: &mut Ui, r: Rect) {
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
            row_bg(ui, anim::id(("artists", i)), row, hover, i == ls.selected);
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

    pub(crate) fn artist_view(&mut self, ui: &mut Ui, r: Rect, player: &Player, name: &str) {
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
        ui.c.glow(avatar, ui.theme.shadow(0.5), 66.0, 18.0);
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
        let (shuffle, _) = ui.pill_icon("Shuffle", Some(Icon::Shuffle), x + 120.0, by + 3.0, false);
        if play || shuffle {
            let tracks = self.library.artist_tracks(name);
            self.play_tracks(tracks, player, shuffle);
        }
        // The same artist on Spotify (found by searching their name): link to the profile.
        let want = crate::model::matching::normalize(name);
        let on_spotify = self
            .spotify_search(name)
            .filter(|(exact, _)| *exact)
            .and_then(|(_, res)| {
                res.artists
                    .iter()
                    .find(|a| crate::model::matching::normalize(&a.name) == want)
                    .map(|a| a.id.clone())
            });
        if let Some(id) = on_spotify {
            let (open, _) = ui.pill_icon(
                "Spotify profile",
                Some(Icon::Spotify),
                x + 240.0,
                by + 3.0,
                false,
            );
            if open {
                self.open_artist(name, Some(&id));
            }
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

    pub(crate) fn songs_view(
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
            Some(0) => self.play_tracks(songs.to_vec(), player, true),
            Some(_) => self.play_tracks(songs.to_vec(), player, false),
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
}
