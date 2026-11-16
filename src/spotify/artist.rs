//! Artist profiles: the mobile app's artist page (monthly listeners, popular songs with play
//! counts, popular releases, fans also like) combined with the artist's metadata (top songs,
//! the full discography, biography).

use std::collections::HashMap;

use anyhow::Context as _;
use http::Method;
use librespot_core::SpotifyUri;
use librespot_core::session::Session;
use librespot_protocol::extension_kind::ExtensionKind;
use librespot_protocol::metadata;
use protobuf::Message;
use serde_json::Value;

use crate::model::{CoverRef, Track};
use crate::spotify::AlbumSummary;
use crate::spotify::api::{self, album_summary, b62, pick_image};

/// An artist in a list: search results, "fans also like", followed artists.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtistSummary {
    pub id: String,
    pub name: String,
    pub image: Option<CoverRef>,
}

#[derive(Debug, Clone, Default)]
pub struct ArtistPage {
    pub id: String,
    pub name: String,
    pub image: Option<CoverRef>,
    pub verified: bool,
    /// As Spotify words it: "27.8M monthly listeners".
    pub monthly_listeners: Option<String>,
    pub top_tracks: Vec<Track>,
    /// Play counts ("1,004,878,020") for `top_tracks`, where Spotify shows them.
    pub plays: Vec<Option<String>>,
    pub popular_releases: Vec<AlbumSummary>,
    pub albums: Vec<AlbumSummary>,
    pub singles: Vec<AlbumSummary>,
    pub related: Vec<ArtistSummary>,
    pub bio: Option<String>,
}

/// Albums per discography section loaded with the page.
const DISCOGRAPHY: usize = 40;

