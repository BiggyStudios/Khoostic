//! Immediate-mode helpers: input snapshot, theme, and a [`Ui`] that pairs the canvas with text
//! measurement in logical pixels.

use khoostic_config::GuiTheme;

use crate::canvas::{Canvas, Color, Rect, Weight};
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
