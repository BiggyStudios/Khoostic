//! Pieces for pages that scroll as a whole, Spotify style (search, artist profiles): the page
//! scroller, section titles, rows and grids of cards (round for artists), two-line song rows
//! and wrapped text.

use crate::audio::{AnalysisFrame, PlaybackState};
use crate::gui::app::{Gui, View, fmt_time};
use crate::gui::menu::MenuSource;
use crate::gui::render::{Color, Rect, Weight};
use crate::gui::ui::icons::{self, Icon};
use crate::gui::ui::{Ui, anim};
use crate::gui::views::lists::{eq_icon, scrollbar_rows};
use crate::gui::views::spotify_green;
use crate::library::CoverSize;
use crate::model::{CoverRef, Track, TrackLocator};
use crate::player::{Player, PlayerCommand as Cmd, PlayerSnapshot};

/// Something to open: an album, artist or playlist, as a cover card.
#[derive(Clone)]
pub(crate) struct Card {
    pub title: String,
    pub subtitle: String,
    pub cover: Option<CoverRef>,
    /// Artists are round.
    pub round: bool,
    /// From Spotify (marked when a page mixes sources).
    pub spotify: bool,
    pub open: View,
}

/// Card size bounds and spacing.
const CARD_MIN: f32 = 150.0;
const CARD_MAX: f32 = 200.0;
const CARD_GAP: f32 = 12.0;
/// Two-line song rows.
pub(crate) const SONG_H: f32 = 56.0;

/// What happened in a list of song rows.
pub(crate) enum SongAction {
    None,
    Play(usize),
}

impl Gui {
    /// Starts a page that scrolls as a whole; returns the y its content starts at.
    pub(crate) fn page_begin(&mut self, ui: &mut Ui, r: Rect, key: &'static str) -> f32 {
        let hovered = ui.hovered(r);
        let ls = self.list(key);
        ls.height = r.h;
        if hovered {
            ls.target += ui.input.scroll;
        }
        let y = r.y - ls.scroll;
        // A little room above the content for card lift and focus rings.
        ui.c.push_clip(Rect::new(r.x - 12.0, r.y - 8.0, r.w + 24.0, r.h + 8.0));
        y
    }

    /// Ends the page started with [`Gui::page_begin`]; `end` is where its content ended.
    pub(crate) fn page_end(&mut self, ui: &mut Ui, r: Rect, key: &'static str, end: f32) {
        ui.c.pop_clip();
        let ls = self.list(key);
        let content = (end + ls.scroll - r.y + 24.0).max(1.0);
        ls.target = ls.target.clamp(0.0, (content - r.h).max(0.0));
        let ls = ls.clone();
        scrollbar_rows(ui, r, &ls, 1, content);
    }

    /// One row of cards (as many as fit). Returns (opened, played) and the row's height.
    pub(crate) fn card_row(
        &mut self,
        ui: &mut Ui,
        x: f32,
        y: f32,
        w: f32,
        key: &str,
        cards: &[Card],
    ) -> (Option<usize>, Option<usize>, f32) {
        let cols = (((w + CARD_GAP) / (CARD_MIN + CARD_GAP)).floor() as usize).max(1);
        let n = cards.len().min(cols);
        self.card_grid(ui, x, y, w, key, &cards[..n])
    }

