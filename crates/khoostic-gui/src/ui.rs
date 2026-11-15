//! Immediate-mode helpers: input snapshot, theme, and a [`Ui`] that pairs the canvas with text
//! measurement in logical pixels.

use khoostic_config::GuiTheme;
use khoostic_core::CoverRef;
use khoostic_library::{CoverSize, Library};

use crate::canvas::{Canvas, Color, Rect, Weight};
use crate::images::ImageStore;
use crate::text::TextSystem;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Space,
    Enter,
    Escape,
    Backspace,
    Delete,
    Tab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
}

#[derive(Debug, Clone, Copy)]
pub struct KeyPress {
    pub key: Key,
    pub shift: bool,
    pub ctrl: bool,
}

/// Input gathered since the previous frame, in logical pixels.
#[derive(Debug, Default, Clone)]
pub struct Input {
    pub mouse: Option<(f32, f32)>,
    pub down: bool,
    pub pressed: bool,
    pub released: bool,
    pub double_click: bool,
    pub right_pressed: bool,
    /// Mouse "back" button (or similar) pressed.
    pub back_pressed: bool,
    /// Wheel movement in logical pixels; positive scrolls content down.
    pub scroll: f32,
    pub keys: Vec<KeyPress>,
    pub dropped: Vec<std::path::PathBuf>,
}

impl Input {
    /// Clears per-frame events, keeping held state (mouse position, button down).
    pub fn end_frame(&mut self) {
        self.pressed = false;
        self.released = false;
        self.double_click = false;
        self.right_pressed = false;
        self.back_pressed = false;
        self.scroll = 0.0;
        self.keys.clear();
        self.dropped.clear();
    }
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub background: Color,
    pub surface: Color,
    pub surface_hover: Color,
    pub fg: Color,
    pub dim: Color,
    pub accent: Color,
    pub playing: Color,
    pub spectrum: Vec<Color>,
    pub radius: f32,
    pub font_size: f32,
}

impl Theme {
    pub fn from_config(t: &GuiTheme, radius: f32, font_size: f32) -> (Self, Vec<String>) {
        let d = GuiTheme::default();
        let mut problems = Vec::new();
        let mut p = |v: &str, fallback: &str| {
            Color::hex(v).unwrap_or_else(|| {
                problems.push(format!("gui theme: can't parse color {v:?}"));
                Color::hex(fallback).expect("default parses")
            })
        };
        let mut theme = Self {
            background: p(&t.background, &d.background),
            surface: p(&t.surface, &d.surface),
            surface_hover: p(&t.surface_hover, &d.surface_hover),
            fg: p(&t.fg, &d.fg),
            dim: p(&t.dim, &d.dim),
            accent: p(&t.accent, &d.accent),
            playing: p(&t.playing, &d.playing),
            spectrum: t.spectrum.iter().filter_map(|c| Color::hex(c)).collect(),
            radius: radius.max(0.0),
            font_size: font_size.clamp(8.0, 40.0),
        };
        if theme.spectrum.is_empty() {
            theme.spectrum = vec![theme.accent];
        }
        (theme, problems)
    }

    pub fn gradient(&self, t: f32) -> Color {
        let n = self.spectrum.len();
        if n == 1 {
            return self.spectrum[0];
        }
        let pos = t.clamp(0.0, 1.0) * (n - 1) as f32;
        let i = (pos.floor() as usize).min(n - 2);
        self.spectrum[i].lerp(self.spectrum[i + 1], pos - i as f32)
    }
}

pub struct Ui<'a> {
    pub c: &'a mut Canvas,
    pub text: &'a mut TextSystem,
    pub images: &'a mut ImageStore,
    pub library: &'a Library,
    pub input: &'a Input,
    pub theme: &'a Theme,
}

impl Ui<'_> {
    pub fn scale(&self) -> f32 {
        self.c.scale
    }

    pub fn line_height(&self, size: f32) -> f32 {
        (size * 1.3).ceil()
    }

    pub fn measure(&mut self, s: &str, size: f32, w: Weight) -> f32 {
        let scale = self.c.scale;
        self.text.measure(s, size * scale, w).0 / scale
    }

    /// Top y for text whose cap height is optically centered in `row`. Uses the real baseline
    /// so mixed sizes and weights line up.
    fn top_for(&mut self, s: &str, row: Rect, size: f32, w: Weight) -> f32 {
        let scale = self.c.scale;
        let baseline = self.text.baseline(s, size * scale, w) / scale;
        (row.y + row.h / 2.0 + size * 0.36 - baseline).round()
    }

    /// Text vertically centered in `row` starting at `x`, shortened to `max_w`.
    pub fn label(
        &mut self,
        s: &str,
        x: f32,
        row: Rect,
        max_w: f32,
        size: f32,
        w: Weight,
        color: Color,
    ) {
        if max_w <= 4.0 || s.is_empty() {
            return;
        }
        let scale = self.c.scale;
        let fitted = self.text.fit(s, size * scale, w, max_w * scale);
        let y = self.top_for(&fitted, row, size, w);
        self.c.text_at(fitted, x, y, size, w, color);
    }

    /// Right-aligned label ending at `right`.
    pub fn label_right(
        &mut self,
        s: &str,
        right: f32,
        row: Rect,
        size: f32,
        w: Weight,
        color: Color,
    ) {
        let width = self.measure(s, size, w);
        let y = self.top_for(s, row, size, w);
        self.c.text_at(s, right - width, y, size, w, color);
    }

    pub fn label_center(&mut self, s: &str, r: Rect, size: f32, w: Weight, color: Color) {
        let width = self.measure(s, size, w);
        let y = self.top_for(s, r, size, w);
        self.c
            .text_at(s, r.x + (r.w - width) / 2.0, y, size, w, color);
    }

    pub fn hovered(&self, r: Rect) -> bool {
        self.input.mouse.is_some_and(|m| r.contains(m))
    }

    pub fn clicked(&self, r: Rect) -> bool {
        self.input.pressed && self.hovered(r)
    }

    /// A rounded text button; returns true when clicked.
    pub fn pill(&mut self, label: &str, x: f32, y: f32, active: bool) -> (bool, f32) {
        let size = self.theme.font_size - 1.0;
        let w = self.measure(label, size, Weight::Medium) + 24.0;
        let r = Rect::new(x, y, w, 30.0);
        let hover = self.hovered(r);
        let bg = if active {
            self.theme.accent.alpha(0.18)
        } else if hover {
            self.theme.surface_hover
        } else {
            self.theme.surface
        };
        self.c.rect(r, bg, 15.0);
        let fg = if active {
            self.theme.accent
        } else if hover {
            self.theme.fg
        } else {
            self.theme.dim
        };
        self.label_center(label, r, size, Weight::Medium, fg);
        (self.clicked(r), w)
    }
}

