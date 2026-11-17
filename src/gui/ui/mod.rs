//! Immediate-mode UI: input, theme, and widgets (labels, buttons, pills, segmented controls,
//! covers). Motion goes through [`anim`]; icons are drawn by [`icons`].

pub mod anim;
pub mod icons;
pub mod text_field;
pub mod themes;

use std::sync::Arc;

use anim::Anims;
use icons::Icon;

use crate::config::GuiTheme;
use crate::gui::render::images::ImageStore;
use crate::gui::render::text::TextSystem;
use crate::gui::render::{Canvas, Color, Rect, Weight};
use crate::library::{CoverSize, Library};
use crate::model::CoverRef;

pub use text_field::TextField;

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
    /// Ctrl+V or Shift+Insert; the text is in `Input::paste`.
    Paste,
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
    /// Clipboard text for `Key::Paste` presses this frame.
    pub paste: String,
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
        self.paste.clear();
    }
}

#[derive(Debug, Clone)]
pub struct Theme {
    /// The built-in theme this started from (its config key).
    pub key: &'static str,
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
    /// Round filled buttons (play) and what's drawn on them.
    pub button: Color,
    pub on_button: Color,
    pub spectrum: Vec<Color>,
    /// How colorful art-derived washes may be (0 = grayscale) and how far they lean toward
    /// the accent; see [`Theme::tint`].
    pub saturation: f32,
    pub pull: f32,
    pub radius: f32,
    pub font_size: f32,
}

impl Theme {
    pub fn from_config(t: &GuiTheme, radius: f32, font_size: f32) -> (Self, Vec<String>) {
        let mut problems = Vec::new();
        let preset = themes::preset(&t.name).unwrap_or_else(|| {
            problems.push(format!(
                "gui theme: no theme called {:?} (there's {})",
                t.name,
                themes::PRESETS
                    .iter()
                    .map(|p| p.key)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            &themes::PRESETS[0]
        });
        let mut p = |v: &Option<String>, fallback: &str| {
            let fallback = Color::hex(fallback).expect("presets parse");
            match v {
                None => fallback,
                Some(v) => Color::hex(v).unwrap_or_else(|| {
                    problems.push(format!("gui theme: can't parse color {v:?}"));
                    fallback
                }),
            }
        };
        let mut theme = Self {
            key: preset.key,
            background: p(&t.background, preset.background),
            surface: p(&t.surface, preset.surface),
            surface_hover: p(&t.surface_hover, preset.surface_hover),
            fg: p(&t.fg, preset.fg),
            dim: p(&t.dim, preset.dim),
            accent: p(&t.accent, preset.accent),
            playing: p(&t.playing, preset.playing),
            button: Color::hex(preset.button).expect("presets parse"),
            on_button: Color::hex(preset.on_button).expect("presets parse"),
            spectrum: match &t.spectrum {
                Some(s) => s.iter().filter_map(|c| Color::hex(c)).collect(),
                None => preset
                    .spectrum
                    .iter()
                    .filter_map(|c| Color::hex(c))
                    .collect(),
            },
            saturation: preset.saturation,
            pull: preset.pull,
            radius: radius.max(0.0),
            font_size: font_size.clamp(8.0, 40.0),
        };
        if theme.spectrum.is_empty() {
            theme.spectrum = vec![theme.accent];
        }
        (theme, problems)
    }

    /// A drop shadow: black on dark themes, a soft tint of the text color on light ones (black
    /// smudges pale backgrounds).
    pub fn shadow(&self, alpha: f32) -> Color {
        if self.is_light() {
            self.fg.alpha(alpha * 0.35)
        } else {
            Color::BLACK.alpha(alpha)
        }
    }

    /// Dark text on a light background (Pink) rather than the other way round.
    pub fn is_light(&self) -> bool {
        let c = self.background;
        0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b > 0.5
    }

    /// A color picked from content (an artist, an album) made to fit the theme: no more
    /// colorful than it allows, leaning toward its accent.
    pub fn tint(&self, hue: f32, s: f32, v: f32) -> Color {
        // Callers ask for dark washes (made for dark themes); on a light theme the same wash is
        // a pastel: as far below white as it was above black, but gentler.
        let (s, v) = if self.is_light() {
            (s * 0.75, 1.0 - (1.0 - v) * 0.22)
        } else {
            (s, v)
        };
        let c = hsv(hue, s * self.saturation, v);
        if self.pull <= 0.0 {
            return c;
        }
        let a = self.accent;
        let m = a.r.max(a.g).max(a.b).max(1e-3);
        let toward = Color {
            r: a.r / m * v,
            g: a.g / m * v,
            b: a.b / m * v,
            a: 1.0,
        };
        c.lerp(toward, self.pull)
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
        // Toward a dark fg on light themes too: then both differences are negative.
        let span = lin(f) - lin(b);
        if span.abs() < 1e-4 {
            return self.fg.alpha(0.0);
        }
        let a = (lin(target) - lin(b)) / span;
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
    /// Text to put on the clipboard (Ctrl+C / Ctrl+X in a text field).
    pub copy: Option<String>,
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
        self.c.glow(ind, self.theme.shadow(0.3), ind.h / 2.0, 6.0);
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
        let top = t.tint(hue, 0.30, 0.30);
        let bottom = t.tint(hue + 40.0 + (h >> 9) as f32 % 60.0, 0.36, 0.18);
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
