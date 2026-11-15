//! Spotify Web API: the subset Khoostic needs, mapped onto Khoostic's own types.

use std::time::Duration;

use anyhow::{Context, bail};
use khoostic_core::{CoverRef, Track, TrackLocator};
use serde::Deserialize;
use serde::de::DeserializeOwned;

const BASE: &str = "https://api.spotify.com/v1";

#[derive(Debug, Clone, Deserialize)]
pub struct Me {
    pub id: String,
    pub display_name: Option<String>,
    /// "premium", "free", ... (needs `user-read-private`).
    pub product: Option<String>,
    pub country: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Image {
    pub url: String,
    pub width: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiArtist {
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiAlbum {
    id: Option<String>,
    name: String,
    #[serde(default)]
    images: Vec<Image>,
    #[serde(default)]
    artists: Vec<ApiArtist>,
    release_date: Option<String>,
    total_tracks: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiTrack {
    id: Option<String>,
    name: String,
    #[serde(default)]
    artists: Vec<ApiArtist>,
    album: Option<ApiAlbum>,
    duration_ms: u64,
    track_number: Option<u32>,
    disc_number: Option<u32>,
    #[serde(default)]
    is_local: bool,
}

#[derive(Debug, Deserialize)]
struct Page<T> {
    items: Vec<T>,
    next: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlaylistSummary {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub images: Option<Vec<Image>>,
    pub owner: Owner,
    pub tracks: Option<TracksRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Owner {
    pub id: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TracksRef {
    pub total: u32,
}

#[derive(Debug, Clone)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub owner: String,
    pub owned_by_me: bool,
    pub tracks: u32,
    pub cover: Option<CoverRef>,
}

#[derive(Debug, Clone)]
pub struct AlbumSummary {
    pub id: String,
    pub name: String,
    pub artist: Option<String>,
    pub year: Option<u16>,
    pub tracks: u32,
    pub cover: Option<CoverRef>,
}

#[derive(Debug, Clone, Default)]
pub struct SearchResults {
    pub tracks: Vec<Track>,
    pub albums: Vec<AlbumSummary>,
    pub playlists: Vec<Playlist>,
}

/// Picks the image closest to ~300px wide: sharp in lists, small to download.
fn pick_image(images: &[Image]) -> Option<CoverRef> {
    images
        .iter()
        .min_by_key(|i| i.width.unwrap_or(640).abs_diff(300))
        .map(|i| CoverRef::Url(i.url.clone()))
}

fn year(date: &Option<String>) -> Option<u16> {
    date.as_deref()?.get(..4)?.parse().ok()
}

fn to_track(t: ApiTrack, album_override: Option<&ApiAlbum>) -> Option<Track> {
    // Local files inside Spotify playlists have no id and can't be streamed.
    let id = t.id.filter(|_| !t.is_local)?;
    let album = t.album.as_ref().or(album_override);
    Some(Track {
        locator: TrackLocator::Spotify { id },
        title: t.name,
        artists: t.artists.into_iter().map(|a| a.name).collect(),
        album: album.map(|a| a.name.clone()),
        album_artist: album.and_then(|a| a.artists.first().map(|x| x.name.clone())),
        track_number: t.track_number,
        disc_number: t.disc_number,
        year: album.and_then(|a| year(&a.release_date)),
        genre: None,
        duration: Some(Duration::from_millis(t.duration_ms)),
        cover: album.and_then(|a| pick_image(&a.images)),
    })
}

fn to_album(a: ApiAlbum) -> Option<AlbumSummary> {
    Some(AlbumSummary {
        id: a.id?,
        artist: a.artists.first().map(|x| x.name.clone()),
        year: year(&a.release_date),
        tracks: a.total_tracks.unwrap_or(0),
        cover: pick_image(&a.images),
        name: a.name,
    })
}

fn to_playlist(p: PlaylistSummary, me: &str) -> Playlist {
    Playlist {
        owned_by_me: p.owner.id == me,
        owner: p.owner.display_name.unwrap_or(p.owner.id),
        tracks: p.tracks.map_or(0, |t| t.total),
        cover: p.images.as_deref().and_then(pick_image),
        id: p.id,
        name: p.name,
    }
}

/// A thin authenticated client. Tokens are supplied per call so refreshes stay outside.
pub struct Api<'a> {
    pub http: &'a reqwest::Client,
    pub token: &'a str,
}

impl Api<'_> {
    async fn send(&self, req: reqwest::RequestBuilder) -> anyhow::Result<reqwest::Response> {
        // Retry politely on rate limiting; Spotify says how long to wait.
        for attempt in 0..4 {
            let r = req
                .try_clone()
                .context("request not retryable")?
                .bearer_auth(self.token)
                .send()
                .await?;
            if r.status() == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt < 3 {
                let wait = r
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok()?.parse::<u64>().ok())
                    .unwrap_or(2)
                    .min(30);
                tracing::warn!("Spotify rate limit: waiting {wait}s");
                tokio::time::sleep(Duration::from_secs(wait)).await;
                continue;
            }
            if !r.status().is_success() {
                let status = r.status();
                let body = r.text().await.unwrap_or_default();
                bail!(
                    "Spotify API {status}: {}",
                    body.chars().take(240).collect::<String>()
                );
            }
            return Ok(r);
        }
        bail!("Spotify kept rate limiting; try again in a minute")
    }

    async fn get<T: DeserializeOwned>(&self, url: &str) -> anyhow::Result<T> {
        let url = if url.starts_with("http") {
            url.to_owned()
        } else {
            format!("{BASE}{url}")
        };
        Ok(self.send(self.http.get(url)).await?.json().await?)
    }

    /// Follows `next` links until done or `limit` items were collected.
    async fn all<T: DeserializeOwned>(&self, first: &str, limit: usize) -> anyhow::Result<Vec<T>> {
        let mut out = Vec::new();
        let mut next = Some(first.to_owned());
        while let Some(url) = next.take() {
            let page: Page<T> = self.get(&url).await?;
            out.extend(page.items);
            if out.len() < limit {
                next = page.next;
            }
        }
        out.truncate(limit);
        Ok(out)
    }

    pub async fn me(&self) -> anyhow::Result<Me> {
        self.get("/me").await
    }

    pub async fn search(&self, query: &str, limit: u32) -> anyhow::Result<SearchResults> {
        #[derive(Deserialize)]
        struct Resp {
            tracks: Option<Page<ApiTrack>>,
            albums: Option<Page<ApiAlbum>>,
            playlists: Option<Page<Option<PlaylistSummary>>>,
        }
        let mut url = url::Url::parse(&format!("{BASE}/search")).expect("static");
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("type", "track,album,playlist")
            .append_pair("limit", &limit.to_string());
        let r: Resp = self.get(url.as_str()).await?;
        Ok(SearchResults {
            tracks: r
                .tracks
                .map(|p| {
                    p.items
                        .into_iter()
                        .filter_map(|t| to_track(t, None))
                        .collect()
                })
                .unwrap_or_default(),
            albums: r
                .albums
                .map(|p| p.items.into_iter().filter_map(to_album).collect())
                .unwrap_or_default(),
            // Spotify returns nulls in playlist search results; skip them.
            playlists: r
                .playlists
                .map(|p| {
                    p.items
                        .into_iter()
                        .flatten()
                        .map(|p| to_playlist(p, ""))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    pub async fn my_playlists(&self, me: &str) -> anyhow::Result<Vec<Playlist>> {
        let items: Vec<PlaylistSummary> = self.all("/me/playlists?limit=50", 2000).await?;
        Ok(items.into_iter().map(|p| to_playlist(p, me)).collect())
    }

    pub async fn playlist_tracks(&self, id: &str) -> anyhow::Result<Vec<Track>> {
        #[derive(Deserialize)]
        struct Item {
            track: Option<ApiTrack>,
        }
        let fields = "next,total,items(track(id,name,is_local,duration_ms,track_number,disc_number,artists(id,name),album(id,name,release_date,total_tracks,images,artists(id,name))))";
        let url = format!("/playlists/{id}/tracks?limit=100&fields={fields}");
        let items: Vec<Item> = self.all(&url, 10_000).await?;
        Ok(items
            .into_iter()
            .filter_map(|i| to_track(i.track?, None))
            .collect())
    }

    pub async fn saved_tracks(&self) -> anyhow::Result<Vec<Track>> {
        #[derive(Deserialize)]
        struct Item {
            track: ApiTrack,
        }
        let items: Vec<Item> = self.all("/me/tracks?limit=50", 10_000).await?;
        Ok(items
            .into_iter()
            .filter_map(|i| to_track(i.track, None))
            .collect())
    }

    pub async fn album_tracks(&self, id: &str) -> anyhow::Result<Vec<Track>> {
        let album: ApiAlbum = self.get(&format!("/albums/{id}")).await?;
        let items: Vec<ApiTrack> = self
            .all(&format!("/albums/{id}/tracks?limit=50"), 1000)
            .await?;
        Ok(items
            .into_iter()
            .filter_map(|t| to_track(t, Some(&album)))
            .collect())
    }

    /// Adds Spotify tracks to a playlist in batches of 100. Returns how many were sent.
    pub async fn add_to_playlist(&self, playlist: &str, ids: &[String]) -> anyhow::Result<usize> {
        for chunk in ids.chunks(100) {
            let uris: Vec<String> = chunk
                .iter()
                .map(|id| format!("spotify:track:{id}"))
                .collect();
            let req = self
                .http
                .post(format!("{BASE}/playlists/{playlist}/tracks"))
                .json(&serde_json::json!({ "uris": uris }));
            self.send(req).await?;
        }
        Ok(ids.len())
    }

    pub async fn create_playlist(&self, me: &str, name: &str) -> anyhow::Result<String> {
        #[derive(Deserialize)]
        struct Created {
            id: String,
        }
        let req = self
            .http
            .post(format!("{BASE}/users/{me}/playlists"))
            .json(&serde_json::json!({ "name": name, "public": false, "description": "Made with Khoostic" }));
        let c: Created = self.send(req).await?.json().await?;
        Ok(c.id)
    }
}

/// Splits tracks into Spotify ids (addable to Spotify playlists) and everything else.
pub fn spotify_ids(tracks: &[Track]) -> (Vec<String>, usize) {
    let mut ids = Vec::new();
    let mut other = 0;
    for t in tracks {
        match &t.locator {
            TrackLocator::Spotify { id } => ids.push(id.clone()),
            _ => other += 1,
        }
    }
    (ids, other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_tracks_and_skips_locals() {
        let json = r#"{"items":[
            {"track":{"id":"abc","name":"Song","duration_ms":200000,"track_number":3,"disc_number":1,
              "artists":[{"id":"a1","name":"Artist"},{"id":"a2","name":"Feat"}],
              "album":{"id":"al","name":"Album","release_date":"2013-05-17","total_tracks":13,
                "images":[{"url":"big","width":640},{"url":"mid","width":300},{"url":"small","width":64}],
                "artists":[{"id":"a1","name":"Artist"}]}}},
            {"track":{"id":null,"name":"My Local File","duration_ms":1000,"is_local":true,"artists":[]}},
            {"track":null}
        ],"next":null,"total":3}"#;
        #[derive(Deserialize)]
        struct Item {
            track: Option<ApiTrack>,
        }
        let page: Page<Item> = serde_json::from_str(json).unwrap();
        let tracks: Vec<Track> = page
            .items
            .into_iter()
            .filter_map(|i| to_track(i.track?, None))
            .collect();
        assert_eq!(tracks.len(), 1);
        let t = &tracks[0];
        assert_eq!(t.locator, TrackLocator::Spotify { id: "abc".into() });
        assert_eq!(t.artists, vec!["Artist", "Feat"]);
        assert_eq!((t.year, t.track_number), (Some(2013), Some(3)));
        assert_eq!(t.cover, Some(CoverRef::Url("mid".into())));
        assert_eq!(t.album_artist.as_deref(), Some("Artist"));
    }

    #[test]
    fn local_tracks_are_rejected_for_spotify_playlists() {
        let tracks = vec![
            Track {
                locator: TrackLocator::Spotify { id: "x".into() },
                ..Default::default()
            },
            Track::from_path("C:/music/local.flac"),
            Track {
                locator: TrackLocator::Spotify { id: "y".into() },
                ..Default::default()
            },
        ];
        assert_eq!(
            spotify_ids(&tracks),
            (vec!["x".to_string(), "y".to_string()], 1)
        );
    }
}
