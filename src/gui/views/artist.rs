//! A Spotify artist's profile: portrait, name, monthly listeners and genres; play, shuffle and
//! follow; popular songs with play counts; the discography; fans also like; about; and the
//! artist's songs in your library.

use std::sync::Arc;

use crate::audio::AnalysisFrame;
use crate::gui::app::{Gui, PAD, View};
use crate::gui::render::{Color, Rect, Weight};
use crate::gui::ui::icons::{self, Icon};
use crate::gui::ui::{Ui, anim};
use crate::gui::views::page::{Card, SONG_H, SongAction, chips, section_title, wrap};
use crate::gui::views::search::spin_angle;
use crate::library::CoverSize;
use crate::model::Track;
use crate::player::{Player, PlayerCommand as Cmd, PlayerSnapshot};
use crate::spotify::{AlbumSummary, ArtistSummary};

const PORTRAIT: f32 = 196.0;

impl Gui {
    pub(crate) fn spotify_artist_view(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        player: &Player,
        snap: &PlayerSnapshot,
        a: &AnalysisFrame,
        id: &str,
        name: &str,
    ) {
        let t = ui.theme.clone();
        let page = self.ensure_artist(id);
        let genres = self.genres_of(name);
        let local = self.library_tracks_of(name);

        // A wash of the artist's color behind the header.
        let hue = name
            .bytes()
            .fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619))
            % 360;
        let tint = ui.theme.tint(hue as f32, 0.45, 0.42);
        // Edge to edge across the page panel (`r` is inset by PAD), scrolling with the page.
        let scroll = self.list("artist-page").scroll;
        ui.c.gradient(
            Rect::new(r.x - PAD, r.y - PAD + 4.0 - scroll, r.w + 2.0 * PAD, 420.0),
            tint.alpha(0.55),
            tint.alpha(0.0),
            12.0,
        );
        let mut y = self.page_begin(ui, r, "artist-page");

        // Header.
        let portrait = Rect::new(r.x, y + 8.0, PORTRAIT, PORTRAIT);
        ui.c.glow(
            Rect::new(
                portrait.x + 8.0,
                portrait.y + 14.0,
                PORTRAIT - 16.0,
                PORTRAIT - 10.0,
            ),
            ui.theme.shadow(0.5),
            PORTRAIT / 2.0,
            24.0,
        );
        let image = page.as_ref().and_then(|p| p.image.clone());
        ui.cover(
            portrait,
            image.as_ref(),
            CoverSize::Large,
            PORTRAIT / 2.0,
            name,
        );
        let tx = portrait.right() + 28.0;
        let tw = (r.right() - tx).max(80.0);
        let mut ty = portrait.y + 30.0;
        if page.as_ref().is_some_and(|p| p.verified) {
            icons::draw(
                ui.c,
                Icon::Verified,
                Rect::new(tx, ty + 1.0, 20.0, 20.0),
                t.accent,
            );
            ui.label(
                "Verified Artist",
                tx + 26.0,
                Rect::new(tx, ty, tw, 22.0),
                tw,
                13.5,
                Weight::Medium,
                t.fg,
            );
        }
        ty += 26.0;
        // The name as big as fits.
        let mut size = 76.0;
        while size > 30.0 && ui.measure(name, size, Weight::Bold) > tw {
            size -= 4.0;
        }
        ui.label(
            name,
            tx,
            Rect::new(tx, ty, tw, size * 1.25),
            tw,
            size,
            Weight::Bold,
            t.fg,
        );
        ty += size * 1.25 + 6.0;
        let listeners = match &page {
            Some(p) => p.monthly_listeners.clone().unwrap_or_default(),
            None => "Loading…".into(),
        };
        ui.label(
            &listeners,
            tx,
            Rect::new(tx, ty, tw, 22.0),
            tw,
            14.5,
            Weight::Regular,
            t.fg.alpha(0.9),
        );
        ty += 30.0;
        let mut gx = tx;
        for g in &genres {
            let w = ui.measure(g, 12.0, Weight::Medium) + 20.0;
            if gx + w > r.right() {
                break;
            }
            let chip = Rect::new(gx, ty, w, 24.0);
            ui.c.rect(chip, Color::BLACK.alpha(0.28), 12.0);
            ui.label_center(g, chip, 12.0, Weight::Medium, t.fg.alpha(0.9));
            gx += w + 6.0;
        }
        y = portrait.bottom() + 28.0;

        let Some(page) = page else {
            ui.anim.keep_alive();
            let a0 = spin_angle();
            ui.c.arc(r.x + 28.0, y + 28.0, 12.0, a0, a0 + 4.2, 3.0, t.dim);
            return self.page_end(ui, r, "artist-page", y + 60.0);
        };

        // Play, shuffle, follow.
        let play = Rect::new(r.x, y, 56.0, 56.0);
        let pid = anim::id(("artist-play", id));
        let ph = ui.hover_t(pid, play);
        ui.c.circle(
            play.x + 28.0,
            play.y + 28.0,
            28.0 * (1.0 + 0.04 * ph),
            t.button.lerp(Color::WHITE, 0.1 * ph),
        );
        icons::draw(
            ui.c,
            Icon::Play,
            Rect::new(play.x + 17.0, play.y + 16.0, 24.0, 24.0),
            t.on_button,
        );
        ui.pointer |= ui.hovered(play);
        if ui.clicked(play) && !page.top_tracks.is_empty() {
            player.send(Cmd::SetShuffle(false));
            player.send(Cmd::PlayTracks {
                tracks: page.top_tracks.clone(),
                start: 0,
            });
        }
        if ui.icon_button(
            anim::id(("artist-shuffle", id)),
            Icon::Shuffle,
            (play.right() + 34.0, play.y + 28.0),
            24.0,
            t.dim,
            t.fg,
        ) {
            let tracks = page.top_tracks.clone();
            self.play_tracks(tracks, player, true);
        }
        let following = self.is_following(id);
        let label = if following { "Following" } else { "Follow" };
        let fw = ui.measure(label, 13.5, Weight::Bold) + 32.0;
        let follow = Rect::new(play.right() + 66.0, play.y + 12.0, fw, 32.0);
        let fh = ui.hover_t(anim::id(("artist-follow", id)), follow);
        ui.c.outline(follow, t.fg.alpha(0.45 + 0.5 * fh), 16.0, 1.0);
        ui.label_center(label, follow, 13.5, Weight::Bold, t.fg);
        ui.pointer |= ui.hovered(follow);
        if ui.clicked(follow) {
            let who = ArtistSummary {
                id: id.to_owned(),
                name: page.name.clone(),
                image: page.image.clone(),
            };
            self.set_following(who, !following);
            self.toast(if following {
                format!("Unfollowed {}", page.name)
            } else {
                format!("Following {}", page.name)
            });
        }
        y = play.bottom() + 32.0;

        // Popular.
        if !page.top_tracks.is_empty() {
            section_title(ui, r.x, y, r.w, "Popular", None);
            y += 44.0;
            let n = if self.artist_more { 10 } else { 5 }.min(page.top_tracks.len());
            let rows = Rect::new(r.x, y, r.w, n as f32 * SONG_H);
            let plays: Vec<Option<String>> = page.plays.iter().take(n).cloned().collect();
            if let SongAction::Play(i) = self.song_rows(
                ui,
                rows,
                "artist-popular",
                &page.top_tracks[..n],
                Some(&plays),
                snap,
                a,
            ) {
                player.send(Cmd::PlayTracks {
                    tracks: page.top_tracks.clone(),
                    start: i,
                });
            }
            y = rows.bottom() + 6.0;
            if page.top_tracks.len() > 5 {
                let more = if self.artist_more {
                    "Show less"
                } else {
                    "See more"
                };
                let mw = ui.measure(more, 13.0, Weight::Bold) + 8.0;
                let mr = Rect::new(r.x + 16.0, y, mw, 26.0);
                let hover = ui.hovered(mr);
                ui.pointer |= hover;
                ui.label(
                    more,
                    mr.x,
                    mr,
                    mw,
                    13.0,
                    Weight::Bold,
                    if hover { t.fg } else { t.dim },
                );
                if ui.clicked(mr) {
                    self.artist_more = !self.artist_more;
                }
                y += 34.0;
            }
        }

        // Discography.
        let tabs: [(&str, &[AlbumSummary]); 3] = [
            ("Popular releases", &page.popular_releases),
            ("Albums", &page.albums),
            ("Singles and EPs", &page.singles),
        ];
        let available: Vec<usize> = (0..3).filter(|&i| !tabs[i].1.is_empty()).collect();
        if !available.is_empty() {
            y += 16.0;
            section_title(ui, r.x, y, r.w, "Discography", None);
            y += 44.0;
            let current = if available.contains(&self.discography) {
                self.discography
            } else {
                available[0]
            };
            let labels: Vec<&str> = available.iter().map(|&i| tabs[i].0).collect();
            let active = available.iter().position(|&i| i == current).unwrap_or(0);
            if let Some(i) = chips(ui, r.x, y, &labels, active) {
                self.discography = available[i];
            }
            y += 46.0;
            let cards: Vec<Card> = tabs[current]
                .1
                .iter()
                .map(|al| Card {
                    title: al.name.clone(),
                    subtitle: al.year.map(|y| y.to_string()).unwrap_or_default(),
                    cover: al.cover.clone(),
                    round: false,
                    spotify: false,
                    open: View::SpotifyAlbum {
                        id: al.id.clone(),
                        name: al.name.clone(),
                    },
                })
                .collect();
            let (open, play, h) =
                self.card_row(ui, r.x - 12.0, y, r.w + 24.0, "artist-discography", &cards);
            self.card_clicked(&cards, open, play, player);
            y += h;
        }

        // Fans also like.
        if !page.related.is_empty() {
            y += 16.0;
            section_title(ui, r.x, y, r.w, "Fans also like", None);
            y += 44.0;
            let cards: Vec<Card> = page
                .related
                .iter()
                .map(|ar| Card {
                    title: ar.name.clone(),
                    subtitle: "Artist".into(),
                    cover: ar.image.clone(),
                    round: true,
                    spotify: false,
                    open: View::SpotifyArtist {
                        id: ar.id.clone(),
                        name: ar.name.clone(),
                    },
                })
                .collect();
            let (open, play, h) =
                self.card_row(ui, r.x - 12.0, y, r.w + 24.0, "artist-related", &cards);
            self.card_clicked(&cards, open, play, player);
            y += h;
        }

        // In your library.
        if !local.is_empty() {
            y += 16.0;
            section_title(ui, r.x, y, r.w, "In your library", None);
            y += 44.0;
            let n = local.len().min(5);
            let rows = Rect::new(r.x, y, r.w, n as f32 * SONG_H);
            if let SongAction::Play(i) =
                self.song_rows(ui, rows, "artist-local", &local[..n], None, snap, a)
            {
                player.send(Cmd::PlayTracks {
                    tracks: local.to_vec(),
                    start: i,
                });
            }
            y = rows.bottom() + 8.0;
        }

        // About.
        if let Some(bio) = &page.bio {
            y += 16.0;
            section_title(ui, r.x, y, r.w, "About", None);
            y += 44.0;
            let box_w = r.w.min(760.0);
            let lines = wrap(ui, bio, box_w - 48.0, 14.5, Weight::Regular);
            let shown = lines.len().min(8);
            let bh = 48.0 + shown as f32 * 22.0;
            ui.c.rect(Rect::new(r.x, y, box_w, bh), t.wash(0.05), t.radius + 4.0);
            for (i, line) in lines.iter().take(shown).enumerate() {
                let mut line = line.clone();
                if i + 1 == shown && lines.len() > shown {
                    line.push('…');
                }
                ui.label(
                    &line,
                    r.x + 24.0,
                    Rect::new(r.x + 24.0, y + 24.0 + i as f32 * 22.0, box_w - 48.0, 22.0),
                    box_w - 48.0,
                    14.5,
                    Weight::Regular,
                    t.fg.alpha(0.85),
                );
            }
            y += bh;
        }
        self.page_end(ui, r, "artist-page", y);
    }

    /// Your songs by `artist` (cached while the page is open).
    pub(crate) fn library_tracks_of(&mut self, artist: &str) -> Arc<Vec<Track>> {
        match &self.cache.artist_tracks {
            Some((a, t)) if a == artist => t.clone(),
            _ => {
                let t = Arc::new(self.library.artist_tracks(artist));
                self.cache.artist_tracks = Some((artist.to_owned(), t.clone()));
                t
            }
        }
    }
}
