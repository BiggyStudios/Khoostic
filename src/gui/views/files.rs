//! The file browser.

use crate::gui::app::{Gui, ROW_H, count};
use crate::gui::render::{Rect, Weight};
use crate::gui::ui::icons::{self, Icon};
use crate::gui::ui::{Ui, anim};
use crate::gui::views::empty_state;
use crate::gui::views::lists::{row_bg, scrollbar};
use crate::player::browse::EntryKind;
use crate::player::{Player, PlayerCommand as Cmd};

impl Gui {
    pub(crate) fn files_view(&mut self, ui: &mut Ui, r: Rect, player: &Player) {
        let t = ui.theme.clone();
        let dir = self.browser.dir().display().to_string();
        let name = self
            .browser
            .dir()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.clone());
        let body = self.header(ui, r, &name, &dir);
        match self.header_buttons(ui, r, &["Music", "Up", "Add all"]) {
            Some(0) => {
                if let Some(d) = self.music_dirs.first().cloned()
                    && self.browser.open(&d).is_ok()
                {
                    self.lists.remove("files");
                }
            }
            Some(1) => self.files_up(),
            Some(_) => {
                let tracks = self.library.tracks_for_paths(&[self.browser.dir()]);
                self.toast(format!(
                    "Added {} to the queue",
                    count(tracks.len(), "track")
                ));
                player.send(Cmd::Enqueue(tracks));
            }
            None => {}
        }
        let len = self.browser.entries().len();
        if len == 0 {
            let msg = self
                .browser
                .error()
                .unwrap_or("No folders or audio files here")
                .to_owned();
            return empty_state(ui, body, &msg, "Backspace goes up a folder");
        }
        let mut ls = self.lists.get("files").cloned().unwrap_or_default();
        ls.height = body.h;
        ls.clamp(len, ROW_H);
        ls.selected = ls.selected.min(len - 1);
        if ui.hovered(body) {
            ls.target += ui.input.scroll;
        }
        let mut clicked = None;
        ui.c.push_clip(body);
        let first = (ls.scroll / ROW_H).floor().max(0.0) as usize;
        for i in first..(first + (body.h / ROW_H) as usize + 2).min(len) {
            let e = &self.browser.entries()[i];
            let row = Rect::new(
                body.x,
                body.y + i as f32 * ROW_H - ls.scroll,
                body.w,
                ROW_H - 2.0,
            );
            let hover = ui.hovered(row) && ui.hovered(body);
            row_bg(ui, anim::id(("files", i)), row, hover, i == ls.selected);
            let icon = Rect::new(row.x + 14.0, row.y + (row.h - 18.0) / 2.0, 18.0, 18.0);
            let (glyph, color) = match e.kind {
                EntryKind::Dir => (Icon::Folder, t.accent.alpha(0.85)),
                EntryKind::Audio => (Icon::Song, t.dim),
            };
            icons::draw(ui.c, glyph, icon, color);
            ui.label(
                &e.name,
                row.x + 46.0,
                row,
                row.w - 60.0,
                t.font_size,
                Weight::Medium,
                t.fg,
            );
            if hover && ui.input.pressed {
                clicked = Some((i, ui.input.double_click, false));
            }
            if hover && ui.input.right_pressed {
                clicked = Some((i, false, true));
            }
        }
        ui.c.pop_clip();
        scrollbar(ui, body, &ls, len);
        self.lists.insert("files", ls);
        if let Some((i, dbl, right)) = clicked {
            self.list("files").selected = i;
            if right {
                self.files_add(i, player, false);
            } else if dbl {
                self.files_open(i, player);
            }
        }
    }

    pub(crate) fn files_open(&mut self, i: usize, player: &Player) {
        let Some(e) = self.browser.entries().get(i).cloned() else {
            return;
        };
        match e.kind {
            EntryKind::Dir => match self.browser.open(&e.path) {
                Ok(()) => {
                    self.lists.remove("files");
                }
                Err(err) => self.toast(err),
            },
            EntryKind::Audio => {
                let tracks = self.library.tracks_for_paths(&[&e.path]);
                player.send(Cmd::EnqueueAndPlay(tracks));
            }
        }
    }

    pub(crate) fn files_add(&mut self, i: usize, player: &Player, next: bool) {
        let Some(e) = self.browser.entries().get(i).cloned() else {
            return;
        };
        let tracks = self.library.tracks_for_paths(&[&e.path]);
        if tracks.is_empty() {
            return self.toast("Nothing playable there");
        }
        let n = count(tracks.len(), "track");
        self.toast(if next {
            format!("Playing {n} next")
        } else {
            format!("Added {n} to the queue")
        });
        player.send(if next {
            Cmd::PlayNext(tracks)
        } else {
            Cmd::Enqueue(tracks)
        });
    }

    pub(crate) fn files_up(&mut self) {
        if let Some(i) = self.browser.parent() {
            let l = self.list("files");
            l.selected = i;
            l.target = (i as f32 * ROW_H - l.height / 2.0).max(0.0);
        }
    }
}
