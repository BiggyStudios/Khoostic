//! CPU-side frame description. UI code draws into a [`Canvas`] in logical pixels; the renderer
//! turns it into GPU instances. Keeping this pure makes the UI testable without a GPU.

use bytemuck::{Pod, Zeroable};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn contains(&self, (px, py): (f32, f32)) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }

    pub fn inset(&self, dx: f32, dy: f32) -> Self {
        Self::new(
            self.x + dx,
            self.y + dy,
            (self.w - 2.0 * dx).max(0.0),
            (self.h - 2.0 * dy).max(0.0),
        )
    }

    pub fn intersect(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        Rect::new(x, y, (r - x).max(0.0), (b - y).max(0.0))
    }

    /// Splits off `w` from the left.
    pub fn split_left(&self, w: f32) -> (Rect, Rect) {
        let w = w.min(self.w);
        (
            Rect::new(self.x, self.y, w, self.h),
            Rect::new(self.x + w, self.y, self.w - w, self.h),
        )
    }

    pub fn split_top(&self, h: f32) -> (Rect, Rect) {
        let h = h.min(self.h);
        (
            Rect::new(self.x, self.y, self.w, h),
            Rect::new(self.x, self.y + h, self.w, self.h - h),
        )
    }

    pub fn split_bottom(&self, h: f32) -> (Rect, Rect) {
        let h = h.min(self.h);
        (
            Rect::new(self.x, self.y, self.w, self.h - h),
            Rect::new(self.x, self.bottom() - h, self.w, h),
        )
    }
}

