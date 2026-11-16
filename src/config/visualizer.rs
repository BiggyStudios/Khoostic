//! Visualizer tuning (`[visualizer]` in config.toml): every knob of the 2D bars and the 3D
//! wave, the user's saved presets, and the built-in ones.
//!
//! Each knob is declared once (key, default, range, label, group) and the rest is generated:
//! the struct, its defaults, the list Settings draws, and the order the GPU receives them in
//! (`shaders/swarm.wgsl` reads them by the same index; a test checks the two agree).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct VisualizerConfig {
    /// The current tuning (only what differs from the defaults is written).
    #[serde(skip_serializing_if = "VisualizerTuning::is_default")]
    pub tuning: VisualizerTuning,
    /// Saved presets by name.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub presets: BTreeMap<String, VisualizerTuning>,
}

/// One knob, for Settings.
#[derive(Debug, Clone, Copy)]
pub struct Knob {
    pub key: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    pub group: Group,
    pub default: f32,
    pub min: f32,
    pub max: f32,
    /// Shown as Off / On instead of a slider.
    pub toggle: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Wave,
    Motion,
    Impact,
    Camera,
    Look,
    Bars,
}

impl Group {
    pub const ALL: [Group; 6] = [
        Group::Wave,
        Group::Motion,
        Group::Impact,
        Group::Camera,
        Group::Look,
        Group::Bars,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Group::Wave => "Wave",
            Group::Motion => "Motion",
            Group::Impact => "Impact",
            Group::Camera => "Camera",
            Group::Look => "Look",
            Group::Bars => "2D bars",
        }
    }
}

macro_rules! knobs {
    ($( $field:ident: $group:ident, $default:expr, $min:expr, $max:expr, $toggle:expr,
        $label:literal, $hint:literal; )*) => {
        /// Every knob, in the order the GPU gets them.
        #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
        #[serde(default, deny_unknown_fields)]
        pub struct VisualizerTuning {
            $( pub $field: f32, )*
        }

        impl Default for VisualizerTuning {
            fn default() -> Self {
                Self { $( $field: $default, )* }
            }
        }

        pub const KNOBS: &[Knob] = &[
            $( Knob {
                key: stringify!($field),
                label: $label,
                hint: $hint,
                group: Group::$group,
                default: $default,
                min: $min,
                max: $max,
                toggle: $toggle,
            }, )*
        ];

        impl VisualizerTuning {
            pub fn get(&self, i: usize) -> f32 {
                [$( self.$field, )*][i]
            }

            pub fn set(&mut self, i: usize, v: f32) {
                let all = [$( &mut self.$field, )*];
                *all[i] = v;
            }
        }
    };
}

