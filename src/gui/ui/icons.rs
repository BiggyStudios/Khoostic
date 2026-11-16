//! Stroked vector icons on a 24-unit grid, drawn with the canvas's SDF lines and arcs so they
//! stay crisp at any scale.

use std::f32::consts::{FRAC_PI_4, PI, TAU};

use crate::gui::render::{Canvas, Color, Rect};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Icon {
    Play,
    Pause,
    Prev,
    Next,
    Shuffle,
    Repeat,
    RepeatOne,
    /// Speaker with waves for a 0..1 volume (0 = muted).
    Volume(f32),
    Search,
    Album,
    Artist,
    Song,
    Playlist,
    Queue,
    Folder,
    Visualizer,
    Settings,
    Heart,
    Plus,
    Close,
    Back,
    Forward,
    Home,
    Up,
    Spotify,
    /// Song radio: a signal radiating from a point.
    Radio,
    /// Autoplay (endless): an infinity sign.
    Autoplay,
    /// Window controls.
    Minimize,
    Maximize,
    /// Two overlapping squares (un-maximize).
    Restore,
    /// A solid heart (liked).
    HeartFilled,
    Check,
    /// Lyrics: a microphone.
    Mic,
    /// Your Library: books on a shelf.
    Library,
    /// Collapse / expand the library panel.
    PanelLeft,
    /// A filled badge with a check (verified artist).
    Verified,
    /// Three dots in a row (more options).
    More,
}

struct Pen<'a> {
    c: &'a mut Canvas,
    r: Rect,
    k: f32,
    w: f32,
    color: Color,
}

impl Pen<'_> {
    fn p(&self, x: f32, y: f32) -> (f32, f32) {
        (self.r.x + x * self.k, self.r.y + y * self.k)
    }

    fn line(&mut self, a: (f32, f32), b: (f32, f32)) {
        let (a, b) = (self.p(a.0, a.1), self.p(b.0, b.1));
        self.c.line(a, b, self.w, self.color);
    }

    fn poly(&mut self, pts: &[(f32, f32)]) {
        let pts: Vec<_> = pts.iter().map(|&(x, y)| self.p(x, y)).collect();
        self.c.polyline(&pts, self.w, self.color);
    }

    fn arc(&mut self, cx: f32, cy: f32, r: f32, a0: f32, a1: f32) {
        let (x, y) = self.p(cx, cy);
        self.c.arc(x, y, r * self.k, a0, a1, self.w, self.color);
    }

    fn ring(&mut self, cx: f32, cy: f32, r: f32) {
        self.arc(cx, cy, r, 0.0, TAU);
    }

    fn dot(&mut self, cx: f32, cy: f32, r: f32) {
        let (x, y) = self.p(cx, cy);
        self.c.circle(x, y, r * self.k, self.color);
    }

    fn fill(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32) {
        let (px, py) = self.p(x, y);
        self.c.rect(
            Rect::new(px, py, w * self.k, h * self.k),
            self.color,
            radius * self.k,
        );
    }

    fn tri(&mut self, x: f32, y: f32, w: f32, h: f32, right: bool) {
        let (px, py) = self.p(x, y);
        self.c.triangle(
            Rect::new(px, py, w * self.k, h * self.k),
            self.color,
            right,
            1.2 * self.k,
        );
    }
}

