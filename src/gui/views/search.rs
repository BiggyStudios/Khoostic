//! Search across the library and Spotify: album cards, then one song list.

use std::sync::Arc;

use crate::audio::AnalysisFrame;
use crate::gui::app::{Gui, SearchMode, View, count};
use crate::gui::render::{Color, Rect, Weight};
use crate::gui::ui::icons::{self, Icon};
use crate::gui::ui::{Ui, anim};
use crate::gui::views::lists::{TrackListStyle, column_header};
use crate::gui::views::{empty_state, spotify_green};
use crate::library::{Album, CoverSize};
use crate::model::Track;
use crate::player::{Player, PlayerSnapshot};

impl Gui {
    pub(crate) fn search_view(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        player: &Player,
        snap: &PlayerSnapshot,
        a: &AnalysisFrame,
    ) {
        let t = ui.theme.clone();
        let q = self.search.text.trim().to_string();
        let sp_ok = self.spotify.as_ref().is_some_and(|s| s.is_connected());
        let mode = if sp_ok {
            self.search_mode
        } else {
            SearchMode::Library
        };

        // Library matches (cached per query).
        let lib: Arc<Vec<Track>> = if mode == SearchMode::Spotify {
            Arc::new(Vec::new())
        } else {
            match &self.cache.lib_search {
                Some((cq, res)) if *cq == q => res.clone(),
                _ => {
                    let res = Arc::new(self.library.search(&q, 300));
                    self.cache.lib_search = Some((q.clone(), res.clone()));
                    res
                }
            }
        };
        let ql = q.to_lowercase();
        let lib_albums: Vec<Album> = if mode == SearchMode::Spotify {
            Vec::new()
        } else {
            self.cache
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
                .collect()
        };

        // Spotify matches (debounced, in the background; stale results are hidden).
        let sp = if mode != SearchMode::Library {
            self.spotify_search(&q);
            self.sp
                .search
                .as_ref()
                .filter(|(sq, _)| *sq == q)
                .map(|(_, res)| res.clone())
        } else {
            None
        };
        let sp_loading = mode != SearchMode::Library && sp.is_none() && !q.is_empty();

        // One list: library first, then Spotify songs you don't already have.
        let key = format!(
            "{mode:?}|{q}|{}|{}",
            lib.len(),
            sp.as_ref().map_or(0, |s| Arc::as_ptr(s) as usize)
        );
        let tracks = match &self.cache.search {
            Some((k, t)) if *k == key => t.clone(),
            _ => {
                let mut all: Vec<Track> = lib.to_vec();
                if let Some(sp) = &sp {
                    let have: std::collections::HashSet<_> =
                        all.iter().map(crate::model::matching::track_key).collect();
                    all.extend(
                        sp.tracks
                            .iter()
                            .filter(|t| !have.contains(&crate::model::matching::track_key(t)))
                            .cloned(),
                    );
                }
                let all = Arc::new(all);
                self.cache.search = Some((key, all.clone()));
                all
            }
        };
        let mut cards: Vec<SearchCard> = lib_albums
            .iter()
            .map(|al| SearchCard {
                title: al.name.clone(),
                subtitle: al.artist.clone().unwrap_or_default(),
                cover: al.cover.clone(),
                spotify: false,
                open: View::Album {
                    name: al.name.clone(),
                    artist: al.artist.clone(),
                },
            })
            .collect();
        if let Some(sp) = &sp {
            use crate::model::matching::song_key;
            // Albums you already have locally are shown once, as yours.
            let have: std::collections::HashSet<_> = lib_albums
                .iter()
                .map(|al| song_key(al.artist.as_deref().unwrap_or_default(), &al.name))
                .collect();
            let fresh = sp.albums.iter().filter(|al| {
                !have.contains(&song_key(
                    al.artist.as_deref().unwrap_or_default(),
                    &al.name,
                ))
            });
            cards.extend(fresh.take(12).map(|al| SearchCard {
                title: al.name.clone(),
                subtitle: al.artist.clone().unwrap_or_default(),
                cover: al.cover.clone(),
                spotify: true,
                open: View::SpotifyAlbum {
                    id: al.id.clone(),
                    name: al.name.clone(),
                },
            }));
        }

        let n_sp = tracks.len() - lib.len().min(tracks.len());
        let mut sub = match mode {
            SearchMode::Spotify => format!("{} on Spotify", count(tracks.len(), "song")),
            SearchMode::All => format!("{} · {} from Spotify", count(tracks.len(), "song"), n_sp),
            SearchMode::Library => count(tracks.len(), "song"),
        };
        if sp_loading {
            sub.push_str(" · searching Spotify…");
        }
        let body = self.header(ui, r, &format!("“{q}”"), &sub);
        if sp_ok {
            let size = t.font_size - 1.0;
            let labels = ["All", "Library", "Spotify"];
            let seg_w = labels
                .iter()
                .map(|l| ui.measure(l, size, Weight::Medium))
                .fold(0.0, f32::max)
                + 30.0;
            let x = r.right() - 3.0 * seg_w - 6.0;
            let sel = self.search_mode as usize;
            if let Some(i) = ui.segmented(anim::id("search-mode"), x, r.y + 4.0, &labels, sel) {
                self.search_mode = [SearchMode::All, SearchMode::Library, SearchMode::Spotify][i];
                self.list("search").selected = 0;
            }
        }
        if tracks.is_empty() && cards.is_empty() {
            if sp_loading {
                return;
            }
            let hint = if sp_ok {
                "Nothing in your library or on Spotify matches"
            } else {
                "Search looks at titles, artists and albums. Connect Spotify to search everything"
            };
            return empty_state(ui, body, "No matches", hint);
        }
        let mut list = body;
        if !cards.is_empty() {
            let strip_h = 236.0f32.min(body.h * 0.45);
            let strip = Rect::new(body.x, body.y, body.w, strip_h);
            if let Some(view) = self.search_cards(ui, strip, &cards) {
                self.navigate(view);
            }
            list = Rect::new(body.x, body.y + strip_h, body.w, body.h - strip_h);
        }
        if !tracks.is_empty() {
            let list = column_header(ui, list, true, true);
            let style = TrackListStyle {
                covers: true,
                album_column: true,
                track_numbers: false,
            };
            let action = self.track_list(ui, list, "search", &tracks, style, snap, a);
            self.apply_row_action(action, &tracks, player);
        }
    }

