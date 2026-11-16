//! Lyrics for any song, from several providers tried in the configured order:
//! * `local`: an `.lrc` file next to the song, or lyrics in its tags;
//! * `spotify`: Spotify's own lyrics, also for local songs it can match to a Spotify track;
//! * `lrclib`: lrclib.net, a free public collection of (mostly synced) lyrics.
//!
//! Results are cached per song for the session, including "none found".

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use lofty::prelude::*;
use parking_lot::Mutex;

use crate::config::LyricsConfig;
use crate::model::matching::is_song;
use crate::model::{Lyrics, Track, TrackLocator};
use crate::pending::Pending;
use crate::spotify::Spotify;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Local,
    Spotify,
    Lrclib,
}

impl Provider {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "local" | "file" | "files" => Some(Self::Local),
            "spotify" => Some(Self::Spotify),
            "lrclib" => Some(Self::Lrclib),
            _ => None,
        }
    }
}

const LRCLIB: &str = "https://lrclib.net/api";
const TIMEOUT: Duration = Duration::from_secs(12);

pub struct LyricsService {
    providers: Vec<Provider>,
    spotify: Option<Arc<Spotify>>,
    http: Option<reqwest::blocking::Client>,
    cache: Mutex<HashMap<String, Option<Arc<Lyrics>>>>,
}

impl LyricsService {
    /// Unknown provider names are skipped (and returned, to show as a config problem).
    pub fn new(cfg: &LyricsConfig, spotify: Option<Arc<Spotify>>) -> (Arc<Self>, Vec<String>) {
        let mut problems = Vec::new();
        let providers = cfg
            .providers
            .iter()
            .filter_map(|p| {
                let parsed = Provider::parse(p);
                if parsed.is_none() {
                    problems.push(format!("lyrics.providers: unknown provider “{p}”"));
                }
                parsed
            })
            .collect();
        let http = reqwest::blocking::Client::builder()
            .user_agent("Khoostic (music player)")
            .timeout(TIMEOUT)
            .build()
            .ok();
        let svc = Self {
            providers,
            spotify,
            http,
            cache: Mutex::default(),
        };
        (Arc::new(svc), problems)
    }

    /// Already looked up this session (`Some(None)` = there are none).
    pub fn cached(&self, track: &Track) -> Option<Option<Arc<Lyrics>>> {
        self.cache.lock().get(&track.locator.to_uri()).cloned()
    }

    /// Looks up lyrics in the background.
    pub fn get(self: &Arc<Self>, track: &Track) -> Pending<Option<Arc<Lyrics>>> {
        if let Some(hit) = self.cached(track) {
            return Pending::ready(Ok(hit));
        }
        let (svc, track) = (self.clone(), track.clone());
        Pending::spawn("khoostic-lyrics", move || {
            let found = svc.find(&track).map(Arc::new);
            svc.cache
                .lock()
                .insert(track.locator.to_uri(), found.clone());
            Ok(found)
        })
    }

    fn find(&self, track: &Track) -> Option<Lyrics> {
        for p in &self.providers {
            let result = match p {
                Provider::Local => Ok(local(track)),
                Provider::Spotify => self.via_spotify(track),
                Provider::Lrclib => self.via_lrclib(track),
            };
            match result {
                Ok(Some(l)) if !l.is_empty() => return Some(l),
                Ok(_) => {}
                Err(e) => tracing::debug!("lyrics ({p:?}) for {}: {e:#}", track.title),
            }
        }
        None
    }

    fn via_spotify(&self, track: &Track) -> anyhow::Result<Option<Lyrics>> {
        let Some(sp) = self.spotify.as_ref().filter(|s| s.is_connected()) else {
            return Ok(None);
        };
        let id = match &track.locator {
            TrackLocator::Spotify { id } => id.clone(),
            _ => {
                // Find the same song on Spotify.
                let artist = track.primary_artist().unwrap_or_default();
                if artist.is_empty() {
                    return Ok(None);
                }
                let found = sp.search(&format!("{artist} {}", track.title), 10).wait()?;
                let same = found
                    .tracks
                    .iter()
                    .find(|t| is_song(t, artist, &track.title));
                match same.map(|t| &t.locator) {
                    Some(TrackLocator::Spotify { id }) => id.clone(),
                    _ => return Ok(None),
                }
            }
        };
        sp.lyrics(&id).wait()
    }