impl Ui<'_> {
    /// Cover art, or a generated placeholder while it loads / when there is none.
    /// `seed` picks the placeholder's colors so different albums look different.
    pub fn cover(
        &mut self,
        r: Rect,
        cover: Option<&CoverRef>,
        size: CoverSize,
        radius: f32,
        seed: &str,
    ) {
        if let Some(c @ CoverRef::Cached(key)) = cover {
            let suffix = if size == CoverSize::Small { "s" } else { "l" };
            let lib = self.library;
            if let Some(id) = self
                .images
                .get(&format!("{key}-{suffix}"), || lib.cover_path(c, size))
            {
                self.c.image(r, id, radius, 1.0);
                return;
            }
        }
        let h = seed
            .bytes()
            .fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619));
        let t = self.theme;
        // A muted two-hue gradient unique to the seed, so cover-less albums are tellable apart.
        let hue = (h % 360) as f32;
        let top = hsv(hue, 0.38, 0.42);
        let bottom = hsv(hue + 40.0 + (h >> 9) as f32 % 60.0, 0.45, 0.26);
        self.c.gradient(r, top, bottom, radius);
        // A small note glyph drawn with shapes, scaled to the tile.
        let s = r.w.min(r.h) / 56.0;
        let (cx, cy) = r.center();
        let ink = t.fg.alpha(0.55);
        self.c.circle(cx - 3.0 * s, cy + 6.0 * s, 5.0 * s, ink);
        self.c.rect(
            Rect::new(cx, cy - 10.0 * s, 2.5 * s, 16.0 * s),
            ink,
            1.0 * s,
        );
        self.c
            .rect(Rect::new(cx, cy - 10.0 * s, 8.0 * s, 3.0 * s), ink, 1.5 * s);
    }

    /// A one-line text field. Returns true when its text changed this frame.
    pub fn text_field(&mut self, field: &mut TextField, r: Rect, placeholder: &str) -> bool {
        let t = self.theme;
        if self.input.pressed {
            field.focused = self.hovered(r);
        }
        let hover = self.hovered(r);
        let bg = if field.focused {
            t.surface_hover
        } else if hover {
            t.surface_hover.alpha(0.7)
        } else {
            t.background.alpha(0.6)
        };
        self.c.rect(r, bg, r.h / 2.0);
        if field.focused {
            self.c.outline(r, t.accent.alpha(0.6), r.h / 2.0, 1.0);
        }
        // Magnifier icon.
        let ic = Rect::new(r.x + 12.0, r.y + r.h / 2.0 - 7.0, 14.0, 14.0);
        self.c
            .outline(Rect::new(ic.x, ic.y, 10.0, 10.0), t.dim, 5.0, 1.6);
        self.c
            .rect(Rect::new(ic.x + 9.0, ic.y + 9.5, 5.0, 2.0), t.dim, 1.0);

        let mut changed = false;
        if field.focused {
            for k in &self.input.keys {
                match k.key {
                    Key::Char(c) if !k.ctrl => {
                        field.text.push(c);
                        changed = true;
                    }
                    Key::Space => {
                        field.text.push(' ');
                        changed = true;
                    }
                    Key::Backspace if k.ctrl => {
                        let keep = field.text.trim_end().rfind(' ').map_or(0, |i| i + 1);
                        field.text.truncate(keep);
                        changed = true;
                    }
                    Key::Backspace => {
                        changed |= field.text.pop().is_some();
                    }
                    Key::Escape | Key::Enter | Key::Down | Key::Tab => field.focused = false,
                    _ => {}
                }
            }
        }
        let x = r.x + 34.0;
        let w = r.w - 44.0;
        let size = t.font_size - 1.0;
        if field.text.is_empty() {
            self.label(placeholder, x, r, w, size, Weight::Regular, t.dim);
        } else {
            self.label(&field.text, x, r, w, size, Weight::Regular, t.fg);
        }
        if field.focused {
            let tw = self.measure(&field.text, size, Weight::Regular).min(w);
            self.c.rect(
                Rect::new(x + tw + 1.0, r.y + r.h / 2.0 - 8.0, 1.5, 16.0),
                t.accent,
                0.5,
            );
        }
        changed
    }
}

#[derive(Debug, Default, Clone)]
pub struct TextField {
    pub text: String,
    pub focused: bool,
}

/// HSV → sRGB color (hue in degrees).
fn hsv(h: f32, s: f32, v: f32) -> Color {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    Color {
        r: r + m,
        g: g + m,
        b: b + m,
        a: 1.0,
    }
}
