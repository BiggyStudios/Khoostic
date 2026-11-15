//! The full-page visualizer.

use crate::audio::{AnalysisFrame, BANDS};
use crate::gui::app::Gui;
use crate::gui::render::Rect;
use crate::gui::ui::Ui;

impl Gui {
    pub(crate) fn visualizer(&mut self, ui: &mut Ui, r: Rect, a: &AnalysisFrame, dt: f32) {
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