    fn via_lrclib(&self, track: &Track) -> anyhow::Result<Option<Lyrics>> {
        let Some(http) = &self.http else {
            return Ok(None);
        };
        let artist = track.primary_artist().unwrap_or_default().to_owned();
        if artist.is_empty() || track.title.is_empty() {
            return Ok(None);
        }
        let mut q = vec![
            ("track_name", track.title.clone()),
            ("artist_name", artist.clone()),
        ];
        if let Some(album) = &track.album {
            q.push(("album_name", album.clone()));
        }
        if let Some(d) = track.duration {
            q.push(("duration", d.as_secs().to_string()));
        }
        let url = url::Url::parse_with_params(&format!("{LRCLIB}/get"), &q)?;
        let res = http.get(url).send()?;
        if res.status().is_success() {
            let v: serde_json::Value = res.json().context("reading LRCLIB")?;
            if let Some(l) = lrclib_lyrics(&v) {
                return Ok(Some(l));
            }
        }
        // No exact match (different album or length): search, and take the closest length.
        let url = url::Url::parse_with_params(
            &format!("{LRCLIB}/search"),
            [("track_name", &track.title), ("artist_name", &artist)],
        )?;
        let res = http.get(url).send()?.error_for_status()?;
        let list: Vec<serde_json::Value> = res.json().context("reading LRCLIB")?;
        let want = track.duration.map(|d| d.as_secs_f64());
        let best = list
            .iter()
            .filter(|v| v["syncedLyrics"].is_string() || v["plainLyrics"].is_string())
            .filter(|v| {
                let d = v["duration"].as_f64();
                match (want, d) {
                    (Some(w), Some(d)) => (w - d).abs() <= 8.0,
                    _ => true,
                }
            })
            .max_by_key(|v| v["syncedLyrics"].is_string());
        Ok(best.and_then(lrclib_lyrics))
    }
}

fn lrclib_lyrics(v: &serde_json::Value) -> Option<Lyrics> {
    if let Some(s) = v["syncedLyrics"].as_str().filter(|s| !s.trim().is_empty()) {
        return Some(Lyrics::from_lrc(s, "LRCLIB"));
    }
    v["plainLyrics"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(|s| Lyrics::plain(s, "LRCLIB"))
}

/// An `.lrc` next to the file, else lyrics stored in its tags.
fn local(track: &Track) -> Option<Lyrics> {
    let TrackLocator::Local(path) = &track.locator else {
        return None;
    };
    if let Ok(text) = std::fs::read_to_string(path.with_extension("lrc")) {
        return Some(Lyrics::from_lrc(&text, "Local file"));
    }
    embedded(path).map(|text| Lyrics::from_lrc(&text, "Song tags"))
}

fn embedded(path: &Path) -> Option<String> {
    let file = lofty::read_from_path(path).ok()?;
    file.tags().iter().find_map(|tag| {
        tag.get_string(ItemKey::Lyrics)
            .or_else(|| tag.get_string(ItemKey::UnsyncLyrics))
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_parse() {
        assert_eq!(Provider::parse(" LRCLIB "), Some(Provider::Lrclib));
        assert_eq!(Provider::parse("files"), Some(Provider::Local));
        assert_eq!(Provider::parse("genius"), None);
        let cfg = LyricsConfig {
            providers: vec!["spotify".into(), "nope".into()],
        };
        let (svc, problems) = LyricsService::new(&cfg, None);
        assert_eq!(svc.providers, [Provider::Spotify]);
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn lrclib_prefers_synced() {
        let v = serde_json::json!({ "syncedLyrics": "[00:01.00]hi", "plainLyrics": "hi" });
        assert!(lrclib_lyrics(&v).unwrap().synced);
        let v = serde_json::json!({ "syncedLyrics": null, "plainLyrics": "hi\nthere" });
        let l = lrclib_lyrics(&v).unwrap();
        assert!(!l.synced);
        assert_eq!(l.lines.len(), 2);
        assert!(lrclib_lyrics(&serde_json::json!({ "instrumental": true })).is_none());
    }

    #[test]
    fn lrc_file_next_to_a_song() {
        let dir = std::env::temp_dir().join(format!("khoostic-lyrics-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let song = dir.join("song.flac");
        std::fs::write(dir.join("song.lrc"), "[00:02.00]line").unwrap();
        let l = local(&Track::from_path(&song)).unwrap();
        assert!(l.synced);
        assert_eq!(l.source, "Local file");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