    /// Cards wrapped into rows. Returns (opened, played) and the height used.
    pub(crate) fn card_grid(
        &mut self,
        ui: &mut Ui,
        x: f32,
        y: f32,
        w: f32,
        key: &str,
        cards: &[Card],
    ) -> (Option<usize>, Option<usize>, f32) {
        if cards.is_empty() {
            return (None, None, 0.0);
        }
        let t = ui.theme.clone();
        let cols = (((w + CARD_GAP) / (CARD_MIN + CARD_GAP)).floor() as usize).max(1);
        let cw = ((w - CARD_GAP * (cols - 1) as f32) / cols as f32).min(CARD_MAX);
        let pad = 12.0;
        let img = cw - 2.0 * pad;
        let ch = img + pad + 58.0;
        let (mut open, mut play) = (None, None);
        let view = ui.c.bounds();
        for (i, card) in cards.iter().enumerate() {
            let cx = x + (i % cols) as f32 * (cw + CARD_GAP);
            let cy = y + (i / cols) as f32 * (ch + CARD_GAP);
            let r = Rect::new(cx, cy, cw, ch);
            if r.bottom() < view.y - 50.0 || r.y > view.bottom() + 50.0 {
                continue;
            }
            let hover = ui.hovered(r);
            let id = anim::id((key, i));
            let h = ui.anim.to(id, hover as u8 as f32, anim::FAST);
            ui.pointer |= hover;
            // Spotify-style: the card's background lights up on hover.
            if h > 0.01 {
                ui.c.rect(r, t.wash(0.06 * h), t.radius + 2.0);
            }
            let cover = Rect::new(cx + pad, cy + pad, img, img);
            let radius = if card.round { img / 2.0 } else { t.radius };
            if h > 0.01 {
                ui.c.glow(
                    Rect::new(cover.x + 6.0, cover.y + 10.0, cover.w - 12.0, cover.h - 8.0),
                    Color::BLACK.alpha(0.45 * h),
                    radius,
                    14.0,
                );
            }
            ui.cover(
                cover,
                card.cover.as_ref(),
                CoverSize::Large,
                radius,
                &card.title,
            );
            let mut tx = cx + pad;
            let ty = cover.bottom() + 10.0;
            if card.spotify {
                icons::draw(
                    ui.c,
                    Icon::Spotify,
                    Rect::new(tx, ty + 3.5, 13.0, 13.0),
                    spotify_green(),
                );
                tx += 18.0;
            }
            ui.label(
                &card.title,
                tx,
                Rect::new(tx, ty, img, 20.0),
                img - (tx - cx - pad),
                t.font_size - 0.5,
                Weight::Medium,
                t.fg,
            );
            ui.label(
                &card.subtitle,
                cx + pad,
                Rect::new(cx + pad, ty + 21.0, img, 18.0),
                img,
                12.5,
                Weight::Regular,
                t.dim,
            );
            let on_play = play_button(ui, id, cover, h);
            if hover && ui.input.pressed {
                if on_play {
                    play = Some(i);
                } else {
                    open = Some(i);
                }
            }
        }
        let rows = cards.len().div_ceil(cols);
        (open, play, rows as f32 * (ch + CARD_GAP))
    }

