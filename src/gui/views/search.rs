//! Search across your library and Spotify, laid out like Spotify's: tabs (All, Songs,
//! Artists, Albums, Your library); "All" shows a top result beside the top songs, then rows of
//! artists and albums.
//!
//! Library results update on every keystroke. Spotify results follow a short debounce, and
//! the previous ones stay up meanwhile, so the page never blanks while typing.

use std::collections::HashSet;
use std::sync::Arc;

use crate::audio::AnalysisFrame;
use crate::gui::app::{Gui, SearchTab, View};
use crate::gui::render::{Rect, Weight};
use crate::gui::ui::icons::{self, Icon};
use crate::gui::ui::{Ui, anim};
use crate::gui::views::lists::{TrackListStyle, column_header};
use crate::gui::views::page::{Card, SONG_H, SongAction, chips, play_button, section_title};
use crate::gui::views::{empty_state, spotify_green};
use crate::library::CoverSize;
use crate::model::matching::{normalize, song_key, track_key};
use crate::model::{Track, TrackLocator};
use crate::player::{Player, PlayerCommand as Cmd, PlayerSnapshot};
use crate::spotify::SearchResults;

const TABS: [(SearchTab, &str); 5] = [
    (SearchTab::All, "All"),
    (SearchTab::Songs, "Songs"),
    (SearchTab::Artists, "Artists"),
    (SearchTab::Albums, "Albums"),
    (SearchTab::Library, "Your library"),
];