knobs! {
    // The wave
    cubes: Wave, 1.0, 0.1, 1.0, false, "Cubes", "How much of the storm is drawn";
    splashes: Wave, 1.0, 0.0, 1.0, false, "Splashes", "How many of the big splash cubes fly";
    cube_size: Wave, 1.0, 0.3, 3.0, false, "Cube size", "Size of the sand";
    splash_size: Wave, 1.0, 0.3, 3.0, false, "Splash size", "Size of the splash cubes";
    length: Wave, 3.4, 0.5, 8.0, false, "Length", "How long the wave trails behind";
    width: Wave, 1.0, 0.2, 3.0, false, "Width", "How wide the ribbon is";
    thickness: Wave, 1.0, 0.2, 4.0, false, "Thickness", "How thick the ribbon is";
    splash_spread: Wave, 1.4, 1.0, 4.0, false, "Splash spread", "How far splashes stray from the wave";
    reach: Wave, 1.0, 0.3, 2.0, false, "Reach", "How far the wave roams";
    twist: Wave, 1.0, 0.0, 4.0, false, "Twist", "How much the ribbon twists along its length";
    // How it moves
    speed: Motion, 0.45, 0.0, 2.0, false, "Speed", "How fast the wave swings on its own";
    bass_speed: Motion, 0.8, 0.0, 3.0, false, "Speed from bass", "Extra swing when the bass is loud";
    mid_speed: Motion, 1.3, 0.0, 3.0, false, "Speed from mids", "Extra swing when the mids are loud";
    treble_speed: Motion, 0.5, 0.0, 3.0, false, "Speed from treble", "Extra swing when the treble is loud";
    spin: Motion, 0.25, 0.0, 2.0, false, "Spin", "How fast the ribbon turns on its own";
    mid_spin: Motion, 2.6, 0.0, 8.0, false, "Spin from mids", "Extra turning when the mids are loud";
    hold: Motion, 10.0, 2.0, 30.0, false, "Hold", "How tightly cubes stick to the wave";
    drift: Motion, 1.0, 0.0, 4.0, false, "Drift", "Sand blowing about inside the wave";
    // Hits
    burst: Impact, 1.0, 0.0, 3.0, false, "Beat burst", "How hard a beat throws the cubes out";
    lunge: Impact, 3.5, 0.0, 10.0, false, "Beat lunge", "How far the wave surges on a beat";
    slack: Impact, 0.6, 0.0, 0.95, false, "Slack", "How loose the wave goes right after a beat";
    recover: Impact, 6.0, 1.0, 20.0, false, "Recovery", "How quickly a hit fades";
    bass_punch: Impact, 1.0, 0.0, 3.0, false, "Bass punch", "Sudden bass blowing the wave outward";
    mid_whip: Impact, 1.0, 0.0, 3.0, false, "Mid whip", "Sudden mids whipping cubes around";
    puff: Impact, 1.0, 0.0, 3.0, false, "Puff", "How much the wave swells with bass and beats";
    flash: Impact, 1.0, 0.0, 3.0, false, "Flash", "How bright a beat flashes";
    // Camera
    distance: Camera, 4.3, 2.5, 8.0, false, "Distance", "How far back the camera sits";
    fov: Camera, 50.0, 25.0, 90.0, false, "Field of view", "Wider sees more, narrower zooms in";
    sway: Camera, 0.2, 0.0, 1.0, false, "Sway", "How much the camera drifts side to side";
    camera_punch: Camera, 0.45, 0.0, 1.5, false, "Punch", "How far the camera jumps in on a beat";
    shake: Camera, 0.05, 0.0, 0.3, false, "Shake", "Camera shake on a beat";
    bass_zoom: Camera, 0.25, 0.0, 1.0, false, "Bass zoom", "How far the camera leans in with the bass";
    // Look
    metal: Look, 1.6, 0.0, 3.0, false, "Shine", "How much the cubes reflect, like metal";
    band_glow: Look, 1.0, 0.0, 3.0, false, "Band glow", "Cubes glowing with their own frequency";
    fog: Look, 0.45, 0.0, 1.5, false, "Fog", "How quickly the distance fades out";
    backdrop_glow: Look, 1.0, 0.0, 3.0, false, "Backdrop glow", "The glow behind the wave";
    walls: Look, 1.0, 0.0, 1.0, true, "Walls", "The cube walls at the sides";
    wall_bulge: Look, 0.35, 0.0, 1.0, false, "Wall bulge", "How far the walls pulse with the music";
    // The flat bars (also how the 3D starts and ends)
    bar_gap: Bars, 4.0, 0.0, 12.0, false, "Gap", "Space between the bars";
    peaks: Bars, 1.0, 0.0, 1.0, true, "Peaks", "Falling markers above the bars";
    bar_glow: Bars, 1.0, 0.0, 3.0, false, "Glow", "The glow behind the bars";
    // Reacting to the whole spectrum (vocals, hats, synths), not just the bass. Last so the
    // GPU slots above don't move.
    activity_speed: Motion, 1.0, 0.0, 3.0, false, "Speed from everything", "Keeps the wave moving whenever anything is playing, even quiet parts";
    spectrum_push: Impact, 1.0, 0.0, 3.0, false, "Spectrum push", "Each frequency pushing its own cubes: vocals, hats, synths";
    onset_kick: Impact, 1.0, 0.0, 3.0, false, "Onset hits", "Hits from any sudden sound, not only the bass";
}

