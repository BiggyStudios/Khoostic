//! The play queue.

use crate::audio::AnalysisFrame;
use crate::gui::app::{Gui, ROW_H, count, fmt_long};
use crate::gui::render::Rect;
use crate::gui::ui::{Ui, anim};
use crate::gui::views::empty_state;
use crate::gui::views::lists::{Cols, column_header, draw_track_row, row_bg, scrollbar};
use crate::player::{Player, PlayerCommand as Cmd, PlayerSnapshot};

impl Gui {
    pub(crate) fn queue_view(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        player: &Player,
        snap: &PlayerSnapshot,
        a: &AnalysisFrame,
    ) {
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
            row_bg(ui, anim::id(("queue", i)), row, hover, i == ls.selected);
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
                self.open_menu(
                    vec![(*e.track).clone()],
                    crate::gui::menu::MenuSource::Queue(e.id),
                );
            }
        }
        ui.c.pop_clip();
        scrollbar(ui, list, &ls, entries.len());
        self.lists.insert("queue", ls);
        if let Some((id, true)) = clicked {
            player.send(Cmd::PlayEntry(id));
        }
    }
}