    /// Two-line song rows (cover, title over artists, an optional extra column such as play
    /// counts, duration). Double-click or the hover play button plays; right-click opens the
    /// song menu.
    pub(crate) fn song_rows(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        key: &str,
        tracks: &[Track],
        extra: Option<&[Option<String>]>,
        snap: &PlayerSnapshot,
        a: &AnalysisFrame,
    ) -> SongAction {
        let t = ui.theme.clone();
        let playing = snap.current.as_ref().map(|e| &e.track.locator);
        let mut action = SongAction::None;
        let view = ui.c.bounds();
        let num_w = 40.0;
        let extra_w = if extra.is_some() {
            (r.w * 0.22).min(170.0)
        } else {
            0.0
        };
        for (i, tr) in tracks.iter().enumerate() {
            let row = Rect::new(r.x, r.y + i as f32 * SONG_H, r.w, SONG_H - 2.0);
            if row.bottom() < view.y || row.y > view.bottom() {
                continue;
            }
            let hover = ui.hovered(row);
            let id = anim::id((key, "song", i));
            let h = ui.anim.to(id, hover as u8 as f32, anim::FAST);
            if h > 0.01 {
                ui.c.rect(row, t.wash(0.05 * h), t.radius);
            }
            let is_playing = playing == Some(&tr.locator);
            let numr = Rect::new(row.x, row.y, num_w, row.h);
            if hover && !is_playing {
                let on = ui.hovered(numr);
                icons::draw(
                    ui.c,
                    Icon::Play,
                    Rect::new(
                        numr.right() - 24.0,
                        row.y + (row.h - 14.0) / 2.0,
                        14.0,
                        14.0,
                    ),
                    if on { t.fg } else { t.fg.alpha(0.8) },
                );
                ui.pointer |= on;
                if on && ui.input.pressed {
                    action = SongAction::Play(i);
                }
            } else if is_playing {
                eq_icon(ui, numr, a, snap.state == PlaybackState::Playing, t.accent);
            } else {
                ui.label_right(
                    &(i + 1).to_string(),
                    numr.right() - 10.0,
                    row,
                    13.0,
                    Weight::Regular,
                    t.dim,
                );
            }
            let cover = Rect::new(numr.right() + 6.0, row.y + 7.0, 40.0, 40.0);
            let seed = tr.album.as_deref().unwrap_or(&tr.title);
            ui.cover(cover, tr.cover.as_ref(), CoverSize::Small, 5.0, seed);
            let tx = cover.right() + 12.0;
            let dur_w = 60.0;
            let tw = (row.right() - tx - extra_w - dur_w - 36.0).max(40.0);
            ui.label(
                &tr.title,
                tx,
                Rect::new(tx, row.y + 8.0, tw, 20.0),
                tw,
                t.font_size,
                Weight::Medium,
                if is_playing { t.accent } else { t.fg },
            );
            let mut ax = tx;
            if matches!(tr.locator, TrackLocator::Spotify { .. }) && extra.is_none() {
                icons::draw(
                    ui.c,
                    Icon::Spotify,
                    Rect::new(ax, row.y + 31.0, 12.0, 12.0),
                    spotify_green().alpha(0.85),
                );
                ax += 17.0;
            }
            ui.label(
                &tr.artist_line(),
                ax,
                Rect::new(ax, row.y + 28.0, tw, 18.0),
                tw - (ax - tx),
                12.5,
                Weight::Regular,
                t.dim,
            );
            if let Some(Some(e)) = extra.and_then(|x| x.get(i)) {
                ui.label(
                    e,
                    row.right() - dur_w - extra_w - 24.0,
                    row,
                    extra_w,
                    13.0,
                    Weight::Regular,
                    t.dim,
                );
            }
            if self.is_liked(tr) {
                icons::draw(
                    ui.c,
                    Icon::HeartFilled,
                    Rect::new(
                        row.right() - dur_w - 28.0,
                        row.y + (row.h - 15.0) / 2.0,
                        15.0,
                        15.0,
                    ),
                    spotify_green(),
                );
            }
            if let Some(d) = tr.duration {
                ui.label_right(
                    &fmt_time(d),
                    row.right() - 14.0,
                    row,
                    13.0,
                    Weight::Regular,
                    t.dim,
                );
            }
            if hover && ui.input.double_click {
                action = SongAction::Play(i);
            }
            if hover && ui.input.right_pressed {
                self.open_menu(vec![tr.clone()], MenuSource::Other);
            }
        }
        action
    }

    /// Plays what a card or library row opens (an album, artist or playlist).
    pub(crate) fn play_view(&mut self, v: &View, player: &Player) {
        match v {
            View::Album { name, artist } => {
                let tracks = self.library.album_tracks(name, artist.as_deref());
                self.play_tracks(tracks, player, false);
            }
            View::Artist(name) => {
                let tracks = self.library.artist_tracks(name);
                self.play_tracks(tracks, player, false);
            }
            View::LocalPlaylist(id) => {
                if let Some((_, t)) = self.sp.local.iter().find(|(p, _)| p.id == *id) {
                    let t = t.to_vec();
                    self.play_tracks(t, player, false);
                }
            }
            View::SpotifyAlbum { id, .. } => self.play_list_key(&format!("al:{id}"), player),
            View::SpotifyPlaylist { id, .. } => self.play_list_key(&format!("pl:{id}"), player),
            View::Liked => self.play_list_key("liked", player),
            View::SpotifyArtist { id, .. } => match self.ensure_artist(id) {
                Some(page) => player.send(Cmd::PlayTracks {
                    tracks: page.top_tracks.clone(),
                    start: 0,
                }),
                None => self.sp.play_when_loaded = Some(format!("artist:{id}")),
            },
            _ => {}
        }
    }
}