/// An sRGB color with straight alpha, components `0..=1`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const TRANSPARENT: Color = Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };
    pub const WHITE: Color = Color {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };
    pub const BLACK: Color = Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };

    pub fn hex(s: &str) -> Option<Color> {
        let s = s.trim().trim_start_matches('#');
        let byte = |i: usize| u8::from_str_radix(s.get(i..i + 2)?, 16).ok();
        let (r, g, b) = (byte(0)?, byte(2)?, byte(4)?);
        let a = match s.len() {
            6 => 255,
            8 => byte(6)?,
            _ => return None,
        };
        Some(Color::rgba8(r, g, b, a))
    }

    pub fn rgba8(r: u8, g: u8, b: u8, a: u8) -> Color {
        Color {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: a as f32 / 255.0,
        }
    }

    pub fn alpha(self, a: f32) -> Color {
        Color {
            a: self.a * a,
            ..self
        }
    }

    pub fn lerp(self, o: Color, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        let l = |a: f32, b: f32| a + (b - a) * t;
        Color {
            r: l(self.r, o.r),
            g: l(self.g, o.g),
            b: l(self.b, o.b),
            a: l(self.a, o.a),
        }
    }

    /// Linear-light components for the GPU (sRGB render targets re-encode on write).
    pub fn linear(self) -> [f32; 4] {
        let f = |c: f32| {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        [f(self.r), f(self.g), f(self.b), self.a]
    }

    pub fn to_rgba8(self) -> [u8; 4] {
        let q = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
        [q(self.r), q(self.g), q(self.b), q(self.a)]
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct Quad {
    pub rect: [f32; 4],
    pub color_top: [f32; 4],
    pub color_bottom: [f32; 4],
    pub border_color: [f32; 4],
    /// radius, border width, kind, blur
    pub params: [f32; 4],
    pub clip: [f32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Box = 0,
    TriangleRight = 1,
    TriangleLeft = 2,
    Line = 3,
    Arc = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Weight {
    Regular,
    Medium,
    Bold,
}

#[derive(Debug, Clone)]
pub struct TextCmd {
    pub text: String,
    /// Top-left in physical pixels.
    pub x: f32,
    pub y: f32,
    /// Physical pixels.
    pub size: f32,
    pub weight: Weight,
    pub color: Color,
    pub clip: Rect,
}

#[derive(Debug, Clone, Copy)]
pub struct ImageCmd {
    pub image: crate::gui::render::images::ImageId,
    /// Physical pixels.
    pub rect: [f32; 4],
    pub radius: f32,
    pub opacity: f32,
    pub clip: [f32; 4],
}

#[derive(Debug, Default)]
pub struct Layer {
    pub quads: Vec<Quad>,
    pub images: Vec<ImageCmd>,
    pub texts: Vec<TextCmd>,
}

/// Two layers are enough for a minimal UI: the page, and overlays (tooltips, toasts) above it.
pub const LAYERS: usize = 2;

/// A translation and opacity applied to everything drawn while it is pushed (page transitions,
/// fading menus). Hit-testing is unaffected; effects are short-lived.
#[derive(Debug, Clone, Copy)]
struct Fx {
    dx: f32,
    dy: f32,
    alpha: f32,
}

pub struct Canvas {
    pub scale: f32,
    /// Logical size.
    pub size: (f32, f32),
    pub clear: Color,
    pub layers: [Layer; LAYERS],
    /// Keep a copy of this frame for a swipe that starts next frame.
    pub snapshot: bool,
    /// Swipe the kept frame away over this one.
    pub wipe: Option<crate::gui::render::wipe::WipeFrame>,
    layer: usize,
    clip: Vec<Rect>,
    fx: Vec<Fx>,
}

impl Canvas {
    pub fn new(physical: (u32, u32), scale: f32, clear: Color) -> Self {
        let size = (physical.0 as f32 / scale, physical.1 as f32 / scale);
        Self {
            scale,
            size,
            clear,
            layers: Default::default(),
            snapshot: false,
            wipe: None,
            layer: 0,
            clip: vec![Rect::new(0.0, 0.0, size.0, size.1)],
            fx: vec![Fx {
                dx: 0.0,
                dy: 0.0,
                alpha: 1.0,
            }],
        }
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0.0, 0.0, self.size.0, self.size.1)
    }

    pub fn set_layer(&mut self, layer: usize) {
        self.layer = layer.min(LAYERS - 1);
    }

    /// Translates by `(dx, dy)` and multiplies opacity by `alpha` until [`Canvas::pop_fx`].
    pub fn push_fx(&mut self, dx: f32, dy: f32, alpha: f32) {
        let cur = self.cur();
        self.fx.push(Fx {
            dx: cur.dx + dx,
            dy: cur.dy + dy,
            alpha: cur.alpha * alpha.clamp(0.0, 1.0),
        });
    }

    pub fn pop_fx(&mut self) {
        if self.fx.len() > 1 {
            self.fx.pop();
        }
    }

    fn cur(&self) -> Fx {
        *self.fx.last().expect("root fx")
    }

    fn moved(&self, r: Rect) -> Rect {
        let f = self.cur();
        Rect::new(r.x + f.dx, r.y + f.dy, r.w, r.h)
    }

    pub fn push_clip(&mut self, r: Rect) {
        let r = self.moved(r);
        let current = *self.clip.last().expect("root clip");
        self.clip.push(current.intersect(&r));
    }

    pub fn pop_clip(&mut self) {
        if self.clip.len() > 1 {
            self.clip.pop();
        }
    }

    fn clip_px(&self) -> [f32; 4] {
        let c = self.clip.last().expect("root clip");
        let s = self.scale;
        [c.x * s, c.y * s, c.right() * s, c.bottom() * s]
    }

    fn push_quad(&mut self, r: Rect, top: Color, bottom: Color, data: [f32; 4], params: [f32; 4]) {
        let a = self.cur().alpha;
        if a <= 0.0 {
            return;
        }
        let r = self.moved(r);
        let s = self.scale;
        let quad = Quad {
            rect: [r.x * s, r.y * s, r.w * s, r.h * s],
            color_top: top.alpha(a).linear(),
            color_bottom: bottom.alpha(a).linear(),
            border_color: data,
            params,
            clip: self.clip_px(),
        };
        self.layers[self.layer].quads.push(quad);
    }

    pub fn shape(
        &mut self,
        r: Rect,
        top: Color,
        bottom: Color,
        radius: f32,
        border: Option<(f32, Color)>,
        shape: Shape,
        blur: f32,
    ) {
        if r.w <= 0.0 || r.h <= 0.0 || (top.a <= 0.0 && bottom.a <= 0.0 && border.is_none()) {
            return;
        }
        let s = self.scale;
        let (bw, bc) = border.unwrap_or((0.0, Color::TRANSPARENT));
        let bc = bc.alpha(self.cur().alpha).linear();
        let params = [radius * s, bw * s, shape as u32 as f32, blur * s];
        self.push_quad(r, top, bottom, bc, params);
    }

    pub fn rect(&mut self, r: Rect, color: Color, radius: f32) {
        self.shape(r, color, color, radius, None, Shape::Box, 0.0);
    }

    pub fn gradient(&mut self, r: Rect, top: Color, bottom: Color, radius: f32) {
        self.shape(r, top, bottom, radius, None, Shape::Box, 0.0);
    }

    pub fn outline(&mut self, r: Rect, color: Color, radius: f32, width: f32) {
        self.shape(
            r,
            Color::TRANSPARENT,
            Color::TRANSPARENT,
            radius,
            Some((width, color)),
            Shape::Box,
            0.0,
        );
    }

    pub fn circle(&mut self, cx: f32, cy: f32, radius: f32, color: Color) {
        let r = Rect::new(cx - radius, cy - radius, radius * 2.0, radius * 2.0);
        self.rect(r, color, radius);
    }

    /// Soft glow / shadow around a rounded rect.
    pub fn glow(&mut self, r: Rect, color: Color, radius: f32, blur: f32) {
        self.shape(r, color, color, radius, None, Shape::Box, blur);
    }

    pub fn triangle(&mut self, r: Rect, color: Color, pointing_right: bool, rounding: f32) {
        let shape = if pointing_right {
            Shape::TriangleRight
        } else {
            Shape::TriangleLeft
        };
        self.shape(r, color, color, rounding, None, shape, 0.0);
    }

    /// A round-capped stroke from `a` to `b`.
    pub fn line(&mut self, a: (f32, f32), b: (f32, f32), width: f32, color: Color) {
        if color.a <= 0.0 || width <= 0.0 {
            return;
        }
        let pad = width / 2.0 + 1.0;
        let (x0, y0) = (a.0.min(b.0) - pad, a.1.min(b.1) - pad);
        let (x1, y1) = (a.0.max(b.0) + pad, a.1.max(b.1) + pad);
        let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let s = self.scale;
        let data = [
            (a.0 - cx) * s,
            (a.1 - cy) * s,
            (b.0 - cx) * s,
            (b.1 - cy) * s,
        ];
        let params = [width / 2.0 * s, 0.0, Shape::Line as u32 as f32, 0.0];
        self.push_quad(
            Rect::new(x0, y0, x1 - x0, y1 - y0),
            color,
            color,
            data,
            params,
        );
    }

    /// Strokes through `points` with round joins.
    pub fn polyline(&mut self, points: &[(f32, f32)], width: f32, color: Color) {
        for w in points.windows(2) {
            self.line(w[0], w[1], width, color);
        }
    }

    /// A round-capped arc stroke around `(cx, cy)` from angle `a0` to `a1` (radians; 0 points
    /// right, positive turns clockwise on screen). `a1 - a0 >= 2π` draws a ring.
    pub fn arc(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        a0: f32,
        a1: f32,
        width: f32,
        color: Color,
    ) {
        if color.a <= 0.0 || width <= 0.0 || radius <= 0.0 {
            return;
        }
        let ext = radius + width / 2.0 + 1.0;
        let s = self.scale;
        let data = [a0, a1, radius * s, 0.0];
        let params = [width / 2.0 * s, 0.0, Shape::Arc as u32 as f32, 0.0];
        let r = Rect::new(cx - ext, cy - ext, ext * 2.0, ext * 2.0);
        self.push_quad(r, color, color, data, params);
    }

    /// Draws a GPU image (see [`crate::gui::render::images::ImageStore`]) with rounded corners.
    pub fn image(
        &mut self,
        r: Rect,
        image: crate::gui::render::images::ImageId,
        radius: f32,
        opacity: f32,
    ) {
        if r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        let r = self.moved(r);
        let s = self.scale;
        let cmd = ImageCmd {
            image,
            rect: [r.x * s, r.y * s, r.w * s, r.h * s],
            radius: radius * s,
            opacity: opacity * self.cur().alpha,
            clip: self.clip_px(),
        };
        self.layers[self.layer].images.push(cmd);
    }

    /// Queues text with its top-left at `(x, y)` (logical). Measuring/alignment is done by the
    /// caller through [`crate::gui::render::text::TextSystem`], which knows the fonts.
    pub fn text_at(
        &mut self,
        text: impl Into<String>,
        x: f32,
        y: f32,
        size: f32,
        weight: Weight,
        color: Color,
    ) {
        let f = self.cur();
        if f.alpha <= 0.0 {
            return;
        }
        let s = self.scale;
        let clip = *self.clip.last().expect("root clip");
        self.layers[self.layer].texts.push(TextCmd {
            text: text.into(),
            x: (x + f.dx) * s,
            y: (y + f.dy) * s,
            size: size * s,
            weight,
            color: color.alpha(f.alpha),
            clip: Rect::new(clip.x * s, clip.y * s, clip.w * s, clip.h * s),
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_colors() {
        assert_eq!(Color::hex("#ff0000"), Some(Color::rgba8(255, 0, 0, 255)));
        assert_eq!(Color::hex("00ff0080").unwrap().to_rgba8(), [0, 255, 0, 128]);
        assert_eq!(Color::hex("#fff"), None);
        let lin = Color::hex("#808080").unwrap().linear();
        assert!((lin[0] - 0.2158).abs() < 0.001);
    }

    #[test]
    fn clipping_and_scale() {
        let mut c = Canvas::new((200, 100), 2.0, Color::BLACK);
        assert_eq!(c.size, (100.0, 50.0));
        c.push_clip(Rect::new(10.0, 10.0, 200.0, 20.0));
        c.rect(Rect::new(0.0, 0.0, 5.0, 5.0), Color::WHITE, 0.0);
        let q = c.layers[0].quads[0];
        assert_eq!(q.rect, [0.0, 0.0, 10.0, 10.0]);
        assert_eq!(q.clip, [20.0, 20.0, 200.0, 60.0]);
        c.pop_clip();
        c.pop_clip(); // never pops the root
        c.rect(Rect::new(0.0, 0.0, 0.0, 5.0), Color::WHITE, 0.0); // empty: skipped
        assert_eq!(c.layers[0].quads.len(), 1);
    }
}
