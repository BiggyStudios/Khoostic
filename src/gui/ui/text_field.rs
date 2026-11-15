//! Text fields: editing, selection-free caret motion and the eased caret.

use std::f32::consts::PI;

use crate::gui::render::{Rect, Weight};
use crate::gui::ui::anim;
use crate::gui::ui::icons::{self, Icon};
use crate::gui::ui::{Key, Ui};

impl Ui<'_> {
    /// The search box. Returns true when its text changed.
    pub fn text_field(&mut self, field: &mut TextField, r: Rect, placeholder: &str) -> bool {
        self.text_input(field, r, placeholder, Some(Icon::Search))
    }

    /// A one-line text field with a smoothly moving caret and an optional leading icon.
    /// Returns true when its text changed.
    pub fn text_input(
        &mut self,
        field: &mut TextField,
        r: Rect,
        placeholder: &str,
        icon: Option<Icon>,
    ) -> bool {
        let t = self.theme.clone();
        let dt = self.anim.dt();
        let hover = self.hovered(r);
        self.pointer |= hover;
        let lead = if icon.is_some() { 36.0 } else { 16.0 };
        let inner = Rect::new(r.x + lead, r.y, r.w - lead - 10.0, r.h);
        let size = t.font_size - 1.0;
        field.cursor = field.cursor.min(field.text.len());

        if self.input.pressed {
            let was = field.focused;
            field.focused = hover;
            if hover {
                // Click places the caret at the nearest character boundary.
                let mx = self.input.mouse.map_or(0.0, |m| m.0) - inner.x + field.scroll_x;
                field.cursor = if field.secret {
                    field.text.len()
                } else {
                    self.nearest_boundary(&field.text, mx, size)
                };
                field.idle = 0.0;
                if !was {
                    let before = field.shown_before_cursor();
                    field.caret_x = self.measure(&before, size, Weight::Regular);
                }
            }
        }

        let mut changed = false;
        if field.focused {
            for k in &self.input.keys {
                let before = (field.text.len(), field.cursor);
                match k.key {
                    Key::Char(c) if !k.ctrl => {
                        field.text.insert(field.cursor, c);
                        field.cursor += c.len_utf8();
                        changed = true;
                    }
                    Key::Space => {
                        field.text.insert(field.cursor, ' ');
                        field.cursor += 1;
                        changed = true;
                    }
                    Key::Backspace => {
                        let from = if k.ctrl {
                            word_left(&field.text, field.cursor)
                        } else {
                            prev_boundary(&field.text, field.cursor)
                        };
                        changed |= from < field.cursor;
                        field.text.replace_range(from..field.cursor, "");
                        field.cursor = from;
                    }
                    Key::Delete => {
                        let to = if k.ctrl {
                            word_right(&field.text, field.cursor)
                        } else {
                            next_boundary(&field.text, field.cursor)
                        };
                        changed |= to > field.cursor;
                        field.text.replace_range(field.cursor..to, "");
                    }
                    Key::Left if k.ctrl => field.cursor = word_left(&field.text, field.cursor),
                    Key::Right if k.ctrl => field.cursor = word_right(&field.text, field.cursor),
                    Key::Left => field.cursor = prev_boundary(&field.text, field.cursor),
                    Key::Right => field.cursor = next_boundary(&field.text, field.cursor),
                    Key::Home => field.cursor = 0,
                    Key::End => field.cursor = field.text.len(),
                    Key::Escape | Key::Enter | Key::Down | Key::Tab => {
                        // Keys after this one belong to the page, not the field.
                        field.focused = false;
                        break;
                    }
                    _ => {}
                }
                if (field.text.len(), field.cursor) != before {
                    field.idle = 0.0;
                }
            }
        }

        let id = anim::id(("field", r.x as i32, r.y as i32));
        let f = self.anim.to(id, field.focused as u8 as f32, anim::NORMAL);
        let h = self.anim.to(id ^ 1, hover as u8 as f32, anim::FAST);
        self.c
            .rect(r, t.wash(0.05 + 0.025 * h + 0.03 * f), r.h / 2.0);
        if f > 0.01 {
            self.c.outline(r, t.accent.alpha(0.55 * f), r.h / 2.0, 1.0);
        }
        if let Some(icon) = icon {
            icons::draw(
                self.c,
                icon,
                Rect::new(r.x + 13.0, r.y + (r.h - 16.0) / 2.0, 16.0, 16.0),
                t.dim.lerp(t.fg, f * 0.7),
            );
        }

        // Caret and horizontal scroll both glide to where they should be.
        let before = field.shown_before_cursor();
        let target = self.measure(&before, size, Weight::Regular);
        let k = 1.0 - (-28.0 * dt).exp();
        if (target - field.caret_x).abs() > 0.3 {
            field.caret_x += (target - field.caret_x) * k;
            self.anim.keep_alive();
        } else {
            field.caret_x = target;
        }
        let view_w = inner.w - 4.0;
        let want_scroll = if field.caret_x - field.scroll_x > view_w {
            field.caret_x - view_w
        } else if field.caret_x < field.scroll_x {
            field.caret_x
        } else {
            field.scroll_x
        };
        let want_scroll = want_scroll.max(0.0);
        if (want_scroll - field.scroll_x).abs() > 0.3 {
            field.scroll_x += (want_scroll - field.scroll_x) * k;
            self.anim.keep_alive();
        } else {
            field.scroll_x = want_scroll;
        }

        self.c.push_clip(inner);
        let ph = self
            .anim
            .to(id ^ 2, field.text.is_empty() as u8 as f32, anim::FAST);
        if ph > 0.01 {
            self.label(
                placeholder,
                inner.x + 6.0 * (1.0 - ph),
                r,
                inner.w,
                size,
                Weight::Regular,
                t.dim.alpha(ph),
            );
        }
        if !field.text.is_empty() {
            let shown = field.shown();
            let y = self.top_for(&shown, r, size, Weight::Regular);
            self.c.text_at(
                shown,
                inner.x - field.scroll_x,
                y,
                size,
                Weight::Regular,
                t.fg,
            );
        }
        if f > 0.01 {
            // Solid while typing, then a soft blink; it settles solid after a while so an idle
            // focused field doesn't keep the GPU busy.
            field.idle += dt;
            let blink = if field.idle < 0.5 || field.idle > 10.0 {
                1.0
            } else {
                self.anim.keep_alive();
                0.55 + 0.45 * ((field.idle - 0.5) * PI * 2.0 / 1.1).cos()
            };
            let ch = 17.0 * f;
            let x = inner.x + field.caret_x - field.scroll_x;
            self.c.rect(
                Rect::new(x, r.y + (r.h - ch) / 2.0, 1.6, ch),
                t.accent.alpha(blink * f),
                0.8,
            );
        }
        self.c.pop_clip();
        changed
    }

    fn nearest_boundary(&mut self, text: &str, x: f32, size: f32) -> usize {
        let mut best = (0, f32::MAX);
        for (i, _) in text
            .char_indices()
            .chain(std::iter::once((text.len(), ' ')))
        {
            let d = (self.measure(&text[..i], size, Weight::Regular) - x).abs();
            if d < best.1 {
                best = (i, d);
            }
        }
        best.0
    }
}

