//! User configuration (`config.toml`).
//!
//! Every field has a default, so an empty or partial file is always valid. Location:
//! * Windows: `%APPDATA%\khoostic\config\config.toml`
//! * Linux:   `$XDG_CONFIG_HOME/khoostic/config.toml`
//! * macOS:   `~/Library/Application Support/khoostic/config.toml`

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub audio: AudioConfig,
    pub library: LibraryConfig,
    pub tui: TuiConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AudioConfig {
    /// Substring of the output device name; empty = system default.
    pub device: String,
    /// Startup volume, 0-100.
    pub volume: u8,
    /// How far ahead to decode, in milliseconds.
    pub buffer_ms: u32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            device: String::new(),
            volume: 80,
            buffer_ms: 1000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LibraryConfig {
    /// Folders the browser starts in / the library scans.
    pub music_dirs: Vec<PathBuf>,
}

impl Default for LibraryConfig {
    fn default() -> Self {
        let music = directories::UserDirs::new()
            .and_then(|u| u.audio_dir().map(Path::to_path_buf))
            .into_iter()
            .collect();
        Self { music_dirs: music }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct TuiConfig {
    /// Redraw rate while something is animating.
    pub fps: u32,
    /// Seek step for small seeks, in seconds (big seeks are 6x).
    pub seek_step: u32,
    /// Volume step in percent.
    pub volume_step: u8,
    pub theme: Theme,
    /// Key overrides per mode: `[tui.keys.normal] "<C-n>" = "next_track"`.
    /// Bind to `"none"` to remove a default binding.
    pub keys: BTreeMap<String, BTreeMap<String, String>>,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            fps: 60,
            seek_step: 5,
            volume_step: 5,
            theme: Theme::default(),
            keys: BTreeMap::new(),
        }
    }
}

/// Colors: names (`"cyan"`, `"darkgray"`), `"#rrggbb"`, or `"reset"` for the terminal default.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    pub fg: String,
    pub bg: String,
    pub dim: String,
    pub accent: String,
    pub selection_fg: String,
    pub selection_bg: String,
    /// Background of rows inside a visual selection.
    pub visual_bg: String,
    pub playing: String,
    pub error: String,
    /// Spectrum gradient, bottom to top.
    pub spectrum: Vec<String>,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            fg: "reset".into(),
            bg: "reset".into(),
            dim: "#6c7086".into(),
            accent: "#89dceb".into(),
            selection_fg: "#1e1e2e".into(),
            selection_bg: "#89dceb".into(),
            visual_bg: "#45475a".into(),
            playing: "#a6e3a1".into(),
            error: "#f38ba8".into(),
            spectrum: vec![
                "#89dceb".into(),
                "#89b4fa".into(),
                "#cba6f7".into(),
                "#f5c2e7".into(),
            ],
        }
    }
}

impl Config {
    pub fn default_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "khoostic")
            .map(|d| d.config_dir().join("config.toml"))
    }

    /// Loads the config from `path`, or the default location. A missing file is not an error.
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        let path = match path.map(Path::to_path_buf).or_else(Self::default_path) {
            Some(p) => p,
            None => return Ok(Self::default()),
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).with_context(|| format!("in {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn parse(text: &str) -> anyhow::Result<Self> {
        Ok(toml::from_str(text)?)
    }

    /// Writes a commented-out-free full default config, for users to edit.
    pub fn write_default(path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, toml::to_string_pretty(&Self::default())?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_is_default() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn partial_overrides() {
        let c = Config::parse(
            r#"
            [audio]
            volume = 30
            [tui.theme]
            accent = "magenta"
            [tui.keys.normal]
            "<C-n>" = "next_track"
            "#,
        )
        .unwrap();
        assert_eq!(c.audio.volume, 30);
        assert_eq!(c.audio.buffer_ms, 1000);
        assert_eq!(c.tui.theme.accent, "magenta");
        assert_eq!(c.tui.keys["normal"]["<C-n>"], "next_track");
    }

    #[test]
    fn typos_are_reported() {
        assert!(Config::parse("[audio]\nvolumee = 3").is_err());
    }

    #[test]
    fn default_round_trips() {
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        assert_eq!(Config::parse(&text).unwrap(), Config::default());
    }
}
