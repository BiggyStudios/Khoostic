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
    pub gui: GuiConfig,
    pub spotify: SpotifyConfig,
    pub lastfm: LastfmConfig,
    pub playback: PlaybackConfig,
    pub lyrics: LyricsConfig,
    /// The file this was loaded from (or would be), for saving settings changed in the app.
    #[serde(skip)]
    pub source: Option<PathBuf>,
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
    /// Rescan the music folders in the background at startup (only changed files are read).
    pub scan_on_startup: bool,
}

impl Default for LibraryConfig {
    fn default() -> Self {
        let music = directories::UserDirs::new()
            .and_then(|u| u.audio_dir().map(Path::to_path_buf))
            .into_iter()
            .collect();
        Self {
            music_dirs: music,
            scan_on_startup: true,
        }
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SpotifyConfig {
    /// Your own Spotify developer app's client id. Empty = the default public client, which
    /// needs no setup.
    pub client_id: String,
    /// Streaming quality in kbps: 96, 160 or 320 (320 needs Premium).
    pub bitrate: u32,
}

impl Default for SpotifyConfig {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            bitrate: 320,
        }
    }
}

/// Last.fm needs an API account of your own (free, instant): https://www.last.fm/api/account/create
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LastfmConfig {
    pub api_key: String,
    pub api_secret: String,
    /// Send what you listen to (once signed in).
    pub scrobble: bool,
}

impl Default for LastfmConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            api_secret: String::new(),
            scrobble: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LyricsConfig {
    /// Where to look for lyrics, in order: "local" (.lrc files next to songs, lyrics in their
    /// tags), "spotify" (needs Spotify connected; also used for local songs it can match) and
    /// "lrclib" (lrclib.net, free and public).
    pub providers: Vec<String>,
}

impl Default for LyricsConfig {
    fn default() -> Self {
        Self {
            providers: vec!["local".into(), "spotify".into(), "lrclib".into()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PlaybackConfig {
    /// When the queue runs out, keep playing similar songs.
    pub autoplay: bool,
}

impl Default for PlaybackConfig {
    fn default() -> Self {
        Self { autoplay: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct GuiConfig {
    /// Font family; empty = the system's default sans-serif.
    pub font: String,
    /// Base text size in logical pixels.
    pub font_size: f32,
    /// Extra UI scale on top of the OS DPI scale.
    pub scale: f32,
    /// Wait for vblank. Off = lowest latency (mailbox/immediate) where supported.
    pub vsync: bool,
    /// Frame rate while the window is focused and something moves (30, 60, 120, ...).
    pub fps: u32,
    /// Frame rate while the window is visible but not focused. Minimized windows don't render.
    pub background_fps: u32,
    /// Corner radius of panels and rows, in logical pixels.
    pub corner_radius: f32,
    /// Bars around the cover art in the player bar that move with the music.
    pub cover_visualizer: bool,
    pub width: u32,
    pub height: u32,
    pub theme: GuiTheme,
}

impl Default for GuiConfig {
    fn default() -> Self {
        Self {
            font: String::new(),
            font_size: 14.0,
            scale: 1.0,
            vsync: true,
            fps: 120,
            background_fps: 30,
            corner_radius: 10.0,
            cover_visualizer: true,
            width: 1200,
            height: 760,
            theme: GuiTheme::default(),
        }
    }
}

/// GUI theme: a built-in one by `name` ("khoostic", "pink", "catppuccin"), with any colors
/// set here (`"#rrggbb"` or `"#rrggbbaa"`) overriding it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct GuiTheme {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface_hover: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dim: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playing: Option<String>,
    /// Visualizer gradient, low to high.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spectrum: Option<Vec<String>>,
}

impl Default for GuiTheme {
    fn default() -> Self {
        Self {
            name: "khoostic".into(),
            background: None,
            surface: None,
            surface_hover: None,
            fg: None,
            dim: None,
            accent: None,
            playing: None,
            spectrum: None,
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
        let mut config = match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).with_context(|| format!("in {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        config.source = Some(path);
        Ok(config)
    }

    /// Sets one setting (`"gui.fps"` = 120, `"lastfm.api_key"` = "…") in the file at `path`,
    /// creating the file if needed and keeping everything else in it, comments included.
    pub fn save_value<V: Into<toml_edit::Value>>(
        path: &Path,
        dotted_key: &str,
        value: V,
    ) -> anyhow::Result<()> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let out = set_value(&text, dotted_key, value.into())?;
        // Never write something that wouldn't load back.
        Self::parse(&out).context("the edited config would be invalid")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, out).with_context(|| format!("writing {}", path.display()))
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

fn set_value(text: &str, dotted_key: &str, value: toml_edit::Value) -> anyhow::Result<String> {
    let mut doc: toml_edit::DocumentMut = text.parse().context("parsing config")?;
    let mut parts: Vec<&str> = dotted_key.split('.').collect();
    let key = parts.pop().context("empty key")?;
    let mut table = doc.as_table_mut();
    for part in parts {
        table = table
            .entry(part)
            .or_insert_with(toml_edit::table)
            .as_table_mut()
            .with_context(|| format!("{part} is not a table"))?;
    }
    table[key] = toml_edit::Item::Value(value);
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_value_keeps_comments_and_creates_tables() {
        let text = "# mine
[gui]
font_size = 15 # big
";
        let out = set_value(text, "gui.fps", 60.into()).unwrap();
        assert!(out.contains("# mine") && out.contains("# big"));
        let c = Config::parse(&out).unwrap();
        assert_eq!((c.gui.fps, c.gui.font_size), (60, 15.0));
        let fresh =
            Config::parse(&set_value("", "gui.background_fps", 10.into()).unwrap()).unwrap();
        assert_eq!(fresh.gui.background_fps, 10);
    }

    #[test]
    fn lists_save_too() {
        let order: toml_edit::Array = ["lrclib", "local"].into_iter().collect();
        let c = Config::parse(&set_value("", "lyrics.providers", order.into()).unwrap()).unwrap();
        assert_eq!(c.lyrics.providers, ["lrclib", "local"]);
    }

    #[test]
    fn theme_name_saves_next_to_other_gui_settings() {
        let text = "[gui]\nfps = 120\n";
        let out = set_value(text, "gui.theme.name", "pink".into()).unwrap();
        let c = Config::parse(&out).unwrap();
        assert_eq!(c.gui.theme.name, "pink");
        assert_eq!(c.gui.fps, 120);
        assert_eq!(c.gui.theme.accent, None);
        // Overrides still load next to a name.
        let c =
            Config::parse("[gui.theme]\nname = \"catppuccin\"\naccent = \"#ff0000\"\n").unwrap();
        assert_eq!(c.gui.theme.accent.as_deref(), Some("#ff0000"));
    }

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
