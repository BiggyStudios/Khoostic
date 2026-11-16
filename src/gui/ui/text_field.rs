//! Text fields: editing, selection, clipboard and the eased caret.

use std::f32::consts::PI;

use crate::gui::render::{Rect, Weight};
use crate::gui::ui::anim;
use crate::gui::ui::icons::{self, Icon};
use crate::gui::ui::{Key, KeyPress, Ui};

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
        field.clamp();

        // Click places the caret, dragging selects, double-click selects a word.
        let mx = self.input.mouse.map_or(0.0, |m| m.0) - inner.x + field.scroll_x;
        if self.input.pressed {
            let was = field.focused;
            field.focused = hover;
            field.anchor = None;
            field.dragging = false;
            if hover {
                field.cursor = self.boundary_at(field, mx, size);
                field.idle = 0.0;
                if self.input.double_click {
                    let (a, b) = field.word_at(field.cursor);
                    field.anchor = Some(a);
                    field.cursor = b;
                } else {
                    field.anchor = Some(field.cursor);
                    field.dragging = true;
                }
                if !was {
                    let before = field.shown_upto(field.cursor);
                    field.caret_x = self.measure(&before, size, Weight::Regular);
                }
            }
        } else if field.dragging {
            if self.input.down {
                let at = self.boundary_at(field, mx, size);
                if at != field.cursor {
                    field.cursor = at;
                    field.idle = 0.0;
                }
            } else {
                field.dragging = false;
            }
        }

        let mut changed = false;
        if field.focused {
            for k in &self.input.keys {
                let before = (field.cursor, field.anchor);
                match field.apply(k, &self.input.paste, &mut self.copy) {
                    // Keys after this one belong to the page, not the field.
                    None => break,
                    Some(c) => changed |= c,
                }
                if changed || (field.cursor, field.anchor) != before {
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
        let before = field.shown_upto(field.cursor);
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
        if let (true, Some((a, b))) = (f > 0.01, field.selection()) {
            let xa = self.measure(&field.shown_upto(a), size, Weight::Regular);
            let xb = self.measure(&field.shown_upto(b), size, Weight::Regular);
            let sh = 22.0;
            self.c.rect(
                Rect::new(
                    inner.x - field.scroll_x + xa,
                    r.y + (r.h - sh) / 2.0,
                    xb - xa,
                    sh,
                ),
                t.accent.alpha(0.32 * f),
                4.0,
            );
        }
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
            let shown = field.shown_upto(field.text.len());
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

    /// The character boundary nearest to `x` (field coordinates, scroll included).
    fn boundary_at(&mut self, field: &TextField, x: f32, size: f32) -> usize {
        let mut best = (0, f32::MAX);
        let ends = field.text.char_indices().map(|(i, _)| i);
        for i in ends.chain(std::iter::once(field.text.len())) {
            let d = (self.measure(&field.shown_upto(i), size, Weight::Regular) - x).abs();
            if d < best.1 {
                best = (i, d);
            }
        }
        best.0
    }
}

/// Pasted text as one line: line breaks and tabs become spaces, other control characters go,
/// and the ends are trimmed (a copied key often brings a trailing newline).
fn one_line(s: &str) -> String {
    let s: String = s
        .chars()
        .filter_map(|c| match c {
            '\r' | '\n' | '\t' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect();
    s.trim().to_owned()
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
    /// The other end of the selection (byte index); the selection runs to `cursor`.
    anchor: Option<usize>,
    /// A mouse drag that started in this field is selecting.
    dragging: bool,
    /// Drawn caret x (eases toward the real position).
    caret_x: f32,
    scroll_x: f32,
    /// Seconds since the last edit or caret move (drives the blink).
    idle: f32,
    /// Shown as dots (API secrets), and never copied out.
    pub secret: bool,
}

impl TextField {
    pub fn secret() -> Self {
        Self {
            secret: true,
            ..Default::default()
        }
    }

    pub fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
        self.anchor = None;
        self.idle = 0.0;
    }

    /// Keeps the caret and selection inside the text (callers may edit `text` directly).
    fn clamp(&mut self) {
        let ok = |i: usize, s: &str| i <= s.len() && s.is_char_boundary(i);
        if !ok(self.cursor, &self.text) {
            self.cursor = self.text.len();
        }
        if !self.anchor.is_some_and(|a| ok(a, &self.text)) {
            self.anchor = None;
        }
    }

    /// The selected byte range, if anything is selected.
    fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor.filter(|&a| a != self.cursor)?;
        Some((a.min(self.cursor), a.max(self.cursor)))
    }

    /// Removes the selected text. Returns whether anything was removed.
    fn delete_selection(&mut self) -> bool {
        let sel = self.selection();
        self.anchor = None;
        let Some((a, b)) = sel else { return false };
        self.text.replace_range(a..b, "");
        self.cursor = a;
        true
    }

    /// Replaces the selection (if any) with `s` at the caret.
    fn insert(&mut self, s: &str) -> bool {
        let removed = self.delete_selection();
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
        removed || !s.is_empty()
    }

    /// The word around byte `i` (the whole text for secrets, which have no visible words).
    fn word_at(&self, i: usize) -> (usize, usize) {
        if self.secret {
            return (0, self.text.len());
        }
        let start = self.text[..i].rfind(' ').map_or(0, |j| j + 1);
        let end = self.text[i..].find(' ').map_or(self.text.len(), |j| i + j);
        (start, end)
    }

    /// Applies one key. Returns whether the text changed, or `None` when the key moves focus
    /// away from the field. Ctrl+C / Ctrl+X put the selection in `copy`.
    fn apply(&mut self, k: &KeyPress, paste: &str, copy: &mut Option<String>) -> Option<bool> {
        let len = self.text.len();
        let changed = match k.key {
            Key::Char(c) if k.ctrl => match c.to_ascii_lowercase() {
                'a' => {
                    self.anchor = Some(0);
                    self.cursor = len;
                    false
                }
                'c' | 'x' => match self.selection() {
                    Some((a, b)) if !self.secret => {
                        *copy = Some(self.text[a..b].to_owned());
                        c.eq_ignore_ascii_case(&'x') && self.delete_selection()
                    }
                    _ => false,
                },
                _ => false,
            },
            Key::Char(c) => self.insert(c.encode_utf8(&mut [0; 4])),
            Key::Space => self.insert(" "),
            Key::Paste => self.insert(&one_line(paste)),
            Key::Backspace | Key::Delete if self.selection().is_some() => self.delete_selection(),
            Key::Backspace => {
                let from = if k.ctrl {
                    word_left(&self.text, self.cursor)
                } else {
                    prev_boundary(&self.text, self.cursor)
                };
                self.text.replace_range(from..self.cursor, "");
                let removed = from < self.cursor;
                self.cursor = from;
                self.anchor = None;
                removed
            }
            Key::Delete => {
                let to = if k.ctrl {
                    word_right(&self.text, self.cursor)
                } else {
                    next_boundary(&self.text, self.cursor)
                };
                self.text.replace_range(self.cursor..to, "");
                self.anchor = None;
                to > self.cursor
            }
            Key::Left | Key::Right | Key::Home | Key::End => {
                // Without Shift, Left/Right first collapse a selection to that side.
                let sel = self.selection().filter(|_| !k.shift && !k.ctrl);
                let to = match (k.key, sel) {
                    (Key::Left, Some((a, _))) => a,
                    (Key::Right, Some((_, b))) => b,
                    (Key::Left, _) if k.ctrl => word_left(&self.text, self.cursor),
                    (Key::Right, _) if k.ctrl => word_right(&self.text, self.cursor),
                    (Key::Left, _) => prev_boundary(&self.text, self.cursor),
                    (Key::Right, _) => next_boundary(&self.text, self.cursor),
                    (Key::Home, _) => 0,
                    _ => len,
                };
                if k.shift {
                    let from = self.cursor;
                    self.anchor.get_or_insert(from);
                } else {
                    self.anchor = None;
                }
                self.cursor = to;
                false
            }
            Key::Escape | Key::Enter | Key::Down | Key::Tab => {
                self.focused = false;
                self.anchor = None;
                return None;
            }
            _ => false,
        };
        Some(changed)
    }

    /// What is drawn for `text[..i]`: the text, or a dot per character for secrets.
    fn shown_upto(&self, i: usize) -> String {
        let head = &self.text[..i.min(self.text.len())];
        if self.secret {
            "•".repeat(head.chars().count())
        } else {
            head.to_owned()
        }
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

    #[test]
    fn pasted_text_is_one_line() {
        assert_eq!(one_line("  abc123\r\n"), "abc123");
        assert_eq!(
            one_line("daft punk\none more time"),
            "daft punk one more time"
        );
        assert_eq!(one_line("a\u{7}b\tc"), "ab c");
    }

    fn press(f: &mut TextField, key: Key, shift: bool, ctrl: bool) -> Option<String> {
        let mut copy = None;
        f.apply(&KeyPress { key, shift, ctrl }, "pasted", &mut copy);
        copy
    }

    fn field(text: &str) -> TextField {
        let mut f = TextField::default();
        f.set(text);
        f
    }

    #[test]
    fn select_all_copy_cut_and_replace() {
        let mut f = field("hello world");
        press(&mut f, Key::Char('a'), false, true);
        assert_eq!(
            press(&mut f, Key::Char('c'), false, true).as_deref(),
            Some("hello world")
        );
        assert_eq!(f.text, "hello world");
        press(&mut f, Key::Char('x'), false, false);
        assert_eq!((f.text.as_str(), f.cursor), ("x", 1));

        let mut f = field("hello world");
        press(&mut f, Key::Char('a'), false, true);
        assert_eq!(
            press(&mut f, Key::Char('x'), false, true).as_deref(),
            Some("hello world")
        );
        assert_eq!(f.text, "");

        let mut f = field("hello world");
        press(&mut f, Key::Char('a'), false, true);
        press(&mut f, Key::Paste, false, true);
        assert_eq!(f.text, "pasted");
    }

    #[test]
    fn shift_arrows_select() {
        let mut f = field("hello world");
        press(&mut f, Key::Left, true, true);
        assert_eq!(
            press(&mut f, Key::Char('c'), false, true).as_deref(),
            Some("world")
        );
        press(&mut f, Key::Backspace, false, false);
        assert_eq!(f.text, "hello ");
        // Left without Shift collapses a selection to its start.
        let mut f = field("abc");
        press(&mut f, Key::Home, true, false);
        press(&mut f, Key::Left, false, false);
        assert_eq!((f.cursor, f.selection()), (0, None));
        // Nothing selected: copy leaves the clipboard alone.
        assert_eq!(press(&mut f, Key::Char('c'), false, true), None);
    }

    #[test]
    fn secrets_are_never_copied() {
        let mut f = TextField::secret();
        f.set("api-key");
        press(&mut f, Key::Char('a'), false, true);
        assert_eq!(press(&mut f, Key::Char('c'), false, true), None);
        assert_eq!(press(&mut f, Key::Char('x'), false, true), None);
        assert_eq!(f.text, "api-key");
        press(&mut f, Key::Backspace, false, false);
        assert_eq!(f.text, "");
    }

    #[test]
    fn double_click_word() {
        let f = field("daft punk live");
        assert_eq!(f.word_at(7), (5, 9));
        assert_eq!(f.word_at(0), (0, 4));
        assert_eq!(f.word_at(14), (10, 14));
    }
}
