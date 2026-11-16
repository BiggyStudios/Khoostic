//! The built-in themes. `[gui.theme] name` picks one; colors set next to it override it.

pub struct Preset {
    /// What config.toml calls it.
    pub key: &'static str,
    /// What Settings calls it.
    pub name: &'static str,
    pub background: &'static str,
    pub surface: &'static str,
    pub surface_hover: &'static str,
    pub fg: &'static str,
    pub dim: &'static str,
    pub accent: &'static str,
    pub playing: &'static str,
    /// Visualizer colors, low to high.
    pub spectrum: &'static [&'static str],
    /// How colorful art-derived washes (artist headers, cover placeholders) may be: 0 is
    /// grayscale.
    pub saturation: f32,
    /// How far those washes lean toward the accent.
    pub pull: f32,
}

pub const PRESETS: &[Preset] = &[
    // Monochrome: white on near-black, color only from the music's art.
    Preset {
        key: "khoostic",
        name: "Khoostic",
        background: "#09090b",
        surface: "#131316",
        surface_hover: "#1f1f23",
        fg: "#f2f2f4",
        dim: "#8c8c94",
        accent: "#f4f4f5",
        playing: "#ffffff",
        spectrum: &["#4b4b53", "#8f8f97", "#d2d2d8", "#ffffff"],
        saturation: 0.0,
        pull: 0.0,
    },
    Preset {
        key: "pink",
        name: "Pink",
        background: "#0d090c",
        surface: "#181016",
        surface_hover: "#261923",
        fg: "#fbeef5",
        dim: "#a98f9d",
        accent: "#ff8cc6",
        playing: "#ffa8d6",
        spectrum: &["#ffc2d4", "#ff8cc6", "#f066c0", "#c77dff"],
        saturation: 0.7,
        pull: 0.45,
    },
    // Catppuccin Mocha (catppuccin.com/palette): crust, base, surface0, text, overlay2,
    // mauve, green; sky, blue, mauve and pink for the visualizer.
    Preset {
        key: "catppuccin",
        name: "Catppuccin",
        background: "#11111b",
        surface: "#1e1e2e",
        surface_hover: "#313244",
        fg: "#cdd6f4",
        dim: "#9399b2",
        accent: "#cba6f7",
        playing: "#a6e3a1",
        spectrum: &["#89dceb", "#89b4fa", "#cba6f7", "#f5c2e7"],
        saturation: 1.0,
        pull: 0.15,
    },
];

/// The preset called `key` (case and spacing ignored).
pub fn preset(key: &str) -> Option<&'static Preset> {
    let key: String = key
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    PRESETS.iter().find(|p| p.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::render::Color;

    #[test]
    fn presets_parse_and_resolve() {
        for p in PRESETS {
            for c in [
                p.background,
                p.surface,
                p.surface_hover,
                p.fg,
                p.dim,
                p.accent,
                p.playing,
            ]
            .iter()
            .chain(p.spectrum)
            {
                assert!(Color::hex(c).is_some(), "{}: {c}", p.key);
            }
            assert_eq!(preset(p.name).map(|q| q.key), Some(p.key));
        }
        assert_eq!(PRESETS[0].key, "khoostic");
        assert!(preset("Catppuccin Mocha").is_none());
        assert!(preset("CATPPUCCIN").is_some());
    }
}
