//! Interface state that survives restarts: the library panel's size and filter, and what's
//! hidden from it. Kept out of `config.toml` (it isn't a setting) in a small JSON file.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What the library panel lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) enum LibraryFilter {
    #[default]
    All,
    Playlists,
    Artists,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct UiState {
    /// The library panel shows only covers.
    pub sidebar_collapsed: bool,
    pub library_filter: LibraryFilter,
    /// Library items hidden from the panel, by key ("sp:<id>", "local:<id>", "artist:<id>").
    pub hidden: BTreeSet<String>,
}

impl UiState {
    /// Next to the library database.
    pub(crate) fn default_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "khoostic")
            .map(|d| d.data_local_dir().join("gui-state.json"))
    }

    pub(crate) fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub(crate) fn save(&self, path: &Path) {
        let write = || -> anyhow::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
            Ok(())
        };
        if let Err(e) = write() {
            tracing::warn!("saving {}: {e:#}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_bad_files() {
        let dir = std::env::temp_dir().join(format!("khoostic-ui-{}", std::process::id()));
        let path = dir.join("gui-state.json");
        let mut s = UiState::load(&path); // missing: defaults
        assert!(!s.sidebar_collapsed);
        s.sidebar_collapsed = true;
        s.library_filter = LibraryFilter::Artists;
        s.hidden.insert("sp:abc".into());
        s.save(&path);
        let back = UiState::load(&path);
        assert!(back.sidebar_collapsed && back.hidden.contains("sp:abc"));
        assert_eq!(back.library_filter, LibraryFilter::Artists);
        std::fs::write(&path, "not json").unwrap();
        assert!(UiState::load(&path).hidden.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
