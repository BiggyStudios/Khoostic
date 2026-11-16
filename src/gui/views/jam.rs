//! The Jam page: host a jam or join one, see who's listening, leave.

use crate::gui::app::Gui;
use crate::gui::render::{Rect, Weight};
use crate::gui::ui::{Key, Ui};
use crate::jam::JamStatus;

const CARD_H: f32 = 196.0;

impl Gui {
    pub(crate) fn jam_view(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.theme.clone();
        let body = self.header(ui, r, "Jam", "Listen together: same song, same moment");
        let Some(jams) = self.jams.clone() else {
            return;
        };
        let lx = body.x;
        let mut y = body.y;
        let small = |ui: &mut Ui, s: &str, x: f32, y: f32, w: f32, color| {
            ui.label(
                s,
                x,
                Rect::new(x, y, w, 18.0),
                w,
                12.0,
                Weight::Regular,
                color,
            );
        };

        match jams.status() {
            JamStatus::Hosting { invite, guests } => {
                self.jam_heading(ui, lx, &mut y, "You're hosting a jam");
                small(
                    ui,
                    "Send this invite to friends. They paste it under Jam → Join.",
                    lx,
                    y,
                    body.w,
                    t.dim,
                );
                y += 30.0;
                // The invite, big, with a copy button.
                let box_w = (body.w - 140.0).clamp(240.0, 460.0);
                let invite_r = Rect::new(lx, y, box_w, 48.0);
                ui.c.rect(invite_r, t.wash(0.07), 12.0);
                ui.label(
                    &invite,
                    lx + 16.0,
                    Rect::new(lx + 16.0, y, box_w - 32.0, 48.0),
                    box_w - 32.0,
                    t.font_size + 3.0,
                    Weight::Medium,
                    t.fg,
                );
                if ui.pill("Copy invite", lx + box_w + 12.0, y + 7.0, true).0 {
                    ui.copy = Some(invite.clone());
                    self.toast("Invite copied");
                }
                y += 72.0;
                let mut people = vec![format!("{} (you)", jams.name())];
                people.extend(guests.iter().cloned());
                let caption = match guests.len() {
                    0 => "Nobody has joined yet".to_string(),
                    1 => "1 person listening along".to_string(),
                    n => format!("{n} people listening along"),
                };
                self.jam_people(ui, lx, &mut y, body.w, &caption, &people);
                y += 12.0;
                if ui.pill("End jam", lx, y, false).0 {
                    jams.leave();
                    self.toast("Jam ended");
                }
                y += 56.0;
                self.jam_footnote(ui, lx, y, body.w, true);
            }
            JamStatus::Joined {
                host,
                members,
                latency_ms,
                note,
            } => {
                self.jam_heading(ui, lx, &mut y, &format!("In {host}'s jam"));
                let mut line = "Playing in time with the host".to_string();
                if let Some(ms) = latency_ms {
                    line.push_str(&format!(" · {ms} ms away"));
                }
                small(ui, &line, lx, y, body.w, t.dim);
                y += 26.0;
                if let Some(note) = note {
                    small(ui, &note, lx, y, body.w, t.accent);
                    y += 26.0;
                }
                y += 10.0;
                let people: Vec<String> = members
                    .iter()
                    .enumerate()
                    .map(|(i, m)| match i {
                        0 => format!("{m} (host)"),
                        _ if m == jams.name() => format!("{m} (you)"),
                        _ => m.clone(),
                    })
                    .collect();
                self.jam_people(ui, lx, &mut y, body.w, "Listening", &people);
                y += 12.0;
                small(
                    ui,
                    "Play, pause, skip and add songs as usual: everyone hears it. Volume stays yours.",
                    lx,
                    y,
                    body.w,
                    t.dim,
                );
                y += 34.0;
                if ui.pill("Leave jam", lx, y, false).0 {
                    jams.leave();
                    self.toast("Left the jam; the music keeps playing here");
                }
            }
            JamStatus::Joining => {
                self.jam_heading(ui, lx, &mut y, "Joining…");
                if ui.pill("Cancel", lx, y, false).0 {
                    jams.leave();
                }
            }
            status @ (JamStatus::Idle | JamStatus::Ended(_)) => {
                if let JamStatus::Ended(why) = status {
                    small(ui, &why, lx, y, body.w, t.accent);
                    y += 30.0;
                }
                let gap = 16.0;
                let wide = body.w >= 620.0;
                let card_w = if wide { (body.w - gap) / 2.0 } else { body.w };
                let host_r = Rect::new(lx, y, card_w, CARD_H);
                let join_r = if wide {
                    Rect::new(lx + card_w + gap, y, card_w, CARD_H)
                } else {
                    Rect::new(lx, y + CARD_H + gap, card_w, CARD_H)
                };
                for (r, title, text) in [
                    (
                        host_r,
                        "Host a jam",
                        "Friends hear what you play at the same moment, and can add songs, skip and pause too.",
                    ),
                    (join_r, "Join a jam", "Paste the invite a friend sent you."),
                ] {
                    ui.c.rect(r, t.wash(0.05), 16.0);
                    ui.label(
                        title,
                        r.x + 22.0,
                        Rect::new(r.x + 22.0, r.y + 20.0, r.w - 44.0, 28.0),
                        r.w - 44.0,
                        t.font_size + 4.0,
                        Weight::Bold,
                        t.fg,
                    );
                    self.jam_wrap(ui, text, r.x + 22.0, r.y + 56.0, r.w - 44.0);
                }
                if ui
                    .pill("Start a jam", host_r.x + 22.0, host_r.bottom() - 58.0, true)
                    .0
                {
                    jams.host();
                }
                let field = Rect::new(
                    join_r.x + 22.0,
                    join_r.bottom() - 60.0,
                    (join_r.w - 44.0 - 82.0).max(120.0),
                    38.0,
                );
                ui.text_input(&mut self.jam_invite, field, "Invite", None);
                let text = self.jam_invite.text.trim().to_owned();
                let enter =
                    self.jam_invite.focused && ui.input.keys.iter().any(|k| k.key == Key::Enter);
                let clicked = ui
                    .pill(
                        "Join",
                        field.right() + 10.0,
                        field.y + 2.0,
                        !text.is_empty(),
                    )
                    .0;
                if (clicked || enter) && !text.is_empty() {
                    match jams.join(&text) {
                        Ok(()) => {
                            self.jam_invite.text.clear();
                            self.jam_invite.focused = false;
                        }
                        Err(e) => self.toast(format!("{e:#}")),
                    }
                }
                let bottom = join_r.bottom().max(host_r.bottom());
                self.jam_footnote(ui, lx, bottom + 24.0, body.w, false);
            }
        }
    }

