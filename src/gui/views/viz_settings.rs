//! Settings → Visualizer: a live preview of the 3D wave, presets (built-in and saved), and a
//! slider for every knob, grouped. Changes show at once and are saved to `[visualizer]` in
//! config.toml when a drag ends. Double-click a slider to put it back to its default.

use crate::audio::AnalysisFrame;
use crate::config::visualizer::{Group, KNOBS, builtin_presets};
use crate::config::{Config, VisualizerConfig, VisualizerTuning};
use crate::gui::app::Gui;
use crate::gui::render::swarm::Mode;
use crate::gui::render::{Rect, Weight};
use crate::gui::ui::{Key, Ui, anim};

const PREVIEW_H: f32 = 340.0;
const ROW_H: f32 = 52.0;

impl Gui {
    /// Draws the section from `*y` down (`body` is the page; `control_x` where controls
    /// start) and moves `*y` past it.
    pub(crate) fn visualizer_settings(
        &mut self,
        ui: &mut Ui,
        body: Rect,
        control_x: f32,
        y: &mut f32,
        a: &AnalysisFrame,
    ) {
        let t = ui.theme.clone();
        let lx = body.x;

        // A live preview of the 3D wave (only drawn while on screen).
        let preview = Rect::new(lx, *y, body.w, PREVIEW_H);
        if preview.bottom() > body.y && preview.y < body.bottom() {
            let dt = ui.anim.dt();
            let frame = self.swarm_frame(
                ui,
                a,
                dt,
                [0.0, 0.0, preview.w, preview.h],
                (Mode::Storm, 99.0, 1.0, 1.0),
            );
            ui.c.swarm(preview, frame, 12.0, 1.0);
            ui.c.set_layer(1);
            ui.label(
                "Live preview · play something to see it move",
                lx + 14.0,
                Rect::new(lx + 14.0, *y + 10.0, body.w - 28.0, 20.0),
                body.w - 28.0,
                12.0,
                Weight::Medium,
                t.fg.alpha(0.7),
            );
            ui.c.set_layer(0);
        } else {
            ui.c.rect(preview, t.wash(0.04), 12.0);
        }
        *y += PREVIEW_H + 20.0;

        // Presets: the built-in ones, then saved ones; the one matching now is lit.
        let mut presets: Vec<(String, VisualizerTuning, bool)> = builtin_presets()
            .into_iter()
            .map(|(n, p)| (n.to_owned(), p, true))
            .collect();
        presets.extend(self.viz_presets.iter().map(|(n, p)| (n.clone(), *p, false)));
        let current = self.viz_tuning;
        let mut x = lx;
        let mut apply = None;
        let mut active_user = None;
        for (name, p, builtin) in &presets {
            let on = *p == current;
            if on && !builtin {
                active_user = Some(name.clone());
            }
            let w = ui.measure(name, t.font_size - 1.0, Weight::Medium) + 32.0;
            if x + w > body.right() && x > lx {
                x = lx;
                *y += 42.0;
            }
            let (clicked, w) = ui.pill(name, x, *y, on);
            if clicked {
                apply = Some(*p);
            }
            x += w + 8.0;
        }
        *y += 50.0;
        if let Some(p) = apply {
            self.viz_tuning = p;
            self.save_visualizer();
        }

        // Save the current knobs under a name, delete the lit saved preset, or start over.
        let field_w = (body.w * 0.4).clamp(160.0, 260.0);
        ui.text_input(
            &mut self.preset_name,
            Rect::new(lx, *y, field_w, 36.0),
            "Preset name",
            None,
        );
        let enter = self.preset_name.focused && ui.input.keys.iter().any(|k| k.key == Key::Enter);
        let name = self.preset_name.text.trim().to_owned();
        let mut bx = lx + field_w + 10.0;
        let (save, w) = ui.pill("Save preset", bx, *y + 1.0, !name.is_empty());
        bx += w + 8.0;
        if (save || enter) && !name.is_empty() {
            self.save_preset(&name);
        }
        if let Some(user) = &active_user {
            let (del, w) = ui.pill("Delete preset", bx, *y + 1.0, false);
            bx += w + 8.0;
            if del {
                self.viz_presets.remove(user);
                self.save_visualizer();
                self.toast(format!("Deleted “{user}”"));
            }
        }
        if ui.pill("Reset to default", bx, *y + 1.0, false).0 {
            self.viz_tuning = VisualizerTuning::default();
            self.save_visualizer();
            self.toast("Visualizer back to its defaults");
        }
        *y += 56.0;

        // Knob groups as tabs.
        let mut x = lx;
        for g in Group::ALL {
            let label = g.label();
            let w = ui.measure(label, t.font_size - 1.0, Weight::Medium) + 32.0;
            if x + w > body.right() && x > lx {
                x = lx;
                *y += 42.0;
            }
            let (clicked, w) = ui.pill(label, x, *y, self.viz_group == g);
            if clicked {
                self.viz_group = g;
            }
            x += w + 8.0;
        }
        *y += 54.0;

        // The knobs of the open group.
        let slider_w = (body.right() - control_x - 64.0).clamp(120.0, 300.0);
        for (i, k) in KNOBS.iter().enumerate() {
            if k.group != self.viz_group {
                continue;
            }
            ui.label(
                k.label,
                lx,
                Rect::new(lx, *y + 2.0, control_x - lx, 20.0),
                control_x - lx - 24.0,
                t.font_size,
                Weight::Medium,
                t.fg,
            );
            ui.label(
                k.hint,
                lx,
                Rect::new(lx, *y + 22.0, control_x - lx, 18.0),
                control_x - lx - 24.0,
                12.0,
                Weight::Regular,
                t.dim,
            );
            if k.toggle {
                let on = (self.viz_tuning.get(i) > 0.5) as usize;
                if let Some(pick) = ui.segmented(
                    anim::id(("viz-knob", i)),
                    control_x,
                    *y + 3.0,
                    &["Off", "On"],
                    on,
                ) && pick != on
                {
                    self.viz_tuning.set(i, pick as f32);
                    self.save_visualizer();
                }
            } else {
                self.knob_slider(ui, i, Rect::new(control_x, *y + 4.0, slider_w, 30.0));
            }
            *y += ROW_H;
        }

        // Slider changes are written once the drag ends.
        if !ui.input.down && (self.viz_drag.take().is_some() || self.viz_dirty) {
            self.viz_dirty = false;
            self.save_visualizer();
        }
        *y += 10.0;
    }