/// The round play button that rises into a hovered cover's corner. Returns whether the
/// mouse is on it.
pub(crate) fn play_button(ui: &mut Ui, id: u64, cover: Rect, h: f32) -> bool {
    if h <= 0.01 {
        return false;
    }
    let s = 46.0f32.min(cover.w * 0.4);
    let b = Rect::new(
        cover.right() - s - 8.0,
        cover.bottom() - s - 8.0 + 8.0 * (1.0 - h),
        s,
        s,
    );
    let bh = ui.hovered(b);
    let bt = ui.anim.to(id ^ 2, bh as u8 as f32, anim::FAST);
    // Overlay layer: images draw after shapes within a layer.
    ui.c.set_layer(1);
    ui.c.push_fx(0.0, 0.0, h);
    ui.c.glow(b, Color::BLACK.alpha(0.5), s / 2.0, 10.0);
    ui.c.circle(
        b.x + s / 2.0,
        b.y + s / 2.0,
        s / 2.0 * (1.0 + 0.06 * bt),
        spotify_green().lerp(Color::WHITE, 0.12 * bt),
    );
    let i = s * 0.42;
    icons::draw(
        ui.c,
        Icon::Play,
        Rect::new(b.x + (s - i) / 2.0 + 1.0, b.y + (s - i) / 2.0, i, i),
        Color::BLACK,
    );
    ui.c.pop_fx();
    ui.c.set_layer(0);
    bh
}

/// A section heading with an optional link on the right ("Show all"). Returns whether the
/// link was clicked.
pub(crate) fn section_title(
    ui: &mut Ui,
    x: f32,
    y: f32,
    w: f32,
    title: &str,
    link: Option<&str>,
) -> bool {
    let t = ui.theme.clone();
    ui.label(
        title,
        x,
        Rect::new(x, y, w, 32.0),
        w - 120.0,
        22.0,
        Weight::Bold,
        t.fg,
    );
    let Some(link) = link else { return false };
    let lw = ui.measure(link, 13.0, Weight::Bold);
    let lr = Rect::new(x + w - lw - 4.0, y + 4.0, lw + 8.0, 24.0);
    let hover = ui.hovered(lr);
    ui.pointer |= hover;
    ui.label_right(
        link,
        lr.right() - 4.0,
        lr,
        13.0,
        Weight::Bold,
        if hover { t.fg } else { t.dim },
    );
    if hover {
        ui.c.rect(
            Rect::new(lr.x + 4.0, lr.bottom() - 4.0, lw, 1.0),
            t.fg.alpha(0.7),
            0.0,
        );
    }
    ui.clicked(lr)
}

/// Splits `text` into lines no wider than `w` (paragraphs kept).
pub(crate) fn wrap(ui: &mut Ui, text: &str, w: f32, size: f32, weight: Weight) -> Vec<String> {
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split_whitespace() {
            let candidate = if line.is_empty() {
                word.to_owned()
            } else {
                format!("{line} {word}")
            };
            if !line.is_empty() && ui.measure(&candidate, size, weight) > w {
                lines.push(std::mem::replace(&mut line, word.to_owned()));
            } else {
                line = candidate;
            }
        }
        lines.push(line);
    }
    lines
}

/// Rounded filter chips in a row; returns the index clicked.
pub(crate) fn chips(ui: &mut Ui, x: f32, y: f32, labels: &[&str], active: usize) -> Option<usize> {
    let t = ui.theme.clone();
    let mut cx = x;
    let mut clicked = None;
    for (i, label) in labels.iter().enumerate() {
        let w = ui.measure(label, 13.0, Weight::Medium) + 28.0;
        let r = Rect::new(cx, y, w, 32.0);
        let on = i == active;
        let id = anim::id(("chip", label));
        let h = ui.hover_t(id, r);
        let o = ui.anim.to(id ^ 7, on as u8 as f32, anim::NORMAL);
        let fill = t.wash(0.08 + 0.05 * h).lerp(t.fg, o);
        ui.c.rect(r, fill, 16.0);
        ui.label_center(label, r, 13.0, Weight::Medium, t.fg.lerp(t.background, o));
        ui.pointer |= ui.hovered(r);
        if ui.clicked(r) {
            clicked = Some(i);
        }
        cx += w + 8.0;
    }
    clicked
}