impl VisualizerTuning {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Every knob, in the order the GPU reads them (padded to whole vec4s).
    pub fn to_gpu(&self) -> [[f32; 4]; 11] {
        let mut out = [[0.0; 4]; 11];
        for i in 0..KNOBS.len() {
            out[i / 4][i % 4] = self.get(i);
        }
        out
    }

    /// Values kept in range and rounded (so the config stays readable).
    pub fn clean(mut self) -> Self {
        for (i, k) in KNOBS.iter().enumerate() {
            let v = self.get(i).clamp(k.min, k.max);
            self.set(
                i,
                if k.toggle {
                    v.round()
                } else {
                    (v * 1000.0).round() / 1000.0
                },
            );
        }
        self
    }
}

/// Presets that come with Khoostic (they can't be overwritten or deleted).
pub fn builtin_presets() -> Vec<(&'static str, VisualizerTuning)> {
    let d = VisualizerTuning::default();
    vec![
        ("Default", d),
        (
            "Calm",
            VisualizerTuning {
                speed: 0.25,
                bass_speed: 0.4,
                mid_speed: 0.6,
                treble_speed: 0.25,
                spin: 0.15,
                mid_spin: 1.2,
                drift: 0.6,
                burst: 0.4,
                lunge: 1.0,
                bass_punch: 0.5,
                mid_whip: 0.5,
                puff: 0.6,
                flash: 0.5,
                camera_punch: 0.15,
                shake: 0.01,
                bass_zoom: 0.1,
                spectrum_push: 0.6,
                onset_kick: 0.5,
                ..d
            },
        ),
        (
            "Wild",
            VisualizerTuning {
                speed: 0.7,
                bass_speed: 1.4,
                mid_speed: 2.0,
                treble_speed: 0.9,
                mid_spin: 4.5,
                twist: 2.0,
                drift: 1.8,
                burst: 1.8,
                lunge: 6.0,
                slack: 0.8,
                bass_punch: 1.8,
                mid_whip: 1.8,
                puff: 1.6,
                flash: 1.8,
                camera_punch: 0.8,
                shake: 0.12,
                spectrum_push: 1.6,
                onset_kick: 1.6,
                activity_speed: 1.5,
                ..d
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knobs_fit_and_defaults_are_in_range() {
        assert!(KNOBS.len() <= 44, "the GPU gets 11 vec4s");
        let d = VisualizerTuning::default();
        for (i, k) in KNOBS.iter().enumerate() {
            assert_eq!(d.get(i), k.default, "{}", k.key);
            assert!(k.min <= k.default && k.default <= k.max, "{}", k.key);
        }
        assert_eq!(d.clean(), d);
        for (name, p) in builtin_presets() {
            assert_eq!(p.clean(), p, "{name}");
        }
    }

    #[test]
    fn shader_reads_the_knobs_in_this_order() {
        // `const T_<KEY>: u32 = <index>u;` for every knob the shader uses.
        let wgsl = include_str!("../gui/render/shaders/swarm.wgsl");
        let mut seen = 0;
        for line in wgsl.lines().map(str::trim) {
            let Some(rest) = line.strip_prefix("const T_") else {
                continue;
            };
            let (name, value) = rest.split_once(": u32 = ").expect("const T_X: u32 = Nu;");
            let index: usize = value.trim_end_matches("u;").parse().expect("index");
            assert_eq!(
                KNOBS[index].key.to_uppercase(),
                name,
                "swarm.wgsl's T_{name} is knob {index}"
            );
            seen += 1;
        }
        assert!(seen > 20, "only {seen} knobs found in the shader");
    }

    #[test]
    fn get_and_set_line_up() {
        let mut t = VisualizerTuning::default();
        t.set(KNOBS.iter().position(|k| k.key == "fov").unwrap(), 70.0);
        assert_eq!(t.fov, 70.0);
    }
}
