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
    /// The music, as motion.
    motion: super::viz_motion::Motion,
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
            // Edge to edge across the page panel; the bars keep their place inside it.
            let view = Rect::new(r.x - PAD, r.y - PAD + 4.0, r.w + 2.0 * PAD, r.h + PAD + 4.0);
            let bars = [r.x - view.x, r.y - view.y, r.w, r.h];
            let frame = self.swarm_frame(ui, a, dt, bars, (mode, phase, depth, walls));
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
        let k = self.viz_tuning;
        let glow = r.w.min(r.h) * (0.25 + 0.12 * self.pulse);
        ui.c.glow(
            Rect::new(cx - glow, cy - glow, glow * 2.0, glow * 2.0),
            t.gradient(0.6)
                .alpha(((0.10 + 0.25 * self.pulse) * k.bar_glow).min(1.0)),
            glow,
            glow * 0.6,
        );
        let gap = k.bar_gap;
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
            if k.peaks > 0.5 && a.peaks[i] > 0.02 {
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

impl Gui {
    /// Moves the storm's music tracking on by one frame and builds what the GPU needs. Call
    /// once per frame, from whichever page shows the storm (the visualizer, or the preview in
    /// Settings). `stage` is (mode, seconds in it, how 3D the camera is, how far the walls
    /// are up).
    pub(crate) fn swarm_frame(
        &mut self,
        ui: &mut Ui,
        a: &AnalysisFrame,
        dt: f32,
        bars: [f32; 4],
        stage: (Mode, f32, f32, f32),
    ) -> SwarmFrame {
        let t = ui.theme.clone();
        let k = self.viz_tuning;
        ui.anim.keep_alive();
        let m = &mut self.viz3d.motion;
        m.update(a, dt, &k);
        let (mode, phase, depth, walls) = stage;
        SwarmFrame {
            size: (0, 0),
            logical: (0.0, 0.0),
            bars,
            time: m.time,
            tau: m.tau,
            kick: m.kick,
            impact: m.impact,
            punch: m.punch,
            spin: m.spin,
            mid_punch: m.whip,
            fullness: m.fullness,
            bassless: m.bassless,
            swirl: m.swirl,
            yaw: m.yaw,
            flares: m.flares(),
            onset: m.onset,
            tuning: k,
            dt,
            mode,
            phase,
            depth,
            walls,
            bands: a.bands,
            // What the bass would drive, driven by the rhythm when there's little bass.
            bass: m.drive,
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
        }
    }
}

/// Follows every frequency band against its own recent range, so a band "flares" when it
/// rises sharply for *its* level: a syllable lights up the vocal bands, a hi-hat the top ones,
/// even when the bass is quiet. Rises across the whole spectrum together are an onset (a hit
/// from any sudden sound), and the average of the relative levels says how busy the music is,
/// whatever its loudness.
pub(crate) struct SpectrumTracker {
    avg: [f32; BANDS],
    high: [f32; BANDS],
    /// Relative level last time (0..1 within the band's own range).
    rel: [f32; BANDS],
    /// Per band, 1 on a sharp rise, fading.
    pub flare: [f32; BANDS],
    flux_avg: f32,
    since_onset: f32,
    seq: u64,
}

impl Default for SpectrumTracker {
    fn default() -> Self {
        Self {
            avg: [0.0; BANDS],
            high: [0.0; BANDS],
            rel: [0.0; BANDS],
            flare: [0.0; BANDS],
            flux_avg: 0.0,
            since_onset: 1.0,
            seq: u64::MAX,
        }
    }
}

impl SpectrumTracker {
    /// One frame on: returns how busy the music is (0..1) and, on an onset, how strong
    /// (0.3..1).
    pub(crate) fn update(&mut self, a: &AnalysisFrame, dt: f32) -> (f32, Option<f32>) {
        let dt = dt.clamp(0.0, 0.1);
        let fade = (-dt * 7.0).exp();
        for f in &mut self.flare {
            *f *= fade;
        }
        self.since_onset += dt;
        let mut onset = None;
        // Only new analysis frames carry news (the GUI can draw faster than they come).
        if a.seq != self.seq {
            self.seq = a.seq;
            let ease = 1.0 - (-dt * 0.8).exp();
            let mut flux = 0.0;
            for i in 0..BANDS {
                let v = a.bands[i].clamp(0.0, 1.0);
                self.avg[i] += (v - self.avg[i]) * ease;
                self.high[i] = v
                    .max(self.high[i] - 0.25 * dt)
                    .max(self.avg[i] + 0.1)
                    .max(0.08);
                let floor = self.avg[i] * 0.7;
                let rel = ((v - floor) / (self.high[i] - floor)).clamp(0.0, 1.0);
                let rise = (rel - self.rel[i]).max(0.0);
                self.rel[i] = rel;
                self.flare[i] = self.flare[i].max((rise * 3.0).min(1.0));
                flux += rise;
            }
            flux /= BANDS as f32;
            if flux > self.flux_avg * 1.8 + 0.015 && self.since_onset > 0.2 {
                onset = Some(((flux - self.flux_avg) * 15.0).clamp(0.3, 1.0));
                self.since_onset = 0.0;
            }
            self.flux_avg += (flux - self.flux_avg) * (1.0 - (-dt * 2.0).exp());
        }
        let activity = self.rel.iter().sum::<f32>() / BANDS as f32;
        (activity, onset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(seq: u64, f: impl Fn(usize) -> f32) -> AnalysisFrame {
        let mut a = AnalysisFrame {
            seq,
            ..Default::default()
        };
        for i in 0..BANDS {
            a.bands[i] = f(i);
        }
        a
    }

    #[test]
    fn vocals_flare_their_own_bands_without_any_bass() {
        let mut s = SpectrumTracker::default();
        let mut seq = 0;
        // A steady loud bass and nothing else: settles, no more onsets.
        let bass = |i: usize| if i < 12 { 0.9 } else { 0.0 };
        let mut late_onsets = 0;
        for k in 0..400 {
            seq += 1;
            let (_, onset) = s.update(&frame(seq, bass), 1.0 / 100.0);
            if k > 200 && onset.is_some() {
                late_onsets += 1;
            }
        }
        assert_eq!(late_onsets, 0, "steady bass keeps hitting");
        // Quiet vocals come in (mid bands, much quieter than the bass): they flare, the high
        // bands and the bass don't, and it counts as a hit.
        let vocals = |i: usize| match i {
            0..12 => 0.9,
            24..40 => 0.35,
            _ => 0.0,
        };
        let mut hit = false;
        for _ in 0..3 {
            seq += 1;
            hit |= s.update(&frame(seq, vocals), 1.0 / 100.0).1.is_some();
        }
        assert!(hit, "the vocals coming in is an onset");
        assert!(s.flare[30] > 0.5, "vocal band flare {}", s.flare[30]);
        assert!(s.flare[5] < 0.1 && s.flare[55] < 0.1);
        // Flares fade once the vocal holds steady.
        for _ in 0..50 {
            seq += 1;
            s.update(&frame(seq, vocals), 1.0 / 100.0);
        }
        assert!(s.flare[30] < 0.1);
    }

    #[test]
    fn repeated_frames_change_nothing() {
        let mut s = SpectrumTracker::default();
        let a = frame(1, |i| if i == 30 { 0.6 } else { 0.0 });
        s.update(&a, 0.01);
        let before = s.flare[30];
        // Same analysis frame again (the GUI drew twice): no new rise.
        s.update(&a, 0.0);
        assert_eq!(s.flare[30], before);
    }
}
