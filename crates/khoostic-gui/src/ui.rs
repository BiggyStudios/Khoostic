//! Immediate-mode helpers: input snapshot, theme, and a [`Ui`] that pairs the canvas with text
//! measurement, animation state and the shared widgets (buttons, text field, covers).

use std::f32::consts::PI;
use std::sync::Arc;

use khoostic_config::GuiTheme;
use khoostic_core::CoverRef;
use khoostic_library::{CoverSize, Library};

use crate::anim::{self, Anims};
use crate::canvas::{Canvas, Color, Rect, Weight};
use crate::icons::{self, Icon};
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
    /// Window background (sidebar and player bar sit directly on it).
    pub background: Color,
    /// The content panel.
    pub surface: Color,
    /// Raised things: menus, toasts, selected rows.
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

    /// A translucent wash of the foreground for hover/selection fills. `amount` is how far
    /// toward the foreground the panel color should *look* (sRGB terms); blending happens in
    /// linear light, where small alphas look far brighter, so convert.
    pub fn wash(&self, amount: f32) -> Color {
        if amount <= 0.0 {
            return self.fg.alpha(0.0);
        }
        let lin = |c: f32| {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let luma = |c: Color| 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
        let (b, f) = (luma(self.surface), luma(self.fg));
        let target = b + (f - b) * amount.min(1.0);
        let a = (lin(target) - lin(b)) / (lin(f) - lin(b)).max(1e-4);
        self.fg.alpha(a.clamp(0.0, 1.0))
    }

    /// Hairline separators.
    pub fn line(&self) -> Color {
        self.wash(0.09)
    }
}

