//! Artist genres from MusicBrainz (musicbrainz.org, free and open, no account). Spotify's
//! apps don't show genres anymore, so artist pages take them from here.
//!
//! MusicBrainz asks for at most one request per second, so requests are spaced out, and
//! answers are cached for the session.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use parking_lot::Mutex;

use crate::model::matching::normalize;
use crate::pending::Pending;

const API: &str = "https://musicbrainz.org/ws/2";
const SPACING: Duration = Duration::from_millis(1100);
/// Genres shown per artist.
const MAX: usize = 4;

pub struct MusicBrainz {
    http: Option<reqwest::blocking::Client>,
    /// When the last request went out (requests take turns through this lock).
    last: Mutex<Option<Instant>>,
    cache: Mutex<HashMap<String, Vec<String>>>,
}

impl MusicBrainz {
    pub fn new() -> Arc<Self> {
        let http = reqwest::blocking::Client::builder()
            .user_agent("Khoostic/0.1 ( music player )")
            .timeout(Duration::from_secs(12))
            .build()
            .ok();
        Arc::new(Self {
            http,
            last: Mutex::new(None),
            cache: Mutex::default(),
        })
    }

    /// The artist's main genres, most voted first (empty when MusicBrainz doesn't know them).
    pub fn genres(self: &Arc<Self>, artist: &str) -> Pending<Vec<String>> {
        let key = normalize(artist);
        if let Some(hit) = self.cache.lock().get(&key) {
            return Pending::ready(Ok(hit.clone()));
        }
        let (mb, name) = (self.clone(), artist.to_owned());
        Pending::spawn("khoostic-genres", move || {
            let found = mb.lookup(&name)?;
            mb.cache.lock().insert(key, found.clone());
            Ok(found)
        })
    }

    fn get(&self, url: url::Url) -> anyhow::Result<serde_json::Value> {
        let http = self.http.as_ref().context("no HTTP client")?;
        let mut last = self.last.lock();
        if let Some(at) = *last {
            let wait = SPACING.saturating_sub(at.elapsed());
            std::thread::sleep(wait);
        }
        let res = http.get(url).send();
        *last = Some(Instant::now());
        drop(last);
        Ok(res?.error_for_status()?.json()?)
    }

    fn lookup(&self, artist: &str) -> anyhow::Result<Vec<String>> {
        let query = format!("artist:\"{}\"", artist.replace('"', ""));
        let found = self.get(url::Url::parse_with_params(
            &format!("{API}/artist/"),
            [("query", query.as_str()), ("fmt", "json"), ("limit", "5")],
        )?)?;
        let Some(best) = best_match(&found, artist) else {
            return Ok(Vec::new());
        };
        let id = best["id"].as_str().context("artist without id")?;
        let detail = self.get(url::Url::parse_with_params(
            &format!("{API}/artist/{id}"),
            [("inc", "genres"), ("fmt", "json")],
        )?)?;
        let genres = top_names(&detail["genres"]);
        Ok(if genres.is_empty() {
            top_names(&best["tags"])
        } else {
            genres
        })
    }
}

/// The search result that is this artist: a confident score and the same name.
fn best_match<'a>(found: &'a serde_json::Value, artist: &str) -> Option<&'a serde_json::Value> {
    let want = normalize(artist);
    found["artists"].as_array()?.iter().find(|a| {
        a["score"].as_u64().unwrap_or(0) >= 90
            && a["name"].as_str().is_some_and(|n| normalize(n) == want)
    })
}

/// Names from a `[{ "name", "count" }]` list, most counted first.
fn top_names(list: &serde_json::Value) -> Vec<String> {
    let mut items: Vec<(i64, &str)> = list
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|g| Some((g["count"].as_i64().unwrap_or(0), g["name"].as_str()?)))
        .filter(|(c, _)| *c > 0)
        .collect();
    items.sort_by_key(|i| std::cmp::Reverse(i.0));
    items
        .into_iter()
        .take(MAX)
        .map(|(_, n)| title_case(n))
        .collect()
}

/// MusicBrainz genres are lowercase ("french house"); show them like Spotify did.
fn title_case(s: &str) -> String {
    s.split(' ')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(_) if w.contains('&') => w.to_uppercase(),
                Some(f) => f.to_uppercase().chain(c).collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_same_artist_and_top_genres() {
        let found = serde_json::json!({ "artists": [
            { "id": "a", "name": "Daft Punk Tribute", "score": 95 },
            { "id": "b", "name": "Daft Punk", "score": 100,
              "tags": [{ "name": "house", "count": 3 }] },
        ]});
        assert_eq!(best_match(&found, "daft punk").unwrap()["id"], "b");
        assert!(best_match(&found, "Justice").is_none());
        let genres = serde_json::json!([
            { "name": "house", "count": 5 },
            { "name": "french house", "count": 9 },
            { "name": "ignored", "count": 0 },
        ]);
        assert_eq!(top_names(&genres), ["French House", "House"]);
        assert_eq!(title_case("contemporary r&b"), "Contemporary R&B");
    }
}
