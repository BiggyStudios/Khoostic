//! Cover grids (albums, playlists) with lift-on-hover tiles and their play buttons.

use crate::gui::app::{Gui, count};
use crate::gui::render::{Color, Rect, Weight};
use crate::gui::ui::icons::{self, Icon};
use crate::gui::ui::{Ui, anim};
use crate::gui::views::lists::scrollbar_rows;
use crate::library::{Album, CoverSize};

/// One tile in a cover grid (albums, playlists).
pub(crate) struct Tile {
    pub title: String,
    pub subtitle: String,
    pub cover: Option<crate::model::CoverRef>,
}

impl Gui {
    /// A responsive grid of album tiles. Returns (clicked album, play-button album).
    pub(crate) fn album_grid(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        key: &'static str,
        albums: &[Album],
    ) -> (Option<usize>, Option<usize>) {
        let tiles: Vec<Tile> = albums
            .iter()
            .map(|al| Tile {
                title: al.name.clone(),
                subtitle: match (&al.artist, al.year) {
                    (Some(a), Some(y)) => format!("{a} · {y}"),
                    (Some(a), None) => a.clone(),
                    (None, Some(y)) => y.to_string(),
                    (None, None) => count(al.tracks as usize, "song"),
                },
                cover: al.cover.clone(),
            })
            .collect();
        self.tile_grid(ui, r, key, &tiles)
    }

    /// A responsive grid of cover tiles. Returns (clicked tile, play-button tile).
    pub(crate) fn tile_grid(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        key: &'static str,
        albums: &[Tile],
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
        let kb = self.kb_nav;
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
            let tile_rect = Rect::new(x, y, tile, row_h - 10.0);
            let hover = ui.hovered(tile_rect) && ui.hovered(r);
            let id = anim::id((key, i));
            let h = ui.anim.to(id, hover as u8 as f32, anim::FAST);
            ui.pointer |= hover;
            // Hovered covers lift a little onto a soft shadow.
            let lift = 4.0 * h;
            let cover = Rect::new(x, y - lift, tile, tile);
            if h > 0.01 {
                ui.c.glow(
                    Rect::new(cover.x + 4.0, cover.y + 10.0, cover.w - 8.0, cover.h - 6.0),
                    ui.theme.shadow(0.55 * h),
                    t.radius,
                    16.0,
                );
            }
            ui.cover(
                cover,
                al.cover.as_ref(),
                CoverSize::Large,
                t.radius,
                &al.title,
            );
            let sel = ui
                .anim
                .to(id ^ 1, (kb && i == ls.selected) as u8 as f32, anim::NORMAL);
            if sel > 0.01 {
                ui.c.outline(
                    cover.inset(-4.0, -4.0),
                    t.accent.alpha(0.55 * sel),
                    t.radius + 4.0,
                    1.5,
                );
            }
            ui.label(
                &al.title,
                x,
                Rect::new(x, y + tile + 10.0, tile, 22.0),
                tile,
                t.font_size,
                Weight::Medium,
                t.fg,
            );
            let sub = al.subtitle.clone();
            ui.label(
                &sub,
                x,
                Rect::new(x, y + tile + 31.0, tile, 20.0),
                tile,
                t.font_size - 1.5,
                Weight::Regular,
                t.dim,
            );
            let mut on_play = false;
            if h > 0.01 {
                // A play button fades and slides up into the cover's corner.
                let b = Rect::new(
                    cover.right() - 54.0,
                    cover.bottom() - 54.0 + 8.0 * (1.0 - h),
                    44.0,
                    44.0,
                );
                let bh = ui.hovered(b);
                let bt = ui.anim.to(id ^ 2, bh as u8 as f32, anim::FAST);
                let rad = 22.0 * (1.0 + 0.07 * bt);
                // Overlay layer: images draw after shapes within a layer, so this would
                // otherwise sit under the cover art.
                ui.c.set_layer(1);
                ui.c.push_fx(0.0, 0.0, h);
                ui.c.glow(b, ui.theme.shadow(0.5), 22.0, 10.0);
                ui.c.circle(
                    b.x + 22.0,
                    b.y + 22.0,
                    rad,
                    t.button.lerp(Color::WHITE, 0.12 * bt),
                );
                icons::draw(
                    ui.c,
                    Icon::Play,
                    Rect::new(b.x + 12.5, b.y + 12.0, 20.0, 20.0),
                    t.on_button,
                );
                ui.c.pop_fx();
                ui.c.set_layer(0);
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
}
