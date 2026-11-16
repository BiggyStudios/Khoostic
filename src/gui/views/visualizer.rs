//! The full-page visualizer: spectrum bars, or the 3D storm of cubes they break apart into
//! (switch at the top right). Going 3D, the bars are rebuilt out of cubes in place, crumble
//! from the top and get swept up while the camera swings into the room; going back, the cubes
//! fly home into bars and the flat bars take over again.

use std::time::Instant;

use crate::audio::{AnalysisFrame, BANDS};
use crate::gui::app::{Gui, PAD};
use crate::gui::render::Rect;
use crate::gui::render::swarm::{Mode, SwarmFrame};
use crate::gui::ui::{Ui, anim};

/// How long the bars take to fall back together (the flat bars fade in after).
const ASSEMBLE: f32 = 1.9;
const FADE: f32 = 0.25;

#[derive(Default)]
pub(crate) struct Viz3d {
    stage: Stage,
    /// Seconds the storm has run; drives its flow and the camera drift.
    time: f32,
    /// Music time: speeds up with the bass and mids, so the wave swings harder to loud music.
    tau: f32,
    /// Beats seen so far (a new one is a hit), the hit envelope, and a slow bass average to
    /// tell sudden bass from loud bass.
    beats: u64,
    impact: f32,
    slow_bass: f32,
    /// The ribbon's spin, a slow mids average (for mid punches) and how full the music is.
    spin: f32,
    slow_mid: f32,
    fullness: f32,
}

#[derive(Default, Clone, Copy)]
enum Stage {
    #[default]
    Flat,
    /// Going (or gone) 3D since then.
    Rising(Instant),
    /// Going back to the flat bars since then.
    Falling(Instant),
}

