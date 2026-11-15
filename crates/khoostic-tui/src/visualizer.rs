//! Terminal visualizers. A preview of the GPU ones: same analysis feed, braille resolution.

use std::time::Instant;

use khoostic_audio::{AnalysisFrame, BANDS, WAVEFORM_LEN};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisStyle {
    Sandstorm,
    Bars,
    Scope,
}

impl VisStyle {
    pub fn name(self) -> &'static str {
        match self {
            VisStyle::Sandstorm => "sandstorm",
            VisStyle::Bars => "spectrum",
            VisStyle::Scope => "oscilloscope",
        }
    }

    fn next(self) -> Self {
        match self {
            VisStyle::Sandstorm => VisStyle::Bars,
            VisStyle::Bars => VisStyle::Scope,
            VisStyle::Scope => VisStyle::Sandstorm,
        }
    }
}

#[derive(Clone, Copy)]
struct Particle {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
}

pub struct Visualizer {
    pub style: VisStyle,
    particles: Vec<Particle>,
    /// Physical aspect (width / height) of the area, so bursts are round.
    aspect: f32,
    t: f32,
    last: Instant,
    last_beat: u64,
    rng: fastrand::Rng,
}

impl Default for Visualizer {
    fn default() -> Self {
        Self {
            style: VisStyle::Sandstorm,
            particles: Vec::new(),
            aspect: 2.0,
            t: 0.0,
            last: Instant::now(),
            last_beat: 0,
            rng: fastrand::Rng::with_seed(0x6b68),
        }
    }
}

impl Visualizer {
    pub fn cycle(&mut self) {
        self.style = self.style.next();
    }

    /// Advances the particle simulation. Cheap when the particles aren't visible.
    pub fn update(&mut self, a: &AnalysisFrame, area: Rect) {
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32().min(0.1);
        self.last = now;
        self.t += dt;
        if self.style != VisStyle::Sandstorm || area.width == 0 || area.height == 0 {
            return;
        }
        // Terminal cells are about twice as tall as they are wide.
        self.aspect = area.width as f32 / (area.height as f32 * 2.0);
        let target = ((area.width as usize * area.height as usize) * 3 / 2).clamp(200, 6000);
        while self.particles.len() < target {
            let (x, y) = (self.rng.f32(), self.rng.f32());
            self.particles.push(Particle {
                x,
                y,
                vx: 0.0,
                vy: 0.0,
            });
        }
        self.particles.truncate(target);

        let beat = a.beat_count != self.last_beat;
        self.last_beat = a.beat_count;
        let t = self.t;
        let energy = 0.025 + a.bass * 0.35 + a.rms * 0.3;
        let follow = (dt * 2.5).min(1.0);
        let jitter = a.treble * 0.12;
        let aspect = self.aspect;
        for p in &mut self.particles {
            // A slowly evolving pseudo-curl flow field: the "wind".
            let angle = ((p.x * 2.3 + t * 0.21).sin() * 1.7
                + (p.y * 3.1 - t * 0.17).cos() * 1.3
                + ((p.x + p.y) * 1.7 + t * 0.11).sin())
                * std::f32::consts::PI;
            let (fx, fy) = (angle.cos() * energy, angle.sin() * energy);
            p.vx += (fx - p.vx) * follow + (self.rng.f32() - 0.5) * jitter * dt;
            p.vy += (fy - p.vy) * follow + (self.rng.f32() - 0.5) * jitter * dt;
            if beat {
                // Gust: radial blast plus a swirl, scaled by the beat strength.
                let (dx, dy) = ((p.x - 0.5) * aspect, p.y - 0.5);
                let d = (dx * dx + dy * dy).sqrt() + 0.08;
                let k = 0.09 * a.beat.max(0.5) / d;
                p.vx += (dx * k - dy * k * 0.6) / aspect;
                p.vy += dy * k + dx * k * 0.6;
            }
            p.x = (p.x + p.vx * dt).rem_euclid(1.0);
            p.y = (p.y + p.vy * dt * aspect).rem_euclid(1.0);
        }
    }

    pub fn render(&self, a: &AnalysisFrame, area: Rect, buf: &mut Buffer, theme: &Theme) {
        if area.width < 2 || area.height < 2 {
            return;
        }
        match self.style {
            VisStyle::Sandstorm => self.render_sandstorm(area, buf, theme),
            VisStyle::Bars => render_bars(a, area, buf, theme),
            VisStyle::Scope => render_scope(a, area, buf, theme),
        }
    }

    fn render_sandstorm(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let mut canvas = Braille::new(area);
        for p in &self.particles {
            let speed = (p.vx * p.vx + p.vy * p.vy).sqrt();
            let heat = (speed * 4.0).min(1.0);
            canvas.dot(p.x, p.y, heat);
        }
        canvas.draw(buf, |heat| theme.gradient(heat));
    }
}

