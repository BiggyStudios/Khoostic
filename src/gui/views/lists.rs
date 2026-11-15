//! Track lists: columns, rows (hover, selection, now-playing equalizer), scrollbars and what
//! clicks on them do.

use std::sync::Arc;

use crate::audio::{AnalysisFrame, PlaybackState};
use crate::gui::app::{Gui, ListState, ROW_H, View, fmt_time, local_path};
use crate::gui::render::{Color, Rect, Weight};
use crate::gui::ui::icons::{self, Icon};
use crate::gui::ui::{Ui, anim};
use crate::gui::views::spotify_green;
use crate::library::CoverSize;
use crate::model::{Track, TrackLocator};
use crate::player::{Player, PlayerCommand as Cmd, PlayerSnapshot};

/// What a click/key did to a row in a track list.
pub(crate) enum RowAction {
    None,
    Play(usize),
    /// Right-click: open the context menu for this row.
    Menu(usize),
}

pub(crate) struct TrackListStyle {
    pub covers: bool,
    pub album_column: bool,
    /// Show tag track numbers instead of list positions (album pages).
    pub track_numbers: bool,
}

impl Gui {
    pub(crate) fn track_list(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        key: &'static str,
        tracks: &[Track],
        style: TrackListStyle,
        snap: &PlayerSnapshot,
        a: &AnalysisFrame,
    ) -> RowAction {
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
        let mut cols = Cols::new(r.w, style.covers, style.album_column);
        cols.badges = key == "search";
        for i in first..(first + rows).min(tracks.len()) {
            let tr = &tracks[i];
            let row = Rect::new(r.x, r.y + i as f32 * ROW_H - ls.scroll, r.w, ROW_H - 2.0);
            let hover = ui.hovered(row) && ui.hovered(r);
            let is_playing = playing == Some(&tr.locator);
            row_bg(ui, anim::id((key, i)), row, hover, i == ls.selected);
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
                action = RowAction::Menu(i);
            }
        }
        ui.c.pop_clip();
        scrollbar(ui, r, &ls, tracks.len());
        self.lists.insert(key, ls);
        action
    }

    pub(crate) fn apply_row_action(
        &mut self,
        action: RowAction,
        tracks: &Arc<Vec<Track>>,
        player: &Player,
    ) {
        match action {
            RowAction::None => {}
            RowAction::Play(i) => player.send(Cmd::PlayTracks {
                tracks: tracks.to_vec(),
                start: i,
            }),
            RowAction::Menu(i) => {
                if let Some(t) = tracks.get(i) {
                    let source = match &self.view {
                        View::LocalPlaylist(id) => {
                            crate::gui::menu::MenuSource::LocalPlaylist { id: *id, index: i }
                        }
                        _ => crate::gui::menu::MenuSource::Other,
                    };
                    self.open_menu(vec![t.clone()], source);
                }
                let _ = player;
            }
        }
    }
}

/// Column layout for track rows.
pub(crate) struct Cols {
    /// Mark Spotify tracks (lists mixing sources).
    pub(crate) badges: bool,
    pub(crate) num: f32,
    pub(crate) cover: f32,
    pub(crate) title: f32,
    pub(crate) artist: f32,
    pub(crate) album: f32,
}