/// Draws `icon` into the square `r` (its width sets the scale).
pub fn draw(c: &mut Canvas, icon: Icon, r: Rect, color: Color) {
    let k = r.w / 24.0;
    let mut p = Pen {
        c,
        r,
        k,
        w: (1.8 * k).max(1.1),
        color,
    };
    match icon {
        Icon::Play => p.tri(7.0, 4.5, 13.0, 15.0, true),
        Icon::Pause => {
            p.fill(6.0, 5.0, 4.2, 14.0, 1.4);
            p.fill(13.8, 5.0, 4.2, 14.0, 1.4);
        }
        Icon::Next => {
            p.tri(5.0, 6.0, 11.0, 12.0, true);
            p.fill(16.6, 6.0, 2.4, 12.0, 1.2);
        }
        Icon::Prev => {
            p.tri(8.0, 6.0, 11.0, 12.0, false);
            p.fill(5.0, 6.0, 2.4, 12.0, 1.2);
        }
        Icon::Shuffle => {
            p.poly(&[(3.5, 7.0), (7.5, 7.0), (13.5, 17.0), (19.5, 17.0)]);
            p.poly(&[(3.5, 17.0), (7.5, 17.0), (13.5, 7.0), (19.5, 7.0)]);
            p.poly(&[(17.0, 4.5), (19.5, 7.0), (17.0, 9.5)]);
            p.poly(&[(17.0, 14.5), (19.5, 17.0), (17.0, 19.5)]);
        }
        Icon::Repeat | Icon::RepeatOne => {
            p.poly(&[(4.0, 12.5), (4.0, 10.0)]);
            p.arc(7.5, 10.0, 3.5, PI, 1.5 * PI);
            p.poly(&[(7.5, 6.5), (19.0, 6.5)]);
            p.poly(&[(16.5, 4.0), (19.0, 6.5), (16.5, 9.0)]);
            p.poly(&[(20.0, 11.5), (20.0, 14.0)]);
            p.arc(16.5, 14.0, 3.5, 0.0, 0.5 * PI);
            p.poly(&[(16.5, 17.5), (5.0, 17.5)]);
            p.poly(&[(7.5, 15.0), (5.0, 17.5), (7.5, 20.0)]);
            if icon == Icon::RepeatOne {
                p.poly(&[(10.8, 10.6), (12.2, 9.6), (12.2, 14.4)]);
            }
        }
        Icon::Volume(v) => {
            p.fill(3.5, 9.0, 4.5, 6.0, 1.2);
            p.tri(4.5, 4.5, 8.0, 15.0, false);
            if v <= 0.001 {
                p.line((15.5, 9.5), (20.5, 14.5));
                p.line((20.5, 9.5), (15.5, 14.5));
            } else {
                p.arc(12.0, 12.0, 4.0, -FRAC_PI_4, FRAC_PI_4);
                if v > 0.5 {
                    p.arc(12.0, 12.0, 7.5, -FRAC_PI_4, FRAC_PI_4);
                }
            }
        }
        Icon::Search => {
            p.ring(10.5, 10.5, 6.0);
            p.line((15.0, 15.0), (19.5, 19.5));
        }
        Icon::Album => {
            p.ring(12.0, 12.0, 8.5);
            p.dot(12.0, 12.0, 2.2);
        }
        Icon::Artist => {
            p.ring(12.0, 8.0, 3.8);
            p.arc(12.0, 21.0, 7.0, PI + 0.25, TAU - 0.25);
        }
        Icon::Song => {
            p.ring(8.5, 17.0, 2.8);
            p.line((11.3, 17.0), (11.3, 5.0));
            p.line((11.3, 5.0), (17.5, 7.0));
        }
        Icon::Playlist => {
            p.line((4.0, 6.5), (16.0, 6.5));
            p.line((4.0, 11.5), (16.0, 11.5));
            p.line((4.0, 16.5), (10.0, 16.5));
            p.ring(16.0, 17.0, 2.3);
            p.line((18.3, 17.0), (18.3, 9.5));
            p.line((18.3, 9.5), (21.0, 10.5));
        }
        Icon::Queue => {
            p.line((4.0, 6.0), (20.0, 6.0));
            p.line((4.0, 12.0), (12.0, 12.0));
            p.line((4.0, 18.0), (12.0, 18.0));
            p.tri(15.5, 11.0, 6.0, 8.0, true);
        }
        Icon::Folder => {
            let w = p.w;
            p.c.outline(
                Rect::new(r.x + 3.5 * k, r.y + 8.0 * k, 17.0 * k, 11.5 * k),
                color,
                2.5 * k,
                w,
            );
            p.poly(&[(3.5, 9.0), (3.5, 5.5), (9.0, 5.5), (10.8, 8.0)]);
        }
        Icon::Visualizer => {
            for (x, h) in [(5.0, 8.0), (9.67, 14.0), (14.33, 5.0), (19.0, 11.0)] {
                p.line((x, 19.0 - h), (x, 19.0));
            }
        }
        Icon::Settings => {
            p.ring(12.0, 12.0, 3.0);
            p.ring(12.0, 12.0, 7.0);
            let w = p.w;
            p.w = w * 1.6;
            for i in 0..8 {
                let a = i as f32 * FRAC_PI_4;
                p.line(
                    (12.0 + 8.0 * a.cos(), 12.0 + 8.0 * a.sin()),
                    (12.0 + 9.2 * a.cos(), 12.0 + 9.2 * a.sin()),
                );
            }
        }
        Icon::Heart => {
            p.arc(8.5, 9.5, 4.0, 0.75 * PI, TAU);
            p.arc(15.5, 9.5, 4.0, PI, 2.25 * PI);
            let s = std::f32::consts::FRAC_1_SQRT_2 * 4.0;
            p.line((8.5 - s, 9.5 + s), (12.0, 19.0));
            p.line((15.5 + s, 9.5 + s), (12.0, 19.0));
        }
        Icon::Plus => {
            p.line((12.0, 5.0), (12.0, 19.0));
            p.line((5.0, 12.0), (19.0, 12.0));
        }
        Icon::Close => {
            p.line((6.0, 6.0), (18.0, 18.0));
            p.line((18.0, 6.0), (6.0, 18.0));
        }
        Icon::Back => p.poly(&[(14.5, 5.5), (8.0, 12.0), (14.5, 18.5)]),
        Icon::Forward => p.poly(&[(9.5, 5.5), (16.0, 12.0), (9.5, 18.5)]),
        Icon::Home => {
            p.poly(&[(4.5, 11.0), (12.0, 4.5), (19.5, 11.0)]);
            p.poly(&[(6.5, 9.5), (6.5, 19.0), (17.5, 19.0), (17.5, 9.5)]);
            p.line((10.5, 19.0), (10.5, 14.5));
            p.line((10.5, 14.5), (13.5, 14.5));
            p.line((13.5, 14.5), (13.5, 19.0));
        }
        Icon::Up => p.poly(&[(5.5, 14.5), (12.0, 8.0), (18.5, 14.5)]),
        Icon::Radio => {
            p.dot(12.0, 12.0, 2.2);
            p.arc(12.0, 12.0, 5.5, -FRAC_PI_4, FRAC_PI_4);
            p.arc(12.0, 12.0, 5.5, PI - FRAC_PI_4, PI + FRAC_PI_4);
            p.arc(12.0, 12.0, 9.0, -FRAC_PI_4, FRAC_PI_4);
            p.arc(12.0, 12.0, 9.0, PI - FRAC_PI_4, PI + FRAC_PI_4);
        }
        Icon::Autoplay => {
            // Two loops meeting in the middle, drawn as arcs so the crossing stays open.
            p.arc(7.5, 12.0, 4.2, 0.25 * PI, 1.75 * PI);
            p.arc(16.5, 12.0, 4.2, 1.25 * PI, 2.75 * PI);
            let s = std::f32::consts::FRAC_1_SQRT_2 * 4.2;
            p.line((7.5 + s, 12.0 - s), (16.5 - s, 12.0 + s));
            p.line((7.5 + s, 12.0 + s), (16.5 - s, 12.0 - s));
        }
        Icon::Minimize => p.line((6.0, 12.0), (18.0, 12.0)),
        Icon::Maximize => {
            let w = p.w;
            p.c.outline(
                Rect::new(r.x + 6.0 * k, r.y + 6.0 * k, 12.0 * k, 12.0 * k),
                color,
                1.0 * k,
                w,
            );
        }
        Icon::Restore => {
            let w = p.w;
            p.c.outline(
                Rect::new(r.x + 5.5 * k, r.y + 8.5 * k, 10.0 * k, 10.0 * k),
                color,
                1.0 * k,
                w,
            );
            p.poly(&[
                (8.5, 8.5),
                (8.5, 5.5),
                (18.5, 5.5),
                (18.5, 15.5),
                (15.5, 15.5),
            ]);
        }
        Icon::HeartFilled => {
            p.dot(8.4, 9.6, 4.6);
            p.dot(15.6, 9.6, 4.6);
            // The point: a fan of thick strokes from the tip up to the lobes.
            p.w = 2.6 * k;
            for i in 0..=10 {
                let x = 4.6 + i as f32 * 1.48;
                p.line((x, 11.0), (12.0, 19.2));
            }
        }
        Icon::Check => p.poly(&[(5.0, 12.5), (10.0, 17.5), (19.0, 7.0)]),
        Icon::Mic => {
            let w = p.w;
            p.c.outline(
                Rect::new(r.x + 9.0 * k, r.y + 3.0 * k, 6.0 * k, 11.0 * k),
                color,
                3.0 * k,
                w,
            );
            p.arc(12.0, 10.5, 6.0, 0.1 * PI, 0.9 * PI);
            p.line((12.0, 16.5), (12.0, 20.5));
            p.line((8.5, 20.5), (15.5, 20.5));
        }
        Icon::Library => {
            p.line((5.0, 4.5), (5.0, 19.5));
            p.line((10.0, 4.5), (10.0, 19.5));
            p.line((14.0, 5.5), (19.0, 19.0));
        }
        Icon::PanelLeft => {
            let w = p.w;
            p.c.outline(
                Rect::new(r.x + 3.5 * k, r.y + 5.0 * k, 17.0 * k, 14.0 * k),
                color,
                2.5 * k,
                w,
            );
            p.line((9.5, 5.0), (9.5, 19.0));
        }
        Icon::Verified => {
            p.dot(12.0, 12.0, 9.5);
            p.color = Color::WHITE;
            p.w *= 1.15;
            p.poly(&[(7.5, 12.3), (10.8, 15.5), (16.5, 9.0)]);
        }
        Icon::More => {
            for x in [5.5, 12.0, 18.5] {
                p.dot(x, 12.0, 1.7);
            }
        }
        Icon::Spotify => {
            p.dot(12.0, 12.0, 10.0);
            p.color = Color::rgba8(16, 16, 20, 255);
            p.w *= 1.1;
            p.arc(12.0, 22.0, 13.0, -0.69 * PI, -0.31 * PI);
            p.w *= 0.9;
            p.arc(12.0, 22.0, 9.5, -0.68 * PI, -0.32 * PI);
            p.arc(12.0, 22.0, 6.2, -0.66 * PI, -0.34 * PI);
        }
    }
}