/// The best match, shown big.
enum Top {
    Artist(Card),
    Song(Track),
}

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
        let nq = normalize(&q);
        let connected = self.spotify_connected();
        let tab = if connected || self.search_tab != SearchTab::Library {
            self.search_tab
        } else {
            SearchTab::All
        };

        // Library matches: instant, cached per query.
        let lib: Arc<Vec<Track>> = match &self.cache.lib_search {
            Some((cq, res)) if *cq == q => res.clone(),
            _ => {
                let res = Arc::new(self.library.search(&q, 300));
                self.cache.lib_search = Some((q.clone(), res.clone()));
                res
            }
        };
        let matches = |s: &str| {
            let n = normalize(s);
            !nq.is_empty() && n.contains(&nq)
        };
        let lib_albums: Vec<Card> = self
            .cache
            .albums
            .iter()
            .filter(|al| matches(&al.name) || al.artist.as_deref().is_some_and(matches))
            .take(24)
            .map(|al| Card {
                title: al.name.clone(),
                subtitle: match (al.year, &al.artist) {
                    (Some(y), Some(ar)) => format!("{y} · {ar}"),
                    (_, Some(ar)) => ar.clone(),
                    _ => "Album".into(),
                },
                cover: al.cover.clone(),
                round: false,
                spotify: false,
                open: View::Album {
                    name: al.name.clone(),
                    artist: al.artist.clone(),
                },
            })
            .collect();
        let lib_artists: Vec<(String, Option<crate::model::CoverRef>)> = self
            .cache
            .artists
            .iter()
            .filter(|ar| matches(&ar.name))
            .take(24)
            .map(|ar| {
                let cover = self
                    .cache
                    .albums
                    .iter()
                    .find(|al| al.artist.as_deref() == Some(ar.name.as_str()))
                    .and_then(|al| al.cover.clone());
                (ar.name.clone(), cover)
            })
            .collect();

        // Spotify matches (debounced in the background).
        let sp: Option<(bool, Arc<SearchResults>)> = if tab == SearchTab::Library {
            None
        } else {
            self.spotify_search(&q)
        };
        let loading = connected && tab != SearchTab::Library && !sp.as_ref().is_some_and(|s| s.0);
        let sp = sp.map(|s| s.1);
        if let Some(res) = &sp {
            let ids: Vec<String> = res.artists.iter().take(12).map(|a| a.id.clone()).collect();
            self.want_artist_pics(&ids);
        }

        // Songs: library first, then Spotify songs you don't already have.
        let key = format!(
            "{q}|{}|{}",
            lib.len(),
            sp.as_ref().map_or(0, |s| Arc::as_ptr(s) as usize)
        );
        let songs = match &self.cache.search {
            Some((k, s)) if *k == key => s.clone(),
            _ => {
                let mut all: Vec<Track> = lib.to_vec();
                if let Some(sp) = &sp {
                    let have: HashSet<_> = all.iter().map(track_key).collect();
                    all.extend(
                        sp.tracks
                            .iter()
                            .filter(|t| !have.contains(&track_key(t)))
                            .cloned(),
                    );
                }
                let all = Arc::new(all);
                self.cache.search = Some((key, all.clone()));
                all
            }
        };

        self.cache.search_shown = Some(songs.clone());

        // Artists: Spotify's (with pictures), then library ones Spotify didn't list.
        let mut artists: Vec<Card> = Vec::new();
        let mut seen = HashSet::new();
        if let Some(res) = &sp {
            for ar in &res.artists {
                let pic = self
                    .sp
                    .artist_pics
                    .get(&ar.id)
                    .and_then(|p| p.image.clone());
                seen.insert(normalize(&ar.name));
                artists.push(Card {
                    title: ar.name.clone(),
                    subtitle: "Artist".into(),
                    cover: pic,
                    round: true,
                    spotify: false,
                    open: View::SpotifyArtist {
                        id: ar.id.clone(),
                        name: ar.name.clone(),
                    },
                });
            }
        }
        // Library artists first when they match better than Spotify's guesses.
        let mut local_artists: Vec<Card> = lib_artists
            .iter()
            .filter(|(name, _)| !seen.contains(&normalize(name)))
            .map(|(name, cover)| Card {
                title: name.clone(),
                subtitle: "Artist · in your library".into(),
                cover: cover.clone(),
                round: true,
                spotify: false,
                open: View::Artist(name.clone()),
            })
            .collect();
        local_artists.append(&mut artists);
        let artists = local_artists;

        // Albums: yours, then Spotify's you don't have.
        let mut albums = lib_albums;
        if let Some(res) = &sp {
            let have: HashSet<_> = albums
                .iter()
                .map(|c| song_key(&c.subtitle, &c.title))
                .collect();
            albums.extend(
                res.albums
                    .iter()
                    .filter(|al| {
                        !have.contains(&song_key(
                            al.artist.as_deref().unwrap_or_default(),
                            &al.name,
                        ))
                    })
                    .map(|al| Card {
                        title: al.name.clone(),
                        subtitle: match (al.year, &al.artist) {
                            (Some(y), Some(ar)) => format!("{y} · {ar}"),
                            (_, Some(ar)) => ar.clone(),
                            _ => "Album".into(),
                        },
                        cover: al.cover.clone(),
                        round: false,
                        spotify: false,
                        open: View::SpotifyAlbum {
                            id: al.id.clone(),
                            name: al.name.clone(),
                        },
                    }),
            );
        }

        // Tabs.
        let labels: Vec<&str> = TABS
            .iter()
            .filter(|(tb, _)| connected || *tb != SearchTab::Library)
            .map(|(_, l)| *l)
            .collect();
        let active = TABS.iter().position(|(tb, _)| *tb == tab).unwrap_or(0);
        if let Some(i) = chips(ui, r.x, r.y, &labels, active) {
            self.search_tab = TABS[i].0;
            self.list("search").selected = 0;
            self.list("search-page").target = 0.0;
        }
        if loading {
            // A small spinner after the tabs while Spotify answers.
            ui.anim.keep_alive();
            let a0 = spin_angle();
            ui.c.arc(r.right() - 12.0, r.y + 16.0, 7.0, a0, a0 + 4.2, 2.0, t.dim);
        }
        let body = Rect::new(r.x, r.y + 52.0, r.w, (r.h - 52.0).max(0.0));

        let nothing = songs.is_empty() && artists.is_empty() && albums.is_empty();
        if nothing {
            if loading {
                return;
            }
            let hint = if connected {
                "Check the spelling, or try fewer words"
            } else {
                "Search looks at titles, artists and albums. Connect Spotify to search everything"
            };
            return empty_state(ui, body, &format!("No results for “{q}”"), hint);
        }

        match tab {
            SearchTab::Songs | SearchTab::Library => {
                let list: Arc<Vec<Track>> = if tab == SearchTab::Library {
                    lib.clone()
                } else {
                    songs.clone()
                };
                self.cache.search_shown = Some(list.clone());
                if list.is_empty() {
                    return empty_state(ui, body, "No songs", "Try the other tabs");
                }
                let list_r = column_header(ui, body, true, true);
                let style = TrackListStyle {
                    covers: true,
                    album_column: true,
                    track_numbers: false,
                };
                let action = self.track_list(ui, list_r, "search", &list, style, snap, a);
                self.apply_row_action(action, &list, player);
            }
            SearchTab::Artists | SearchTab::Albums => {
                let cards = if tab == SearchTab::Artists {
                    &artists
                } else {
                    &albums
                };
                let mut y = self.page_begin(ui, body, "search-page");
                let (open, play, h) = self.card_grid(ui, body.x, y, body.w, "search-grid", cards);
                y += h;
                self.page_end(ui, body, "search-page", y);
                self.card_clicked(cards, open, play, player);
            }
            SearchTab::All => {
                let top = top_result(&nq, &artists, &songs);
                let mut y = self.page_begin(ui, body, "search-page");
                y = self.top_and_songs(ui, body, y, top, &songs, player, snap, a);
                if !artists.is_empty() {
                    y += 28.0;
                    let all = section_title(ui, body.x, y, body.w, "Artists", Some("Show all"));
                    if all {
                        self.search_tab = SearchTab::Artists;
                    }
                    y += 40.0;
                    let (open, play, h) = self.card_row(
                        ui,
                        body.x - 12.0,
                        y,
                        body.w + 24.0,
                        "search-artists",
                        &artists,
                    );
                    self.card_clicked(&artists, open, play, player);
                    y += h;
                }
                if !albums.is_empty() {
                    y += 16.0;
                    let all = section_title(ui, body.x, y, body.w, "Albums", Some("Show all"));
                    if all {
                        self.search_tab = SearchTab::Albums;
                    }
                    y += 40.0;
                    let (open, play, h) = self.card_row(
                        ui,
                        body.x - 12.0,
                        y,
                        body.w + 24.0,
                        "search-albums",
                        &albums,
                    );
                    self.card_clicked(&albums, open, play, player);
                    y += h;
                }
                self.page_end(ui, body, "search-page", y);
            }
        }
    }

    /// Opens or plays a clicked card.
    pub(crate) fn card_clicked(
        &mut self,
        cards: &[Card],
        open: Option<usize>,
        play: Option<usize>,
        player: &Player,
    ) {
        if let Some(c) = play.and_then(|i| cards.get(i)) {
            let v = c.open.clone();
            self.play_view(&v, player);
        } else if let Some(c) = open.and_then(|i| cards.get(i)) {
            match &c.open {
                View::SpotifyArtist { id, name } => {
                    let (id, name) = (id.clone(), name.clone());
                    self.open_artist(&name, Some(&id));
                }
                v => {
                    let v = v.clone();
                    self.navigate(v);
                }
            }
        }
    }

    /// The top result card beside the first four songs (stacked on narrow pages). Returns
    /// the y below them.
    fn top_and_songs(
        &mut self,
        ui: &mut Ui,
        body: Rect,
        y: f32,
        top: Option<Top>,
        songs: &Arc<Vec<Track>>,
        player: &Player,
        snap: &PlayerSnapshot,
        a: &AnalysisFrame,
    ) -> f32 {
        let wide = body.w >= 760.0;
        let top_w = if wide {
            (body.w * 0.42).min(460.0)
        } else {
            body.w
        };
        let mut bottom = y;
        if let Some(top) = &top {
            section_title(ui, body.x, y, top_w, "Top result", None);
            let card = Rect::new(body.x, y + 44.0, top_w, 228.0);
            self.top_card(ui, card, top, player);
            bottom = card.bottom();
        }
        let (sx, sy, sw) = if wide || top.is_none() {
            let x = if top.is_some() {
                body.x + top_w + 24.0
            } else {
                body.x
            };
            (x, y, body.right() - x)
        } else {
            (body.x, bottom + 24.0, body.w)
        };
        let n = songs.len().min(4);
        if n > 0 {
            section_title(ui, sx, sy, sw, "Songs", None);
            let rows = Rect::new(sx, sy + 44.0, sw, n as f32 * SONG_H);
            if let SongAction::Play(i) =
                self.song_rows(ui, rows, "search-top-songs", &songs[..n], None, snap, a)
            {
                player.send(Cmd::PlayTracks {
                    tracks: songs.to_vec(),
                    start: i,
                });
            }
            bottom = bottom.max(rows.bottom());
        }
        bottom
    }

    fn top_card(&mut self, ui: &mut Ui, r: Rect, top: &Top, player: &Player) {
        let t = ui.theme.clone();
        let id = anim::id("search-top");
        let hover = ui.hovered(r);
        let h = ui.anim.to(id, hover as u8 as f32, anim::FAST);
        ui.pointer |= hover;
        ui.c.rect(r, t.wash(0.05 + 0.04 * h), t.radius + 4.0);
        let img = Rect::new(r.x + 20.0, r.y + 20.0, 96.0, 96.0);
        let (title, kind, sub, cover, round, spotify) = match top {
            Top::Artist(c) => (
                c.title.clone(),
                "Artist",
                String::new(),
                c.cover.clone(),
                true,
                matches!(c.open, View::SpotifyArtist { .. }),
            ),
            Top::Song(tr) => (
                tr.title.clone(),
                "Song",
                tr.artist_line(),
                tr.cover.clone(),
                false,
                matches!(tr.locator, TrackLocator::Spotify { .. }),
            ),
        };
        let radius = if round { 48.0 } else { 6.0 };
        ui.c.glow(
            Rect::new(img.x + 4.0, img.y + 8.0, img.w - 8.0, img.h - 4.0),
            crate::gui::render::Color::BLACK.alpha(0.45),
            radius,
            14.0,
        );
        ui.cover(img, cover.as_ref(), CoverSize::Large, radius, &title);
        ui.label(
            &title,
            r.x + 20.0,
            Rect::new(r.x + 20.0, img.bottom() + 16.0, r.w - 40.0, 40.0),
            r.w - 40.0,
            30.0,
            Weight::Bold,
            t.fg,
        );
        // "Song · Artist" with a pill for the kind, like Spotify.
        let ky = img.bottom() + 62.0;
        let mut x = r.x + 20.0;
        if !sub.is_empty() {
            ui.label(
                &sub,
                x,
                Rect::new(x, ky, r.w - 160.0, 24.0),
                r.w - 170.0,
                13.5,
                Weight::Regular,
                t.dim,
            );
            x += ui.measure(&sub, 13.5, Weight::Regular).min(r.w - 170.0) + 10.0;
        }
        let kw = ui.measure(kind, 12.5, Weight::Bold) + 20.0;
        let pill = Rect::new(x, ky, kw, 24.0);
        ui.c.rect(pill, t.background.alpha(0.7), 12.0);
        ui.label_center(kind, pill, 12.5, Weight::Bold, t.fg);
        if spotify {
            icons::draw(
                ui.c,
                Icon::Spotify,
                Rect::new(pill.right() + 10.0, ky + 5.0, 14.0, 14.0),
                spotify_green(),
            );
        }
        let cover_for_button = Rect::new(r.right() - 76.0, r.bottom() - 76.0, 68.0, 68.0);
        let on_play = play_button(ui, id, cover_for_button, h);
        if hover && ui.input.pressed {
            match top {
                Top::Artist(c) => {
                    if on_play {
                        let v = c.open.clone();
                        self.play_view(&v, player);
                    } else {
                        self.card_clicked(std::slice::from_ref(c), Some(0), None, player);
                    }
                }
                Top::Song(tr) => {
                    if on_play {
                        player.send(Cmd::PlayTracks {
                            tracks: vec![tr.clone()],
                            start: 0,
                        });
                    } else {
                        let tr = tr.clone();
                        self.open_album_of(&tr);
                    }
                }
            }
        }
    }
}

/// An artist whose name is what was typed (or starts with it), else the first song.
fn top_result(nq: &str, artists: &[Card], songs: &[Track]) -> Option<Top> {
    let artist = artists.iter().find(|c| {
        let n = normalize(&c.title);
        n == nq || (nq.chars().count() >= 3 && n.starts_with(nq))
    });
    match artist {
        Some(c) => Some(Top::Artist(c.clone())),
        None => songs.first().cloned().map(Top::Song),
    }
}

/// Angle for loading spinners: one turn every ~0.9 s.
pub(crate) fn spin_angle() -> f32 {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() % 900) as f32;
    ms / 900.0 * std::f32::consts::TAU
}