    fn jam_heading(&mut self, ui: &mut Ui, x: f32, y: &mut f32, s: &str) {
        let t = ui.theme.clone();
        ui.label(
            s,
            x,
            Rect::new(x, *y, 600.0, 30.0),
            600.0,
            t.font_size + 6.0,
            Weight::Bold,
            t.fg,
        );
        *y += 36.0;
    }

    /// Everyone in the jam as a row of initials with names under them.
    fn jam_people(
        &mut self,
        ui: &mut Ui,
        x: f32,
        y: &mut f32,
        w: f32,
        caption: &str,
        people: &[String],
    ) {
        let t = ui.theme.clone();
        ui.label(
            caption,
            x,
            Rect::new(x, *y, w, 20.0),
            w,
            12.0,
            Weight::Bold,
            t.dim.alpha(0.8),
        );
        *y += 30.0;
        let step = 96.0;
        let per_row = ((w / step).floor() as usize).max(1);
        for (i, name) in people.iter().enumerate() {
            let cx = x + (i % per_row) as f32 * step + 30.0;
            let cy = *y + (i / per_row) as f32 * 100.0 + 28.0;
            let hue = name
                .bytes()
                .fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
            let color = t.tint((hue % 360) as f32 / 360.0, 0.45, 0.55);
            ui.c.circle(cx, cy, 28.0, color);
            let initial: String = name
                .chars()
                .next()
                .map(|c| c.to_uppercase().collect())
                .unwrap_or_default();
            ui.label_center(
                &initial,
                Rect::new(cx - 28.0, cy - 28.0, 56.0, 56.0),
                22.0,
                Weight::Bold,
                t.fg,
            );
            ui.label_center(
                name,
                Rect::new(cx - 46.0, cy + 34.0, 92.0, 18.0),
                12.0,
                Weight::Medium,
                t.dim,
            );
        }
        let rows = people.len().div_ceil(per_row).max(1);
        *y += rows as f32 * 100.0;
    }

    /// Short text broken into lines that fit `w`.
    fn jam_wrap(&mut self, ui: &mut Ui, text: &str, x: f32, mut y: f32, w: f32) {
        let t = ui.theme.clone();
        let size = t.font_size - 1.0;
        let mut line = String::new();
        let mut lines = Vec::new();
        for word in text.split_whitespace() {
            let next = if line.is_empty() {
                word.to_owned()
            } else {
                format!("{line} {word}")
            };
            if !line.is_empty() && ui.measure(&next, size, Weight::Regular) > w {
                lines.push(std::mem::replace(&mut line, word.to_owned()));
            } else {
                line = next;
            }
        }
        lines.push(line);
        for l in lines {
            ui.label(
                &l,
                x,
                Rect::new(x, y, w, 20.0),
                w,
                size,
                Weight::Regular,
                t.dim,
            );
            y += 21.0;
        }
    }

    fn jam_footnote(&mut self, ui: &mut Ui, x: f32, y: f32, w: f32, hosting: bool) {
        let mut text = String::from(
            "Everyone plays their own copy of each song: Spotify songs need Spotify Premium, and your own files play for friends who have them too (or Spotify's version). ",
        );
        text.push_str(if hosting {
            "Friends on your network can join right away; over the internet, forward this port on your router or all join the same VPN (like Tailscale)."
        } else {
            "On the same network it just works; over the internet the host forwards the jam's port or you all join the same VPN (like Tailscale)."
        });
        self.jam_wrap(ui, &text, x, y, w.min(720.0));
    }
}