pub struct Ui<'a> {
    pub c: &'a mut Canvas,
    pub text: &'a mut TextSystem,
    pub images: &'a mut ImageStore,
    pub library: &'a Arc<Library>,
    /// Downloads remote images (Spotify covers); `None` when offline/unavailable.
    pub fetch: Option<&'a UrlFetcher>,
    pub input: &'a Input,
    pub theme: &'a Theme,
    /// Animation state, owned by the GUI between frames.
    pub anim: Anims,
    /// Set when the pointer is over something clickable (the window shows a hand cursor).
    pub pointer: bool,
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

    /// Eased 0..1 hover state of `r` (marks the pointer as clickable while hovered).
    pub fn hover_t(&mut self, id: u64, r: Rect) -> f32 {
        let h = self.hovered(r);
        self.pointer |= h;
        self.anim.to(id, h as u8 as f32, anim::FAST)
    }

    /// Eased 0..1 "held down" state of `r`; drives the press squish.
    pub fn press_t(&mut self, id: u64, r: Rect) -> f32 {
        let held = self.input.down && self.hovered(r);
        self.anim.to(id ^ 0x9e37_79b9, held as u8 as f32, 30.0)
    }

    /// A rounded text button; returns (clicked, width).
    pub fn pill(&mut self, label: &str, x: f32, y: f32, active: bool) -> (bool, f32) {
        self.pill_icon(label, None, x, y, active)
    }

    /// A rounded button with an optional leading icon; returns (clicked, width).
    pub fn pill_icon(
        &mut self,
        label: &str,
        icon: Option<Icon>,
        x: f32,
        y: f32,
        active: bool,
    ) -> (bool, f32) {
        let t = self.theme.clone();
        let size = t.font_size - 1.0;
        let icon_w = if icon.is_some() { 22.0 } else { 0.0 };
        let w = self.measure(label, size, Weight::Medium) + 32.0 + icon_w;
        let r = Rect::new(x, y, w, 34.0);
        let id = anim::id(("pill", label, x as i32, y as i32));
        let h = self.hover_t(id, r);
        let p = self.press_t(id, r);
        let on = self.anim.to(id ^ 1, active as u8 as f32, anim::NORMAL);
        let shrink = 1.5 * p;
        let br = r.inset(shrink, shrink);
        let bg = t.wash(0.055 + 0.05 * h + 0.06 * on);
        self.c.rect(br, bg, br.h / 2.0);
        let fg = t.dim.lerp(t.fg, (h + on).min(1.0));
        let mut tx = r.x + 16.0;
        if let Some(icon) = icon {
            icons::draw(
                self.c,
                icon,
                Rect::new(tx - 2.0, r.y + (r.h - 16.0) / 2.0, 16.0, 16.0),
                fg,
            );
            tx += icon_w;
        }
        self.label(label, tx, r, w, size, Weight::Medium, fg);
        (self.clicked(r), w)
    }

    /// A circular icon button centered at `(cx, cy)`. `size` is the icon size; the hover disc
    /// is a little larger. Returns true when clicked.
    pub fn icon_button(
        &mut self,
        id: u64,
        icon: Icon,
        (cx, cy): (f32, f32),
        size: f32,
        color: Color,
        hover_color: Color,
    ) -> bool {
        let pad = size * 0.55;
        let r = Rect::new(
            cx - size / 2.0 - pad,
            cy - size / 2.0 - pad,
            size + 2.0 * pad,
            size + 2.0 * pad,
        );
        let h = self.hover_t(id, r);
        let p = self.press_t(id, r);
        let disc = r.w / 2.0 * (0.82 + 0.18 * h);
        self.c.circle(cx, cy, disc, self.theme.wash(0.07 * h));
        let s = size * (1.0 - 0.1 * p);
        icons::draw(
            self.c,
            icon,
            Rect::new(cx - s / 2.0, cy - s / 2.0, s, s),
            color.lerp(hover_color, h),
        );
        self.clicked(r)
    }

    /// A row of mutually exclusive options with a highlight that slides to the selected one.
    /// Returns the option clicked, if any.
    pub fn segmented(
        &mut self,
        id: u64,
        x: f32,
        y: f32,
        options: &[&str],
        selected: usize,
    ) -> Option<usize> {
        let t = self.theme.clone();
        let size = t.font_size - 1.0;
        let seg_w = options
            .iter()
            .map(|o| self.measure(o, size, Weight::Medium))
            .fold(0.0, f32::max)
            + 30.0;
        let h = 34.0;
        let outer = Rect::new(x, y, seg_w * options.len() as f32 + 6.0, h);
        self.c.rect(outer, t.wash(0.05), h / 2.0);
        let target = x + 3.0 + seg_w * selected as f32;
        let sx = self.anim.to(id, target, 20.0);
        let ind = Rect::new(sx, y + 3.0, seg_w, h - 6.0);
        self.c.glow(ind, Color::BLACK.alpha(0.3), ind.h / 2.0, 6.0);
        self.c
            .rect(ind, t.surface_hover.lerp(t.fg, 0.08), ind.h / 2.0);
        let mut clicked = None;
        for (i, o) in options.iter().enumerate() {
            let r = Rect::new(x + 3.0 + seg_w * i as f32, y + 3.0, seg_w, h - 6.0);
            let hv = self.hover_t(anim::id((id, i)), r);
            // Brightness follows the sliding highlight, so labels light up as it passes.
            let near = 1.0 - ((sx - r.x).abs() / seg_w).min(1.0);
            let fg = t.dim.lerp(t.fg, (near + 0.5 * hv).min(1.0));
            self.label_center(o, r, size, Weight::Medium, fg);
            if self.clicked(r) {
                clicked = Some(i);
            }
        }
        clicked
    }

    /// The accent-filled main action ("Play"). Returns true when clicked.
    pub fn primary_button(&mut self, r: Rect, label: &str) -> bool {
        let t = self.theme.clone();
        let id = anim::id(("primary", label, r.x as i32, r.y as i32));
        let h = self.hover_t(id, r);
        let p = self.press_t(id, r);
        let grow = 1.5 * h - 2.0 * p;
        let br = r.inset(-grow, -grow);
        self.c.glow(br, t.accent.alpha(0.18 * h), br.h / 2.0, 12.0);
        self.c
            .rect(br, t.accent.lerp(Color::WHITE, 0.1 * h), br.h / 2.0);
        let ink = t.background;
        icons::draw(
            self.c,
            Icon::Play,
            Rect::new(r.x + 16.0, r.y + (r.h - 18.0) / 2.0, 18.0, 18.0),
            ink,
        );
        self.label(
            label,
            r.x + 40.0,
            r,
            r.w - 44.0,
            t.font_size,
            Weight::Bold,
            ink,
        );
        self.clicked(r)
    }
}

