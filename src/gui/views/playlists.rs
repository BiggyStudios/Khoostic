//! Playlists (local and Spotify), liked songs and Spotify albums.

use std::sync::Arc;
use std::time::Duration;

use crate::gui::app::{Gui, View, count, fmt_long};
use crate::gui::render::{Color, Rect, Weight};
use crate::gui::ui::Ui;
use crate::gui::ui::icons::Icon;
use crate::gui::views::grid::Tile;
use crate::model::{CoverRef, Track};
use crate::player::{Player, PlayerCommand as Cmd, PlayerSnapshot};

impl Gui {
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
            return crate::gui::views::empty_state(ui, body, "No playlists yet", hint);
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
        a: &crate::audio::AnalysisFrame,
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
            crate::library::CoverSize::Large,
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
            if crate::gui::views::primary_button(ui, Rect::new(x, by, 108.0, 40.0), "Play") {
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
            let list = crate::gui::views::lists::column_header(ui, list, true, true);
            let style = crate::gui::views::lists::TrackListStyle {
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
        a: &crate::audio::AnalysisFrame,
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
        a: &crate::audio::AnalysisFrame,
    ) {
        let tracks = self.liked_tracks();
        if self
            .collection_page(
                ui,
                r,
                player,
                snap,
                a,
                "PLAYLIST",
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
        a: &crate::audio::AnalysisFrame,
        id: &str,
        name: &str,
    ) {
        let tracks = self.ensure_tracks(&format!("al:{id}"));
        // The cover from wherever the album was found (until its songs, which carry it, load).
        let in_search = self.sp.searches.values().flat_map(|s| s.albums.iter());
        let on_artists = self
            .sp
            .artists
            .values()
            .flat_map(|p| p.popular_releases.iter().chain(&p.albums).chain(&p.singles));
        let cover = in_search
            .chain(on_artists)
            .find(|al| al.id == id)
            .and_then(|al| al.cover.clone());
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
        a: &crate::audio::AnalysisFrame,
        id: i64,
    ) {
        let Some((p, tracks)) = self.sp.local.iter().find(|(p, _)| p.id == id).cloned() else {
            return crate::gui::views::empty_state(
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
}
