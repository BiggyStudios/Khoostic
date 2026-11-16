//! Artist genres for artist pages. Spotify's apps don't show genres anymore, so they come from
//! open sources that need no account:
//! * MusicBrainz (musicbrainz.org): community-voted genres ("French House");
//! * Deezer's public API, when MusicBrainz is busy or doesn't know the artist: the genres of
//!   the artist's albums ("Electro", "Dance").
//!
//! Requests are spaced out (MusicBrainz asks for at most one per second) and answers are cached
//! for the session.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use parking_lot::Mutex;
use serde_json::Value;

use crate::model::matching::normalize;
use crate::pending::Pending;

const MUSICBRAINZ: &str = "https://musicbrainz.org/ws/2";
const DEEZER: &str = "https://api.deezer.com";
const SPACING: Duration = Duration::from_millis(1100);
/// Genres shown per artist.
const MAX: usize = 4;

pub struct Genres {
    http: Option<reqwest::blocking::Client>,
    /// When the last MusicBrainz request went out (requests take turns through this lock).
    last: Mutex<Option<Instant>>,
    cache: Mutex<HashMap<String, Vec<String>>>,
    /// Deezer's genre names by id (fetched once).
    deezer_names: Mutex<Option<HashMap<i64, String>>>,
}

impl Genres {
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
            deezer_names: Mutex::new(None),
        })
    }

    /// The artist's main genres (empty when no source knows them).
    pub fn of(self: &Arc<Self>, artist: &str) -> Pending<Vec<String>> {
        let key = normalize(artist);
        if let Some(hit) = self.cache.lock().get(&key) {
            return Pending::ready(Ok(hit.clone()));
        }
        let (me, name) = (self.clone(), artist.to_owned());
        Pending::spawn("khoostic-genres", move || {
            let found = match me.musicbrainz(&name) {
                Ok(g) if !g.is_empty() => g,
                Ok(_) => me.deezer(&name)?,
                Err(e) => {
                    tracing::debug!("MusicBrainz genres for {name}: {e:#}");
                    me.deezer(&name)?
                }
            };
            me.cache.lock().insert(key, found.clone());
            Ok(found)
        })
    }

    fn get(&self, url: url::Url) -> anyhow::Result<Value> {
        let http = self.http.as_ref().context("no HTTP client")?;
        let res = http.get(url).header("Accept-Language", "en").send()?;
        Ok(res.error_for_status()?.json()?)
    }

    /// One MusicBrainz request, at least [`SPACING`] after the previous one.
    fn mb_get(&self, url: url::Url) -> anyhow::Result<Value> {
        let mut last = self.last.lock();
        if let Some(at) = *last {
            std::thread::sleep(SPACING.saturating_sub(at.elapsed()));
        }
        let res = self.get(url);
        *last = Some(Instant::now());
        res
    }

    fn musicbrainz(&self, artist: &str) -> anyhow::Result<Vec<String>> {
        let query = format!("artist:\"{}\"", artist.replace('"', ""));
        let found = self.mb_get(url::Url::parse_with_params(
            &format!("{MUSICBRAINZ}/artist/"),
            [("query", query.as_str()), ("fmt", "json"), ("limit", "5")],
        )?)?;
        let Some(best) = best_match(&found["artists"], artist) else {
            return Ok(Vec::new());
        };
        let id = best["id"].as_str().context("artist without id")?;
        let detail = self.mb_get(url::Url::parse_with_params(
            &format!("{MUSICBRAINZ}/artist/{id}"),
            [("inc", "genres"), ("fmt", "json")],
        )?)?;
        let genres = top_names(&detail["genres"]);
        Ok(if genres.is_empty() {
            top_names(&best["tags"])
        } else {
            genres
        })
    }

    /// The most common genres among the artist's albums on Deezer.
    fn deezer(&self, artist: &str) -> anyhow::Result<Vec<String>> {
        let found = self.get(url::Url::parse_with_params(
            &format!("{DEEZER}/search/artist"),
            [("q", artist), ("limit", "5")],
        )?)?;
        let want = normalize(artist);
        let Some(id) = found["data"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|a| a["name"].as_str().is_some_and(|n| normalize(n) == want))
            .and_then(|a| a["id"].as_i64())
        else {
            return Ok(Vec::new());
        };
        let albums = self.get(url::Url::parse(&format!(
            "{DEEZER}/artist/{id}/albums?limit=40"
        ))?)?;
        let mut counts: HashMap<i64, usize> = HashMap::new();
        for g in albums["data"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a["genre_id"].as_i64())
            .filter(|&g| g > 0)
        {
            *counts.entry(g).or_default() += 1;
        }
        let mut ids: Vec<(i64, usize)> = counts.into_iter().collect();
        ids.sort_by_key(|&(id, n)| (std::cmp::Reverse(n), id));
        let names = self.deezer_genre_names()?;
        Ok(ids
            .into_iter()
            .filter_map(|(id, _)| names.get(&id).cloned())
            .take(3)
            .collect())
    }

    fn deezer_genre_names(&self) -> anyhow::Result<HashMap<i64, String>> {
        if let Some(n) = self.deezer_names.lock().as_ref() {
            return Ok(n.clone());
        }
        let list = self.get(url::Url::parse(&format!("{DEEZER}/genre"))?)?;
        let names: HashMap<i64, String> = list["data"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|g| Some((g["id"].as_i64()?, g["name"].as_str()?.to_owned())))
            .collect();
        *self.deezer_names.lock() = Some(names.clone());
        Ok(names)
    }
}

/// The search result that is this artist: a confident score and the same name.
fn best_match<'a>(found: &'a Value, artist: &str) -> Option<&'a Value> {
    let want = normalize(artist);
    found.as_array()?.iter().find(|a| {
        a["score"].as_u64().unwrap_or(0) >= 90
            && a["name"].as_str().is_some_and(|n| normalize(n) == want)
    })
}

/// Names from a `[{ "name", "count" }]` list, most counted first.
fn top_names(list: &Value) -> Vec<String> {
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

/// MusicBrainz genres are lowercase ("french house"); show them capitalized.
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
        let found = serde_json::json!([
            { "id": "a", "name": "Daft Punk Tribute", "score": 95 },
            { "id": "b", "name": "Daft Punk", "score": 100,
              "tags": [{ "name": "house", "count": 3 }] },
        ]);
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