    /// A row of album cards (library and Spotify); returns the view to open when one is clicked.
    fn search_cards(&mut self, ui: &mut Ui, strip: Rect, cards: &[SearchCard]) -> Option<View> {
        let t = ui.theme.clone();
        const TILE: f32 = 150.0;
        let mut open = None;
        ui.c.push_clip(Rect::new(
            strip.x - 10.0,
            strip.y - 6.0,
            strip.w + 20.0,
            strip.h + 6.0,
        ));
        for (i, card) in cards.iter().enumerate() {
            let x = strip.x + i as f32 * (TILE + 18.0);
            if x > strip.right() {
                break;
            }
            let hover = ui.hovered(Rect::new(x, strip.y, TILE, TILE + 48.0));
            let h = ui
                .anim
                .to(anim::id(("card", i)), hover as u8 as f32, anim::FAST);
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
                card.cover.as_ref(),
                CoverSize::Large,
                t.radius,
                &card.title,
            );
            let mut tx = x;
            if card.spotify {
                icons::draw(
                    ui.c,
                    Icon::Spotify,
                    Rect::new(x, strip.y + TILE + 11.0, 13.0, 13.0),
                    spotify_green(),
                );
                tx += 18.0;
            }
            ui.label(
                &card.title,
                tx,
                Rect::new(x, strip.y + TILE + 8.0, TILE, 20.0),
                TILE - (tx - x),
                t.font_size - 0.5,
                Weight::Medium,
                t.fg,
            );
            ui.label(
                &card.subtitle,
                x,
                Rect::new(x, strip.y + TILE + 28.0, TILE, 18.0),
                TILE,
                12.0,
                Weight::Regular,
                t.dim,
            );
            if hover && ui.input.pressed {
                open = Some(card.open.clone());
            }
        }
        ui.c.pop_clip();
        open
    }
}

/// An album on the search page, from the library or Spotify.
struct SearchCard {
    title: String,
    subtitle: String,
    cover: Option<crate::model::CoverRef>,
    spotify: bool,
    open: View,
}
