//! Frame-rate independent animation state for the immediate-mode UI.
//!
//! Widgets ask for a value by id with a target every frame ("hovered: 1, otherwise 0") and get
//! back where it currently is on its way there. Values that are still moving keep the window
//! rendering; values nobody asked for in a while are dropped.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// A stable id for an animated value.
pub fn id(parts: impl Hash) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    parts.hash(&mut h);
    h.finish()
}

struct Value {
    v: f32,
    last_frame: u64,
}

#[derive(Default)]
pub struct Anims {
    values: HashMap<u64, Value>,
    frame: u64,
    dt: f32,
    moving: bool,
}

/// Default rates, in "1/e per second" units: higher is snappier.
pub const FAST: f32 = 22.0;
pub const NORMAL: f32 = 14.0;
pub const SLOW: f32 = 8.0;

impl Anims {
    /// Starts a frame `dt` seconds after the previous one.
    pub fn begin(&mut self, dt: f32) {
        self.frame += 1;
        self.dt = dt.clamp(0.0, 0.1);
        self.moving = false;
        if self.frame.is_multiple_of(120) {
            let frame = self.frame;
            self.values.retain(|_, v| frame - v.last_frame < 240);
        }
    }

    /// Whether any value moved this frame (the window should keep rendering).
    pub fn moving(&self) -> bool {
        self.moving
    }

    pub fn dt(&self) -> f32 {
        self.dt
    }

    /// Moves the value `id` toward `target` with exponential easing at `rate`. A value seen for
    /// the first time starts at `target` (no animation on first appearance) unless `from` is set.
    pub fn to_from(&mut self, id: u64, target: f32, rate: f32, from: Option<f32>) -> f32 {
        let frame = self.frame;
        let e = self.values.entry(id).or_insert(Value {
            v: from.unwrap_or(target),
            last_frame: frame,
        });
        e.last_frame = frame;
        let diff = target - e.v;
        if diff.abs() < 0.0005 * target.abs().max(1.0) {
            e.v = target;
        } else {
            e.v += diff * (1.0 - (-rate * self.dt).exp());
            self.moving = true;
        }
        e.v
    }

    pub fn to(&mut self, id: u64, target: f32, rate: f32) -> f32 {
        self.to_from(id, target, rate, None)
    }

    /// Sets a value immediately (e.g. to restart an animation).
    pub fn set(&mut self, id: u64, v: f32) {
        let frame = self.frame;
        self.values.insert(
            id,
            Value {
                v,
                last_frame: frame,
            },
        );
    }

    /// Marks the frame as animating without a stored value (time-based effects).
    pub fn keep_alive(&mut self) {
        self.moving = true;
    }
}

/// Smoothstep-shaped ease for 0..1 progress.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converges_independent_of_frame_rate() {
        let run = |fps: f32| {
            let mut a = Anims::default();
            let mut v = 0.0;
            a.begin(0.0);
            a.to(1, 0.0, NORMAL);
            let frames = (0.25 * fps) as usize;
            for _ in 0..frames {
                a.begin(1.0 / fps);
                v = a.to(1, 1.0, NORMAL);
            }
            v
        };
        let (v30, v120) = (run(30.0), run(120.0));
        assert!((v30 - v120).abs() < 0.01, "{v30} vs {v120}");
        assert!(v120 > 0.95);
    }

    #[test]
    fn settles_and_stops_moving() {
        let mut a = Anims::default();
        a.begin(0.016);
        assert_eq!(a.to(7, 1.0, FAST), 1.0, "first sight starts at the target");
        assert!(!a.moving());
        for _ in 0..200 {
            a.begin(0.016);
            a.to(7, 0.0, FAST);
        }
        a.begin(0.016);
        assert_eq!(a.to(7, 0.0, FAST), 0.0);
        assert!(!a.moving());
    }
}