impl Gui {
    pub(crate) fn visualizer(&mut self, ui: &mut Ui, r: Rect, a: &AnalysisFrame, dt: f32) {
        let t = ui.theme.clone();
        // What the 3D side should show now, and how opaque it is over the flat bars.
        let scene = match self.viz3d.stage {
            Stage::Flat => None,
            Stage::Rising(at) => {
                let s = at.elapsed().as_secs_f32();
                Some((
                    Mode::Storm,
                    s,
                    ((s - 0.35) / 1.9).clamp(0.0, 1.0),
                    ((s - 1.3) / 1.2).clamp(0.0, 1.0),
                    (s / 0.15).clamp(0.0, 1.0),
                ))
            }
            Stage::Falling(at) => {
                let s = at.elapsed().as_secs_f32();
                if s > ASSEMBLE + FADE {
                    self.viz3d.stage = Stage::Flat;
                    None
                } else {
                    Some((
                        Mode::Assemble,
                        s,
                        1.0 - (s / 1.5).clamp(0.0, 1.0),
                        1.0 - (s / 0.6).clamp(0.0, 1.0),
                        1.0 - ((s - ASSEMBLE) / FADE).clamp(0.0, 1.0),
                    ))
                }
            }
        };

        // The flat bars, under the 3D picture while it fades in or out.
        if scene.is_none_or(|s| s.4 < 1.0) {
            self.bars(ui, r, a, dt);
        }
        if let Some((mode, phase, depth, walls, opacity)) = scene {
            self.viz3d.time += dt;
            let v = &mut self.viz3d;
            let kick = a.beat_count != v.beats;
            v.beats = a.beat_count;
            v.impact = if kick {
                1.0
            } else {
                v.impact * (-dt * 6.0).exp()
            };
            v.slow_bass += (a.bass - v.slow_bass) * (1.0 - (-dt * 2.5).exp());
            let punch = ((a.bass - v.slow_bass) * 3.0).clamp(0.0, 1.0);
            v.slow_mid += (a.mid - v.slow_mid) * (1.0 - (-dt * 3.0).exp());
            let mid_punch = ((a.mid - v.slow_mid) * 4.0).clamp(0.0, 1.0);
            let level = (a.bass + a.mid + a.treble) / 3.0;
            v.fullness += (level - v.fullness) * (1.0 - (-dt * 1.5).exp());
            // Spins with the mids (and a little treble), always a little.
            v.spin += dt * (0.25 + 2.6 * a.mid + 1.2 * a.treble + 4.0 * mid_punch);
            // Swings faster with the whole mix and lunges on every hit.
            v.tau += dt
                * (0.45 + 0.8 * a.bass + 1.3 * a.mid + 0.5 * a.treble + 3.5 * v.impact * v.impact);
            let impact = v.impact;
            ui.anim.keep_alive();
            // Edge to edge across the page panel; the bars keep their place inside it.
            let view = Rect::new(r.x - PAD, r.y - PAD + 4.0, r.w + 2.0 * PAD, r.h + PAD + 4.0);
            let frame = SwarmFrame {
                size: (0, 0),
                logical: (0.0, 0.0),
                bars: [r.x - view.x, r.y - view.y, r.w, r.h],
                time: self.viz3d.time,
                tau: self.viz3d.tau,
                kick,
                impact,
                punch,
                spin: self.viz3d.spin,
                mid_punch,
                fullness: self.viz3d.fullness,
                dt,
                mode,
                phase,
                depth,
                walls,
                bands: a.bands,
                bass: a.bass,
                mid: a.mid,
                treble: a.treble,
                beat: a.beat,
                spectrum: [
                    t.gradient(0.0),
                    t.gradient(1.0 / 3.0),
                    t.gradient(2.0 / 3.0),
                    t.gradient(1.0),
                ],
                accent: t.accent,
                surface: t.surface,
                fg: t.fg,
            };
            ui.c.swarm(view, frame, 12.0, opacity);
        }

        // 2D / 3D, top right, above the picture.
        let three = matches!(self.viz3d.stage, Stage::Rising(_));
        let size = t.font_size - 1.0;
        let seg = ui
            .measure("2D", size, crate::gui::render::Weight::Medium)
            .max(ui.measure("3D", size, crate::gui::render::Weight::Medium))
            + 30.0;
        let w = 2.0 * seg + 6.0;
        ui.c.set_layer(1);
        // A solid backing so it reads over bright cubes.
        ui.c.rect(
            Rect::new(r.right() - w, r.y, w, 34.0),
            t.surface.alpha(0.85),
            17.0,
        );
        let picked = ui.segmented(
            anim::id("viz-3d"),
            r.right() - w,
            r.y,
            &["2D", "3D"],
            three as usize,
        );
        ui.c.set_layer(0);
        match picked {
            Some(1) if !three => self.viz3d.stage = Stage::Rising(Instant::now()),
            Some(0) if three => self.viz3d.stage = Stage::Falling(Instant::now()),
            _ => {}
        }
    }

    fn bars(&mut self, ui: &mut Ui, r: Rect, a: &AnalysisFrame, dt: f32) {
        let t = ui.theme.clone();
        self.pulse = (self.pulse - dt * 2.5).max(a.beat);
        let (cx, cy) = r.center();
        let glow = r.w.min(r.h) * (0.25 + 0.12 * self.pulse);
        ui.c.glow(
            Rect::new(cx - glow, cy - glow, glow * 2.0, glow * 2.0),
            t.gradient(0.6).alpha(0.10 + 0.25 * self.pulse),
            glow,
            glow * 0.6,
        );
        let gap = 4.0;
        let bar_w = ((r.w - gap * (BANDS as f32 - 1.0)) / BANDS as f32).max(1.0);
        let base = r.bottom() - 24.0;
        let max_h = r.h - 48.0;
        for i in 0..BANDS {
            let v = a.bands[i];
            let x = r.x + i as f32 * (bar_w + gap);
            let h = (v * max_h).max(bar_w);
            ui.c.gradient(
                Rect::new(x, base - h, bar_w, h),
                t.gradient(v),
                t.gradient(0.0).alpha(0.5),
                bar_w / 2.0,
            );
            if a.peaks[i] > 0.02 {
                let peak = base - a.peaks[i] * max_h - 6.0;
                ui.c.rect(
                    Rect::new(x, peak, bar_w, 3.0),
                    t.gradient(v).alpha(0.8),
                    1.5,
                );
            }
        }
    }
}