/// Mini spectrum for the now-playing bar: one row of eighth blocks.
pub fn render_mini_spectrum(a: &AnalysisFrame, area: Rect, buf: &mut Buffer, theme: &Theme) {
    const BLOCKS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    let w = area.width as usize;
    for x in 0..w {
        let v = a.bands[x * BANDS / w.max(1)];
        let idx = (v * 8.0).round().clamp(0.0, 8.0) as usize;
        if let Some(cell) = buf.cell_mut((area.x + x as u16, area.y)) {
            cell.set_symbol(BLOCKS[idx]).set_fg(theme.gradient(v));
        }
    }
}

fn render_bars(a: &AnalysisFrame, area: Rect, buf: &mut Buffer, theme: &Theme) {
    const EIGHTHS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    let (w, h) = (area.width as usize, area.height as usize);
    // Wider terminals get chunky bars with gaps; narrow ones get one band per column.
    let bar = (w / BANDS).max(1);
    let gap = usize::from(bar >= 3);
    let cols = (w / bar).clamp(1, BANDS);
    let x0 = area.x as usize + (w - cols * bar) / 2;
    for c in 0..cols {
        let band = c * BANDS / cols;
        let v = a.bands[band] * h as f32;
        let peak_row = ((a.peaks[band] * h as f32).ceil() as usize).min(h);
        for row in 0..h {
            let level = v - row as f32;
            let y = area.y as usize + h - 1 - row;
            let color = theme.gradient(row as f32 / h as f32);
            let symbol = if level >= 1.0 {
                EIGHTHS[8]
            } else if level > 0.0 {
                EIGHTHS[(level * 8.0) as usize]
            } else if row + 1 == peak_row && peak_row > 0 {
                "▔"
            } else {
                continue;
            };
            for dx in 0..bar - gap {
                if let Some(cell) = buf.cell_mut(((x0 + c * bar + dx) as u16, y as u16)) {
                    cell.set_symbol(symbol).set_fg(color);
                }
            }
        }
    }
}

fn render_scope(a: &AnalysisFrame, area: Rect, buf: &mut Buffer, theme: &Theme) {
    let mut canvas = Braille::new(area);
    let peak = a.waveform.iter().fold(0.05f32, |m, s| m.max(s.abs()));
    let gain = 0.45 / peak;
    let dw = canvas.dots_w as f32;
    let mut prev: Option<(f32, f32)> = None;
    let steps = canvas.dots_w.max(2);
    for i in 0..steps {
        let s = a.waveform[i * (WAVEFORM_LEN - 1) / (steps - 1)];
        let (x, y) = (i as f32 / dw, 0.5 - s * gain);
        if let Some((px, py)) = prev {
            canvas.line(px, py, x, y, 0.5 + s.abs() * gain);
        }
        prev = Some((x, y));
    }
    canvas.draw(buf, |heat| theme.gradient(heat));
}

/// A 2x4-dots-per-cell braille raster with a per-cell "heat" used for color.
struct Braille {
    area: Rect,
    dots_w: usize,
    dots_h: usize,
    cells: Vec<(u8, f32)>,
}

impl Braille {
    fn new(area: Rect) -> Self {
        let (w, h) = (area.width as usize, area.height as usize);
        Self {
            area,
            dots_w: w * 2,
            dots_h: h * 4,
            cells: vec![(0, 0.0); w * h],
        }
    }

    /// `x`, `y` in `0..1`.
    fn dot(&mut self, x: f32, y: f32, heat: f32) {
        let dx = ((x * self.dots_w as f32) as usize).min(self.dots_w - 1);
        let dy = ((y * self.dots_h as f32) as usize).min(self.dots_h - 1);
        self.dot_px(dx, dy, heat);
    }

    fn dot_px(&mut self, dx: usize, dy: usize, heat: f32) {
        const BITS: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
        let idx = (dy / 4) * self.area.width as usize + dx / 2;
        let cell = &mut self.cells[idx];
        cell.0 |= BITS[dx % 2][dy % 4];
        cell.1 = cell.1.max(heat);
    }

    fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, heat: f32) {
        let to_px = |x: f32, max: usize| ((x * max as f32) as isize).clamp(0, max as isize - 1);
        let (mut x, mut y) = (to_px(x0, self.dots_w), to_px(y0, self.dots_h));
        let (x1, y1) = (to_px(x1, self.dots_w), to_px(y1, self.dots_h));
        let (dx, dy) = ((x1 - x).abs(), -(y1 - y).abs());
        let (sx, sy) = ((x1 - x).signum(), (y1 - y).signum());
        let mut err = dx + dy;
        loop {
            self.dot_px(x as usize, y as usize, heat);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    fn draw(&self, buf: &mut Buffer, color: impl Fn(f32) -> Color) {
        let w = self.area.width as usize;
        for (i, &(bits, heat)) in self.cells.iter().enumerate() {
            if bits == 0 {
                continue;
            }
            let ch = char::from_u32(0x2800 + bits as u32).expect("braille range");
            let pos = (self.area.x + (i % w) as u16, self.area.y + (i / w) as u16);
            if let Some(cell) = buf.cell_mut(pos) {
                cell.set_char(ch).set_style(Style::new().fg(color(heat)));
            }
        }
    }
}