    /// One knob's slider, with its value to the right and a tick at its default.
    fn knob_slider(&mut self, ui: &mut Ui, i: usize, r: Rect) {
        let t = ui.theme.clone();
        let k = KNOBS[i];
        let id = anim::id(("viz-slider", i));
        let hot_r = Rect::new(r.x - 8.0, r.y, r.w + 16.0, r.h);
        let hover = ui.hovered(hot_r);
        ui.pointer |= hover;
        if hover && ui.input.double_click {
            self.viz_tuning.set(i, k.default);
            self.viz_drag = None;
            self.viz_dirty = true;
        } else if hover && ui.input.pressed {
            self.viz_drag = Some(i);
        }
        let dragging = self.viz_drag == Some(i) && ui.input.down;
        if dragging && let Some((mx, _)) = ui.input.mouse {
            let f = ((mx - r.x) / r.w).clamp(0.0, 1.0);
            let v = k.min + f * (k.max - k.min);
            self.viz_tuning.set(i, (v * 1000.0).round() / 1000.0);
        }
        let v = self.viz_tuning.get(i);
        let frac = ((v - k.min) / (k.max - k.min)).clamp(0.0, 1.0);
        let hot = ui.anim.to(id, (hover || dragging) as u8 as f32, anim::FAST);
        let cy = r.y + r.h / 2.0;
        ui.c.rect(Rect::new(r.x, cy - 2.0, r.w, 4.0), t.wash(0.14), 2.0);
        ui.c.rect(
            Rect::new(r.x, cy - 2.0, r.w * frac, 4.0),
            t.accent.alpha(0.85),
            2.0,
        );
        let dx = r.x + r.w * ((k.default - k.min) / (k.max - k.min));
        ui.c.rect(
            Rect::new(dx - 1.0, cy - 7.0, 2.0, 14.0),
            t.dim.alpha(0.5),
            1.0,
        );
        ui.c.circle(r.x + r.w * frac, cy, 6.0 + 2.0 * hot, t.fg);
        let text = if k.max >= 20.0 {
            format!("{v:.0}")
        } else if k.max >= 5.0 {
            format!("{v:.1}")
        } else {
            format!("{v:.2}")
        };
        ui.label(
            &text,
            r.right() + 14.0,
            Rect::new(r.right() + 14.0, r.y + 5.0, 56.0, 20.0),
            56.0,
            t.font_size - 1.0,
            Weight::Medium,
            if v == k.default { t.dim } else { t.fg },
        );
    }

    fn save_preset(&mut self, name: &str) {
        if builtin_presets()
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case(name))
        {
            return self.toast(format!("“{name}” is a built-in preset; pick another name"));
        }
        self.viz_presets.insert(name.to_owned(), self.viz_tuning);
        self.preset_name.text.clear();
        self.preset_name.focused = false;
        self.save_visualizer();
        self.toast(format!("Saved preset “{name}”"));
    }

    /// Writes the knobs and saved presets to config.toml.
    pub(crate) fn save_visualizer(&mut self) {
        self.viz_tuning = self.viz_tuning.clean();
        let Some(path) = self.config_path.clone() else {
            return;
        };
        let v = VisualizerConfig {
            tuning: self.viz_tuning,
            presets: self.viz_presets.clone(),
        };
        if let Err(e) = Config::save_table(&path, "visualizer", &v) {
            self.toast(format!("Couldn't save: {e:#}"));
        }
    }
}