fn prev_boundary(s: &str, i: usize) -> usize {
    s[..i].char_indices().next_back().map_or(0, |(j, _)| j)
}

fn next_boundary(s: &str, i: usize) -> usize {
    s[i..].chars().next().map_or(i, |c| i + c.len_utf8())
}

fn word_left(s: &str, i: usize) -> usize {
    let head = s[..i].trim_end();
    head.rfind(' ').map_or(0, |j| j + 1)
}

fn word_right(s: &str, i: usize) -> usize {
    let rest = &s[i..];
    let skip = rest.len() - rest.trim_start().len();
    rest[skip..].find(' ').map_or(s.len(), |j| i + skip + j)
}

/// A single-line editable text.
#[derive(Debug, Default, Clone)]
pub struct TextField {
    pub text: String,
    pub focused: bool,
    /// Caret position as a byte index into `text`.
    pub cursor: usize,
    /// Drawn caret x (eases toward the real position).
    caret_x: f32,
    scroll_x: f32,
    /// Seconds since the last edit or caret move (drives the blink).
    idle: f32,
    /// Shown as dots (API secrets).
    pub secret: bool,
}

impl TextField {
    pub fn secret() -> Self {
        Self {
            secret: true,
            ..Default::default()
        }
    }

    /// What is drawn: the text, or a dot per character for secrets.
    fn shown(&self) -> String {
        if self.secret {
            "•".repeat(self.text.chars().count())
        } else {
            self.text.clone()
        }
    }

    fn shown_before_cursor(&self) -> String {
        let head = &self.text[..self.cursor.min(self.text.len())];
        if self.secret {
            "•".repeat(head.chars().count())
        } else {
            head.to_owned()
        }
    }

    pub fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
        self.idle = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_word_motion() {
        let s = "hello big world";
        assert_eq!(word_left(s, s.len()), 10);
        assert_eq!(word_left(s, 10), 6);
        assert_eq!(word_right(s, 0), 5);
        assert_eq!(word_right(s, 5), 9);
        assert_eq!(word_right(s, 10), s.len());
        let u = "aé日";
        assert_eq!(next_boundary(u, 1), 3);
        assert_eq!(prev_boundary(u, u.len()), 3);
    }
}