pub async fn page(session: &Session, id: &str) -> anyhow::Result<ArtistPage> {
    let uri = format!("spotify:artist:{id}");
    let (hub, meta) = tokio::join!(hub(session, id), metadata(session, &uri));
    let meta = meta?;
    // The hub is a bonus (listeners, counts, related): the page still works without it.
    let hub = hub.unwrap_or_else(|e| {
        tracing::debug!("artist hub for {id}: {e:#}");
        Value::Null
    });

    // Top songs for this account's country (or whichever list there is).
    let country = session.country();
    let top = meta
        .top_track
        .iter()
        .find(|t| t.country() == country)
        .or(meta.top_track.first());
    let top_uris: Vec<String> = top
        .map(|t| t.track.iter().filter_map(|t| b62(t.gid())))
        .into_iter()
        .flatten()
        .map(|id| format!("spotify:track:{id}"))
        .collect();
    let album_uris = |groups: &[metadata::AlbumGroup]| -> Vec<String> {
        groups
            .iter()
            .filter_map(|g| g.album.first())
            .filter_map(|a| b62(a.gid()))
            .take(DISCOGRAPHY)
            .map(|id| format!("spotify:album:{id}"))
            .collect()
    };
    let (album_ids, single_ids) = (
        album_uris(&meta.album_group),
        album_uris(&meta.single_group),
    );
    let all: Vec<String> = album_ids.iter().chain(&single_ids).cloned().collect();
    let (tracks, albums) = tokio::join!(
        api::tracks(session, &top_uris),
        api::extended::<metadata::Album>(session, &all, ExtensionKind::ALBUM_V4),
    );
    let top_tracks = tracks?;
    let albums = albums.unwrap_or_default();
    let summaries = |ids: &[String]| -> Vec<AlbumSummary> {
        let mut out: Vec<AlbumSummary> = ids
            .iter()
            .filter_map(|u| album_summary(albums.get(u)?))
            .collect();
        out.sort_by_key(|a| std::cmp::Reverse(a.year));
        out
    };

    let header = &hub["header"];
    let counts: HashMap<String, String> = sections(&hub, "artist-entity-view-top-tracks")
        .filter_map(|row| {
            let uri = row["metadata"]["uri"].as_str()?;
            let plays = row["text"]["subtitle"].as_str()?;
            Some((uri.to_owned(), plays.to_owned()))
        })
        .collect();
    let plays = top_tracks
        .iter()
        .map(|t| counts.get(&t.locator.to_uri()).cloned())
        .collect();
    let popular_releases = sections(&hub, "artist-entity-view-releases_row")
        .filter_map(|row| {
            let id = row["metadata"]["uri"]
                .as_str()?
                .strip_prefix("spotify:album:")?;
            Some(AlbumSummary {
                id: id.to_owned(),
                name: row["text"]["title"].as_str()?.to_owned(),
                artist: Some(meta.name().to_owned()),
                year: row["text"]["subtitle"]
                    .as_str()
                    .and_then(|s| s.rsplit(' ').next()?.parse().ok()),
                tracks: 0,
                cover: image(&row["images"]["main"]),
            })
        })
        .collect();
    let related = hub["body"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["id"] == "artist-entity-view-related")
        .and_then(|s| s["children"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|c| {
            Some(ArtistSummary {
                id: c["metadata"]["uri"]
                    .as_str()?
                    .strip_prefix("spotify:artist:")?
                    .to_owned(),
                name: c["text"]["title"].as_str()?.to_owned(),
                image: image(&c["images"]["main"]),
            })
        })
        .collect();

    Ok(ArtistPage {
        id: id.to_owned(),
        name: meta.name().to_owned(),
        image: image(&header["images"]["main"]).or_else(|| portrait(&meta)),
        verified: header["custom"]["verified"].as_bool().unwrap_or(false),
        monthly_listeners: header["text"]["accessory"]
            .as_str()
            .filter(|s| s.contains("listener"))
            .map(str::to_owned),
        top_tracks,
        plays,
        popular_releases,
        albums: summaries(&album_ids),
        singles: summaries(&single_ids),
        related,
        bio: meta
            .biography
            .first()
            .map(|b| strip_tags(b.text()))
            .filter(|b| !b.trim().is_empty()),
    })
}

/// Names and pictures for artist ids (search results, followed artists), in order.
pub async fn summaries(session: &Session, ids: &[String]) -> anyhow::Result<Vec<ArtistSummary>> {
    let uris: Vec<String> = ids
        .iter()
        .map(|id| format!("spotify:artist:{id}"))
        .collect();
    let found = api::extended::<metadata::Artist>(session, &uris, ExtensionKind::ARTIST_V4).await?;
    Ok(ids
        .iter()
        .zip(&uris)
        .filter_map(|(id, uri)| {
            let a = found.get(uri)?;
            Some(ArtistSummary {
                id: id.clone(),
                name: a.name().to_owned(),
                image: portrait(a),
            })
        })
        .collect())
}

async fn metadata(session: &Session, uri: &str) -> anyhow::Result<metadata::Artist> {
    let body = session
        .spclient()
        .get_artist_metadata(&SpotifyUri::from_uri(uri)?)
        .await
        .context("loading the artist")?;
    Ok(metadata::Artist::parse_from_bytes(&body)?)
}

/// The artist page as the mobile app gets it (a "hub" of components).
async fn hub(session: &Session, id: &str) -> anyhow::Result<Value> {
    let body = session
        .spclient()
        .request_as_json(
            &Method::GET,
            &format!("/artistview/v1/artist/{id}?platform=android&locale=en&country="),
            None,
            None,
        )
        .await?;
    Ok(serde_json::from_slice(&body)?)
}

/// Hub rows whose id starts with `prefix` (e.g. every "…_row3"), in order.
fn sections<'a>(hub: &'a Value, prefix: &'a str) -> impl Iterator<Item = &'a Value> {
    hub["body"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(move |c| {
            c["id"]
                .as_str()
                .is_some_and(|id| id.starts_with(prefix) && id.contains("_row"))
        })
}

fn image(v: &Value) -> Option<CoverRef> {
    v["uri"]
        .as_str()
        .filter(|u| u.starts_with("https://"))
        .map(|u| CoverRef::Url(u.to_owned()))
}

fn portrait(a: &metadata::Artist) -> Option<CoverRef> {
    let images = if a.portrait_group.image.is_empty() {
        &a.portrait
    } else {
        &a.portrait_group.image
    };
    pick_image(images)
}

/// Biographies come with links as HTML.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_sections_and_images() {
        let hub = serde_json::json!({
            "header": { "text": { "accessory": "27.8M monthly listeners" } },
            "body": [
                { "id": "artist-entity-view-top-tracks-combined" },
                { "id": "artist-entity-view-top-tracks-combined_row0",
                  "text": { "subtitle": "1,004,878,020" },
                  "metadata": { "uri": "spotify:track:x" } },
                { "id": "artist-entity-view-releases_row0",
                  "images": { "main": { "uri": "https://i.scdn.co/image/a" } } },
            ]
        });
        let rows: Vec<_> = sections(&hub, "artist-entity-view-top-tracks").collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["text"]["subtitle"], "1,004,878,020");
        let rel: Vec<_> = sections(&hub, "artist-entity-view-releases_row").collect();
        assert_eq!(
            image(&rel[0]["images"]["main"]),
            Some(CoverRef::Url("https://i.scdn.co/image/a".into()))
        );
    }

    #[test]
    fn bio_html_is_stripped() {
        assert_eq!(
            strip_tags("French duo <a href=\"spotify:artist:x\">Daft Punk</a> &amp; friends"),
            "French duo Daft Punk & friends"
        );
    }
}
