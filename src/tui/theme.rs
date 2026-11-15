//! Parsed colors from the config theme.

use std::str::FromStr;

use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone)]
pub struct Theme {
    pub fg: Color,
    pub bg: Color,
    pub dim: Color,
    pub accent: Color,
    pub selection_fg: Color,
    pub selection_bg: Color,
    pub visual_bg: Color,
    pub playing: Color,
    pub error: Color,
    pub spectrum: Vec<Color>,
}

impl Theme {
    /// Parses the config theme; unparseable colors fall back to defaults and are reported.
    pub fn from_config(t: &crate::config::Theme) -> (Self, Vec<String>) {
        let d = crate::config::Theme::default();
        let mut problems = Vec::new();
        let mut parse = |value: &str, fallback: &str| {
            parse_color(value).unwrap_or_else(|| {
                problems.push(format!("theme: can't parse color {value:?}"));
                parse_color(fallback).expect("default colors parse")
            })
        };
        let theme = Self {
            fg: parse(&t.fg, &d.fg),
            bg: parse(&t.bg, &d.bg),
            dim: parse(&t.dim, &d.dim),
            accent: parse(&t.accent, &d.accent),
            selection_fg: parse(&t.selection_fg, &d.selection_fg),
            selection_bg: parse(&t.selection_bg, &d.selection_bg),
            visual_bg: parse(&t.visual_bg, &d.visual_bg),
            playing: parse(&t.playing, &d.playing),
            error: parse(&t.error, &d.error),
            spectrum: t.spectrum.iter().filter_map(|c| parse_color(c)).collect(),
        };
        let theme = if theme.spectrum.is_empty() {
            Self {
                spectrum: vec![theme.accent],
                ..theme
            }
        } else {
            theme
        };
        (theme, problems)
    }

    pub fn base(&self) -> Style {
        Style::new().fg(self.fg).bg(self.bg)
    }

    pub fn dim(&self) -> Style {
        Style::new().fg(self.dim)
    }

    pub fn accent(&self) -> Style {
        Style::new().fg(self.accent)
    }

    pub fn selected(&self) -> Style {
        Style::new()
            .fg(self.selection_fg)
            .bg(self.selection_bg)
            .add_modifier(Modifier::BOLD)
    }

    /// Picks a color along the spectrum gradient, `t` in `0..=1`.
    pub fn gradient(&self, t: f32) -> Color {
        let n = self.spectrum.len();
        if n == 1 {
            return self.spectrum[0];
        }
        let pos = t.clamp(0.0, 1.0) * (n - 1) as f32;
        let i = (pos.floor() as usize).min(n - 2);
        lerp_color(self.spectrum[i], self.spectrum[i + 1], pos - i as f32)
    }
}

pub fn parse_color(s: &str) -> Option<Color> {
    match s.trim().to_ascii_lowercase().as_str() {
        "" | "reset" | "default" | "none" => Some(Color::Reset),
        other => Color::from_str(other).ok(),
    }
}

/// Blends RGB colors; non-RGB colors snap to the nearer endpoint.
pub fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
            Color::Rgb(l(r1, r2), l(g1, g2), l(b1, b2))
        }
        _ if t < 0.5 => a,
        _ => b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(parse_color("#ff0000"), Some(Color::Rgb(255, 0, 0)));
        assert_eq!(parse_color("Cyan"), Some(Color::Cyan));
        assert_eq!(parse_color("reset"), Some(Color::Reset));
        assert_eq!(parse_color("nope"), None);
        let (t, problems) = Theme::from_config(&crate::config::Theme::default());
        assert!(problems.is_empty());
        assert_eq!(t.gradient(0.0), t.spectrum[0]);
        assert_eq!(t.gradient(1.0), *t.spectrum.last().unwrap());
    }
}
