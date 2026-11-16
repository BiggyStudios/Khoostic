//! Lyrics of the playing song. Synced lyrics follow the song: the current line is lit, sung
//! lines dim, and clicking a line jumps there. Scrolling by hand pauses the following for a
//! few seconds.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::gui::app::Gui;
use crate::gui::render::{Rect, Weight};
use crate::gui::ui::{Ui, anim};
use crate::gui::views::empty_state;
use crate::gui::views::page::wrap;
use crate::gui::views::search::spin_angle;
use crate::model::Lyrics;
use crate::player::{Player, PlayerCommand as Cmd, PlayerSnapshot};

const SIZE: f32 = 28.0;
const LINE_H: f32 = 38.0;
const GAP: f32 = 14.0;
/// How long a manual scroll keeps the page from following the song.
const MANUAL: Duration = Duration::from_secs(4);
/// Show a line slightly before it's sung, like a karaoke screen.
const LEAD: Duration = Duration::from_millis(150);

/// Lyric lines wrapped to a width, with where each starts (page coordinates).
pub(crate) struct LyricsLayout {
    uri: String,
    width: f32,
    lines: Vec<(Vec<String>, f32)>,
    height: f32,
}

impl Gui {
    pub(crate) fn lyrics_view(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        player: &Player,
        snap: &PlayerSnapshot,
    ) {
        let t = ui.theme.clone();
        let Some(e) = &snap.current else {
            return empty_state(ui, r, "Nothing playing", "Play a song to see its lyrics");
        };
        let track = (*e.track).clone();
        let hue = track
            .title
            .bytes()
            .chain(track.artist_line().bytes())
            .fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619))
            % 360;
        let tint = ui.theme.tint(hue as f32, 0.5, 0.45);
        ui.c.gradient(
            Rect::new(r.x - 32.0, r.y - 28.0, r.w + 64.0, r.h + 28.0),
            tint.alpha(0.4),
            tint.alpha(0.08),
            12.0,
        );
        let lyrics = match self.lyrics_for(&track) {
            None => {
                ui.anim.keep_alive();
                let a0 = spin_angle();
                let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h * 0.3);
                ui.c.arc(cx, cy, 14.0, a0, a0 + 4.2, 3.0, t.fg.alpha(0.7));
                ui.label_center(
                    "Looking for lyrics…",
                    Rect::new(r.x, cy + 28.0, r.w, 24.0),
                    15.0,
                    Weight::Medium,
                    t.fg.alpha(0.8),
                );
                return;
            }
            Some(None) => {
                return empty_state(
                    ui,
                    r,
                    "No lyrics for this song",
                    "Looked in your files, Spotify and LRCLIB (see lyrics.providers in the config)",
                );
            }
            Some(Some(l)) => l,
        };

        let width = (r.w - 40.0).min(900.0);
        let uri = track.locator.to_uri();
        let stale = self
            .lyrics_layout
            .as_ref()
            .is_none_or(|l| l.uri != uri || (l.width - width).abs() > 1.0);
        if stale {
            self.lyrics_layout = Some(layout(ui, &lyrics, &uri, width));
        }
        let layout = self.lyrics_layout.take().expect("just set");

        let pos = self.clock.position() + LEAD;
        let current = lyrics.line_at(pos);
        let header = if lyrics.synced { 24.0 } else { 56.0 };
        let manual = self.lyrics_manual.is_some_and(|at| at.elapsed() < MANUAL);
        {
            let hovered = ui.hovered(r);
            let scrolled = hovered && ui.input.scroll != 0.0;
            let ls = self.list("lyrics");
            ls.height = r.h;
            if scrolled {
                ls.target += ui.input.scroll;
            }
            if let (Some(i), false, false) = (current, manual, scrolled) {
                // Keep the sung line a third of the way down.
                ls.target = header + layout.lines[i].1 - r.h * 0.33;
            }
            let content = header + layout.height + 120.0;
            ls.target = ls.target.clamp(0.0, (content - r.h).max(0.0));
            if scrolled {
                self.lyrics_manual = Some(Instant::now());
            }
        }
        let scroll = self.list("lyrics").scroll;
        if manual {
            ui.anim.keep_alive();
        }

        ui.c.push_clip(Rect::new(r.x - 8.0, r.y - 8.0, r.w + 16.0, r.h + 8.0));
        let x = r.x + 20.0;
        let top = r.y + header - scroll;
        if !lyrics.synced {
            ui.label(
                "These lyrics aren't synced to the song",
                x,
                Rect::new(x, r.y + 8.0 - scroll, width, 22.0),
                width,
                13.0,
                Weight::Medium,
                t.fg.alpha(0.6),
            );
        }
        let mut seek = None;
        for (i, (rows, ly)) in layout.lines.iter().enumerate() {
            let y = top + ly;
            let h = rows.len().max(1) as f32 * LINE_H;
            if y + h < r.y - 40.0 || y > r.bottom() + 40.0 {
                continue;
            }
            let state = match current {
                _ if !lyrics.synced => 0.85,
                Some(c) if c == i => 1.0,
                Some(c) if c > i => 0.55,
                _ => 0.32,
            };
            let lit = ui.anim.to(anim::id(("lyric", i)), state, anim::NORMAL);
            let line_r = Rect::new(x - 12.0, y - 4.0, width + 24.0, h + 4.0);
            let hover = lyrics.synced && ui.hovered(line_r);
            ui.pointer |= hover;
            let alpha = if hover { lit.max(0.8) } else { lit };
            for (k, row) in rows.iter().enumerate() {
                ui.label(
                    row,
                    x,
                    Rect::new(x, y + k as f32 * LINE_H, width, LINE_H),
                    width,
                    SIZE,
                    Weight::Bold,
                    t.fg.alpha(alpha),
                );
            }
            if hover && ui.input.pressed {
                seek = lyrics.lines[i].start;
            }
        }
        let credit_y = top + layout.height + 40.0;
        ui.label(
            &format!("Lyrics provided by {}", lyrics.source),
            x,
            Rect::new(x, credit_y, width, 22.0),
            width,
            13.0,
            Weight::Medium,
            t.fg.alpha(0.55),
        );
        ui.c.pop_clip();
        self.lyrics_layout = Some(layout);
        if let Some(to) = seek {
            player.send(Cmd::Seek(to));
            self.lyrics_manual = None;
        }
    }
}

fn layout(ui: &mut Ui, lyrics: &Arc<Lyrics>, uri: &str, width: f32) -> LyricsLayout {
    let mut lines = Vec::with_capacity(lyrics.lines.len());
    let mut y = 0.0;
    for l in &lyrics.lines {
        let text = l.text.trim();
        let rows = if text.is_empty() {
            Vec::new()
        } else {
            wrap(ui, text, width, SIZE, Weight::Bold)
        };
        lines.push((rows.clone(), y));
        y += if rows.is_empty() {
            LINE_H * 0.6
        } else {
            rows.len() as f32 * LINE_H + GAP
        };
    }
    LyricsLayout {
        uri: uri.to_owned(),
        width,
        lines,
        height: y,
    }
}