impl Cols {
    pub(crate) fn new(w: f32, covers: bool, album: bool) -> Self {
        let num = 36.0;
        let cover = if covers { 46.0 } else { 0.0 };
        let flex = (w - num - cover - 12.0 - 70.0).max(0.0);
        if album {
            Self {
                badges: false,
                num,
                cover,
                title: flex * 0.42,
                artist: flex * 0.28,
                album: flex * 0.30,
            }
        } else {
            Self {
                badges: false,
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
pub(crate) fn column_header(ui: &mut Ui, r: Rect, covers: bool, album: bool) -> Rect {
    let t = ui.theme;
    let cols = Cols::new(r.w, covers, album);
    let row = Rect::new(r.x, r.y, r.w, 24.0);
    let x = r.x + cols.num + cols.cover + 12.0;
    let c = t.dim.alpha(0.75);
    let size = 12.0;
    ui.label_right("#", r.x + cols.num - 8.0, row, size, Weight::Medium, c);
    ui.label("Title", x, row, cols.title, size, Weight::Medium, c);
    ui.label(
        "Artist",
        x + cols.title,
        row,
        cols.artist,
        size,
        Weight::Medium,
        c,
    );
    if album {
        let ax = x + cols.title + cols.artist;
        ui.label("Album", ax, row, cols.album, size, Weight::Medium, c);
    }
    ui.label_right("Time", r.right() - 14.0, row, size, Weight::Medium, c);
    ui.c.rect(Rect::new(r.x, r.y + 30.0, r.w, 1.0), t.line(), 0.0);
    Rect::new(r.x, r.y + 38.0, r.w, (r.h - 38.0).max(0.0))
}

/// A list row's background: fades in on hover, a little stronger when selected.
pub(crate) fn row_bg(ui: &mut Ui, id: u64, row: Rect, hover: bool, selected: bool) {
    let t = ui.theme;
    let h = ui.anim.to(id, hover as u8 as f32, anim::FAST);
    let s = ui.anim.to(id ^ 1, selected as u8 as f32, anim::NORMAL);
    let a = 0.035 * h + 0.065 * s;
    if a > 0.001 {
        ui.c.rect(row, t.wash(a), t.radius);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_track_row(
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
        ui.label_right(num, numr.right() - 8.0, row, 12.5, Weight::Regular, t.dim);
    }
    let mut x = row.x + cols.num + 12.0;
    if cols.cover > 0.0 {
        let c = Rect::new(x - 2.0, row.y + (row.h - 36.0) / 2.0, 36.0, 36.0);
        let seed = tr.album.as_deref().unwrap_or(&tr.title);
        ui.cover(c, tr.cover.as_ref(), CoverSize::Small, 6.0, seed);
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
    if cols.badges && matches!(tr.locator, TrackLocator::Spotify { .. }) {
        let tw = tr.duration.map_or(0.0, |d| {
            ui.measure(&fmt_time(d), t.font_size - 1.0, Weight::Regular)
        });
        icons::draw(
            ui.c,
            Icon::Spotify,
            Rect::new(
                row.right() - 14.0 - tw - 22.0,
                row.y + (row.h - 14.0) / 2.0,
                14.0,
                14.0,
            ),
            spotify_green().alpha(0.85),
        );
    }
    if let Some(d) = tr.duration {
        ui.label_right(
            &fmt_time(d),
            row.right() - 14.0,
            row,
            t.font_size - 1.0,
            Weight::Regular,
            t.dim,
        );
    }
}

pub(crate) fn scrollbar(ui: &mut Ui, r: Rect, ls: &ListState, rows: usize) {
    scrollbar_rows(ui, r, ls, rows, ROW_H);
}

/// A slim scroll indicator that brightens while scrolling or hovered.
pub(crate) fn scrollbar_rows(ui: &mut Ui, r: Rect, ls: &ListState, rows: usize, row_h: f32) {
    let content = rows as f32 * row_h;
    if content <= r.h {
        return;
    }
    let h = (r.h * r.h / content).max(28.0);
    let y = r.y + (ls.scroll / (content - r.h)) * (r.h - h);
    let moving = (ls.target - ls.scroll).abs() > 0.5;
    let near = ui.hovered(Rect::new(r.right() - 20.0, r.y, 40.0, r.h));
    let id = anim::id(("scrollbar", r.x as i32, r.y as i32));
    let v = ui.anim.to(id, (moving || near) as u8 as f32, anim::SLOW);
    ui.c.rect(
        Rect::new(r.right() + 12.0, y, 3.0, h),
        ui.theme.fg.alpha(0.12 + 0.2 * v),
        1.5,
    );
}

/// Little animated equalizer shown next to the playing track.
pub(crate) fn eq_icon(ui: &mut Ui, r: Rect, a: &AnalysisFrame, playing: bool, color: Color) {
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
pub(crate) fn is_local(t: &Track) -> bool {
    matches!(t.locator, TrackLocator::Local(_))
}
