//! The queue panel on the right: what's playing and what's next. Opened from the player bar
//! (or `4`), it slides in beside the page instead of replacing it.

use crate::audio::{AnalysisFrame, PlaybackState};
use crate::gui::app::{Gui, fmt_time};
use crate::gui::menu::MenuSource;
use crate::gui::render::{Rect, Weight};
use crate::gui::ui::icons::Icon;
use crate::gui::ui::{Ui, anim};
use crate::gui::views::lists::eq_icon;
use crate::library::CoverSize;
use crate::model::QueueEntry;
use crate::player::{Player, PlayerCommand as Cmd, PlayerSnapshot};

/// Panel width on wide windows.
pub(crate) const QUEUE_W: f32 = 340.0;
const ROW_H: f32 = 58.0;
const LABEL_H: f32 = 34.0;

enum Item {
    Label(&'static str),
    Entry { index: usize, current: bool },
}

impl Item {
    fn height(&self) -> f32 {
        match self {
            Item::Label(_) => LABEL_H,
            Item::Entry { .. } => ROW_H,
        }
    }
}

impl Gui {
    pub(crate) fn queue_panel(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        player: &Player,
        snap: &PlayerSnapshot,
        a: &AnalysisFrame,
    ) {
        let t = ui.theme.clone();
        ui.c.rect(r, t.surface, 12.0);
        ui.label(
            "Queue",
            r.x + 20.0,
            Rect::new(r.x, r.y + 14.0, r.w, 34.0),
            r.w - 140.0,
            15.0,
            Weight::Bold,
            t.fg,
        );
        let close = (r.right() - 28.0, r.y + 31.0);
        if ui.icon_button(
            anim::id("queue-close"),
            Icon::Close,
            close,
            15.0,
            t.dim,
            t.fg,
        ) {
            self.queue_open = false;
        }

        let (entries, current): (Vec<QueueEntry>, Option<usize>) = {
            let q = player.queue().read();
            (q.entries().to_vec(), q.current_index())
        };
        if !entries.is_empty() {
            let w = ui.measure("Clear", t.font_size - 1.0, Weight::Medium) + 32.0;
            if ui.pill("Clear", close.0 - 26.0 - w, r.y + 15.0, false).0 {
                player.send(Cmd::Clear);
            }
        }
        let body = Rect::new(r.x + 8.0, r.y + 60.0, r.w - 16.0, r.h - 68.0);
        if entries.is_empty() {
            ui.label_center(
                "Your queue is empty",
                Rect::new(body.x, body.y + body.h * 0.3, body.w, 24.0),
                15.0,
                Weight::Medium,
                t.fg.alpha(0.85),
            );
            ui.label_center(
                "Play a song, or right-click one to queue it",
                Rect::new(body.x, body.y + body.h * 0.3 + 28.0, body.w, 20.0),
                12.5,
                Weight::Regular,
                t.dim,
            );
            return;
        }

        // Now playing, then everything after it.
        let mut items = Vec::new();
        let first_next = match current {
            Some(i) => {
                items.push(Item::Label("Now playing"));
                items.push(Item::Entry {
                    index: i,
                    current: true,
                });
                i + 1
            }
            None => 0,
        };
        if first_next < entries.len() {
            items.push(Item::Label(if snap.shuffle {
                "Next up · shuffled"
            } else {
                "Next up"
            }));
            items.extend((first_next..entries.len()).map(|index| Item::Entry {
                index,
                current: false,
            }));
        }
        let content: f32 = items.iter().map(Item::height).sum();

        let mut ls = self.lists.get("queue").cloned().unwrap_or_default();
        ls.height = body.h;
        if ui.hovered(body) {
            ls.target += ui.input.scroll;
        }
        ls.target = ls.target.clamp(0.0, (content - body.h).max(0.0));

        let mut play = None;
        let mut remove = None;
        ui.c.push_clip(body);
        let mut y = body.y - ls.scroll;
        for item in &items {
            let h = item.height();
            if y + h < body.y || y > body.bottom() {
                y += h;
                continue;
            }
            match *item {
                Item::Label(text) => ui.label(
                    text,
                    body.x + 12.0,
                    Rect::new(body.x, y + 6.0, body.w, LABEL_H - 6.0),
                    body.w,
                    12.5,
                    Weight::Bold,
                    t.dim,
                ),
                Item::Entry { index, current } => {
                    let e = &entries[index];
                    let row = Rect::new(body.x, y, body.w, ROW_H - 2.0);
                    let hover = ui.hovered(row) && ui.hovered(body);
                    let hv = ui
                        .anim
                        .to(anim::id(("qp", e.id)), hover as u8 as f32, anim::FAST);
                    if hv > 0.01 {
                        ui.c.rect(row, t.wash(0.05 * hv), t.radius);
                    }
                    let cover = Rect::new(row.x + 8.0, row.y + 7.0, 42.0, 42.0);
                    let seed = e.track.album.as_deref().unwrap_or(&e.track.title);
                    ui.cover(cover, e.track.cover.as_ref(), CoverSize::Small, 6.0, seed);
                    let tx = cover.right() + 12.0;
                    let tw = row.right() - tx - 52.0;
                    ui.label(
                        &e.track.title,
                        tx,
                        Rect::new(tx, row.y + 9.0, tw, 20.0),
                        tw,
                        t.font_size,
                        Weight::Medium,
                        if current { t.accent } else { t.fg },
                    );
                    ui.label(
                        &e.track.artist_line(),
                        tx,
                        Rect::new(tx, row.y + 29.0, tw, 18.0),
                        tw,
                        12.5,
                        Weight::Regular,
                        t.dim,
                    );
                    let right = Rect::new(row.right() - 48.0, row.y, 40.0, row.h);
                    if current {
                        eq_icon(
                            ui,
                            Rect {
                                x: right.x + 6.0,
                                ..right
                            },
                            a,
                            snap.state == PlaybackState::Playing,
                            t.accent,
                        );
                    } else if hover {
                        let c = (right.x + 26.0, row.y + row.h / 2.0);
                        if ui.icon_button(
                            anim::id(("qp-x", e.id)),
                            Icon::Close,
                            c,
                            13.0,
                            t.dim,
                            t.fg,
                        ) {
                            remove = Some(e.id);
                        }
                    } else if let Some(d) = e.track.duration {
                        ui.label_right(
                            &fmt_time(d),
                            row.right() - 12.0,
                            row,
                            12.5,
                            Weight::Regular,
                            t.dim,
                        );
                    }
                    if hover && remove.is_none() {
                        if ui.input.double_click {
                            play = Some(e.id);
                        } else if ui.input.right_pressed {
                            self.open_menu(vec![(*e.track).clone()], MenuSource::Queue(e.id));
                        }
                    }
                }
            }
            y += h;
        }
        ui.c.pop_clip();
        if content > body.h {
            let bh = (body.h * body.h / content).max(28.0);
            let by = body.y + ls.scroll / (content - body.h) * (body.h - bh);
            ui.c.rect(
                Rect::new(body.right() - 2.0, by, 3.0, bh),
                t.fg.alpha(0.14),
                1.5,
            );
        }
        self.lists.insert("queue", ls);
        if let Some(id) = remove {
            player.send(Cmd::Remove(vec![id]));
        } else if let Some(id) = play {
            player.send(Cmd::PlayEntry(id));
        }
    }
}