impl Ui<'_> {
    /// Cover art, or a generated placeholder while it loads / when there is none. Art fades
    /// in when it finishes loading. `seed` picks the placeholder's colors.
    pub fn cover(
        &mut self,
        r: Rect,
        cover: Option<&CoverRef>,
        size: CoverSize,
        radius: f32,
        seed: &str,
    ) {
        let (key, ready) = match cover {
            Some(c @ CoverRef::Cached(key)) => {
                let suffix = if size == CoverSize::Small { "s" } else { "l" };
                let key = format!("{key}-{suffix}");
                let path = self.library.cover_path(c, size);
                let ready = self.images.get(&key, move || path);
                (key, ready)
            }
            // Remote art (Spotify) is downloaded once on a loader thread, then cached on disk.
            Some(CoverRef::Url(url)) => match self.fetch {
                Some(fetch) => {
                    let (lib, fetch, u) = (self.library.clone(), fetch.clone(), url.clone());
                    let ready = self
                        .images
                        .get(url, move || lib.covers.cached_url(&u, |u| fetch(u)));
                    (url.clone(), ready)
                }
                None => (String::new(), None),
            },
            None => (String::new(), None),
        };
        let fade = match ready {
            Some(_) => self
                .anim
                .to_from(anim::id(("cover", &key)), 1.0, 10.0, Some(0.0)),
            None => 0.0,
        };
        if fade < 1.0 {
            self.placeholder(r, radius, seed);
        }
        if let Some(id) = ready {
            self.c.image(r, id, radius, fade);
        }
    }

    fn placeholder(&mut self, r: Rect, radius: f32, seed: &str) {
        let h = seed
            .bytes()
            .fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619));
        let t = self.theme;
        // A muted two-hue gradient unique to the seed, so cover-less albums are tellable apart.
        let hue = (h % 360) as f32;
        let top = hsv(hue, 0.30, 0.30);
        let bottom = hsv(hue + 40.0 + (h >> 9) as f32 % 60.0, 0.36, 0.18);
        self.c.gradient(r, top, bottom, radius);
        let s = r.w.min(r.h) * 0.34;
        let (cx, cy) = r.center();
        icons::draw(
            self.c,
            Icon::Song,
            Rect::new(cx - s / 2.0, cy - s / 2.0, s, s),
            t.fg.alpha(0.35),
        );
    }

    /// A one-line text field with a smoothly moving caret. Returns true when its text changed.
    pub fn text_field(&mut self, field: &mut TextField, r: Rect, placeholder: &str) -> bool {
        let t = self.theme.clone();
        let dt = self.anim.dt();
        let hover = self.hovered(r);
        self.pointer |= hover;
        let inner = Rect::new(r.x + 36.0, r.y, r.w - 46.0, r.h);
        let size = t.font_size - 1.0;
        field.cursor = field.cursor.min(field.text.len());

        if self.input.pressed {
            let was = field.focused;
            field.focused = hover;
            if hover {
                // Click places the caret at the nearest character boundary.
                let mx = self.input.mouse.map_or(0.0, |m| m.0) - inner.x + field.scroll_x;
                field.cursor = self.nearest_boundary(&field.text, mx, size);
                field.idle = 0.0;
                if !was {
                    field.caret_x =
                        self.measure(&field.text[..field.cursor], size, Weight::Regular);
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
        icons::draw(
            self.c,
            Icon::Search,
            Rect::new(r.x + 13.0, r.y + (r.h - 16.0) / 2.0, 16.0, 16.0),
            t.dim.lerp(t.fg, f * 0.7),
        );

        // Caret and horizontal scroll both glide to where they should be.
        let target = self.measure(&field.text[..field.cursor], size, Weight::Regular);
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
            let y = self.top_for(&field.text, r, size, Weight::Regular);
            self.c.text_at(
                field.text.clone(),
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
}

impl TextField {
    pub fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
        self.idle = 0.0;
    }
}

/// HSV → sRGB color (hue in degrees).
pub fn hsv(h: f32, s: f32, v: f32) -> Color {
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

/// Fetches remote bytes (cover art URLs) on a loader thread.
pub type UrlFetcher = Arc<dyn Fn(&str) -> anyhow::Result<Vec<u8>> + Send + Sync>;

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
