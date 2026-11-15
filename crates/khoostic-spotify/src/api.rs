//! Spotify's client API ("spclient"): the endpoints the official apps use, reached through the
//! librespot session, mapped onto Khoostic's types.
//!
//! Khoostic deliberately avoids the public Web API (api.spotify.com). It rate-limits per app,
//! and the shared app id that makes "sign in without a developer account" possible is
//! permanently over its quota there, so every request would just be told to retry later. The
//! client API is limited per user instead.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail};
use http::header::{CONTENT_TYPE, HeaderValue};
use http::{HeaderMap, Method};
use khoostic_core::{CoverRef, Track, TrackLocator};
use librespot_core::SpotifyId;
use librespot_core::session::Session;
use librespot_protocol::context_page::ContextPage;
use librespot_protocol::extended_metadata::{BatchedEntityRequest, EntityRequest, ExtensionQuery};
use librespot_protocol::extension_kind::ExtensionKind;
use librespot_protocol::metadata;
use librespot_protocol::playlist4_external::SelectedListContent;
use protobuf::{EnumOrUnknown, Message};

#[derive(Debug, Clone)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub owner: String,
    /// Whether the account can add tracks (its own or a collaborative playlist).
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

/// Tracks per extended-metadata request.
const METADATA_BATCH: usize = 250;
/// Context pages followed at most (a page is usually the whole list anyway).
const MAX_PAGES: usize = 50;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn image_url(file_id: &[u8]) -> CoverRef {
    CoverRef::Url(format!("https://i.scdn.co/image/{}", hex(file_id)))
}

/// The cover closest to ~300px wide: sharp in lists, small to download.
fn album_cover(album: &metadata::Album) -> Option<CoverRef> {
    let images = if album.cover_group.image.is_empty() {
        &album.cover
    } else {
        &album.cover_group.image
    };
    let width = |i: &metadata::Image| match (i.width(), i.size()) {
        (w, _) if w > 0 => w as u32,
        (_, metadata::image::Size::SMALL) => 64,
        (_, metadata::image::Size::LARGE) => 640,
        (_, metadata::image::Size::XLARGE) => 1280,
        _ => 300,
    };
    images
        .iter()
        .filter(|i| !i.file_id().is_empty())
        .min_by_key(|i| width(i).abs_diff(300))
        .map(|i| image_url(i.file_id()))
}

fn to_track(id: String, t: &metadata::Track) -> Track {
    let album = t.album.as_ref();
    let positive = |n: i32| u32::try_from(n).ok().filter(|&n| n > 0);
    Track {
        locator: TrackLocator::Spotify { id },
        title: t.name().to_owned(),
        artists: t.artist.iter().map(|a| a.name().to_owned()).collect(),
        album: album.map(|a| a.name().to_owned()).filter(|n| !n.is_empty()),
        album_artist: album
            .and_then(|a| a.artist.first())
            .map(|a| a.name().to_owned()),
        track_number: positive(t.number()),
        disc_number: positive(t.disc_number()),
        year: album
            .and_then(|a| a.date.as_ref())
            .and_then(|d| u16::try_from(d.year()).ok())
            .filter(|&y| y > 0),
        genre: None,
        duration: positive(t.duration()).map(|ms| Duration::from_millis(ms.into())),
        cover: album.and_then(album_cover),
    }
}

/// Raw track metadata for `uris` (`spotify:track:…`) as `(id, track)`, in the same order.
/// Anything that isn't a streamable Spotify track (local files inside playlists, episodes,
/// removed tracks) is dropped.
async fn track_metadata(
    session: &Session,
    uris: &[String],
) -> anyhow::Result<Vec<(String, metadata::Track)>> {
    let uris: Vec<&String> = uris
        .iter()
        .filter(|u| u.starts_with("spotify:track:"))
        .collect();
    let mut found: HashMap<String, metadata::Track> = HashMap::with_capacity(uris.len());
    for chunk in uris.chunks(METADATA_BATCH) {
        let req = BatchedEntityRequest {
            entity_request: chunk
                .iter()
                .map(|uri| EntityRequest {
                    entity_uri: (*uri).clone(),
                    query: vec![ExtensionQuery {
                        extension_kind: EnumOrUnknown::new(ExtensionKind::TRACK_V4),
                        ..Default::default()
                    }],
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let res = session
            .spclient()
            .get_extended_metadata(req)
            .await
            .context("loading track details")?;
        for data in res.extended_metadata.iter().flat_map(|a| &a.extension_data) {
            let Some(any) = data.extension_data.as_ref() else {
                continue;
            };
            if let Ok(t) = metadata::Track::parse_from_bytes(&any.value) {
                found.insert(data.entity_uri.clone(), t);
            }
        }
    }
    Ok(uris
        .into_iter()
        .filter_map(|u| {
            let t = found.remove(u)?;
            Some((u.strip_prefix("spotify:track:")?.to_owned(), t))
        })
        .collect())
}

/// Khoostic tracks for `uris`, in order (see [`track_metadata`]).
pub async fn tracks(session: &Session, uris: &[String]) -> anyhow::Result<Vec<Track>> {
    Ok(track_metadata(session, uris)
        .await?
        .iter()
        .map(|(id, t)| to_track(id.clone(), t))
        .collect())
}

/// Every track uri in a context (album, playlist, liked songs, search, …), following pages.
async fn context_uris(session: &Session, uri: &str, limit: usize) -> anyhow::Result<Vec<String>> {
    let sp = session.spclient();
    let ctx = sp.get_context(uri).await.context("loading from Spotify")?;
    let mut out = Vec::new();
    let mut next = None;
    for page in &ctx.pages {
        out.extend(page.tracks.iter().map(|t| t.uri().to_owned()));
        next = page.next_page_url.clone().filter(|u| !u.is_empty());
    }
    for _ in 0..MAX_PAGES {
        let Some(url) = next.take().filter(|_| out.len() < limit) else {
            break;
        };
        let body = sp.get_next_page(&url).await?;
        let page: ContextPage = protobuf_json_mapping::parse_from_str(std::str::from_utf8(&body)?)
            .context("reading a page from Spotify")?;
        out.extend(page.tracks.iter().map(|t| t.uri().to_owned()));
        next = page.next_page_url.filter(|u| !u.is_empty());
    }
    out.truncate(limit);
    Ok(out)
}

/// Display name and plan. The plan arrives in a packet shortly after connecting.
pub async fn account(session: &Session) -> (String, bool) {
    let mut premium = None;
    for _ in 0..50 {
        if let Some(kind) = session.get_user_attribute("type") {
            premium = Some(kind == "premium");
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let user = session.username();
    let name = match session.spclient().get_user_profile(&user, None, None).await {
        Ok(body) => serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("name")?.as_str().map(str::to_owned))
            .filter(|n| !n.is_empty()),
        Err(e) => {
            tracing::debug!("Spotify profile: {e}");
            None
        }
    };
    // Unknown plan: let playback try; Spotify refuses it clearly if it isn't allowed.
    (name.unwrap_or(user), premium.unwrap_or(true))
}

fn playlist_id(uri: &str) -> anyhow::Result<SpotifyId> {
    let id = uri.strip_prefix("spotify:playlist:").unwrap_or(uri);
    SpotifyId::from_base62(id).with_context(|| format!("not a Spotify playlist id: {id}"))
}

/// The account's playlists (the "rootlist"), folders flattened.
pub async fn playlists(session: &Session) -> anyhow::Result<Vec<Playlist>> {
    let me = session.username();
    let mut out = Vec::new();
    loop {
        let body = session
            .spclient()
            .get_rootlist(out.len(), Some(200))
            .await
            .context("loading your playlists")?;
        let list = SelectedListContent::parse_from_bytes(&body)?;
        let contents = list.contents.get_or_default();
        let before = out.len();
        for (i, item) in contents.items.iter().enumerate() {
            let Some(id) = item.uri().strip_prefix("spotify:playlist:") else {
                continue; // folder markers
            };
            let meta = contents.meta_items.get(i).cloned().unwrap_or_default();
            let attrs = meta.attributes.get_or_default();
            let owner = meta.owner_username().to_owned();
            let cover = attrs
                .picture_size
                .iter()
                .find(|p| p.target_name() == "default")
                .or(attrs.picture_size.first())
                .map(|p| CoverRef::Url(p.url().to_owned()))
                .or_else(|| {
                    attrs
                        .picture
                        .as_deref()
                        .filter(|p| !p.is_empty())
                        .map(image_url)
                });
            out.push(Playlist {
                id: id.to_owned(),
                name: attrs.name().to_owned(),
                owned_by_me: owner == me || meta.capabilities.get_or_default().can_edit_items(),
                owner,
                tracks: u32::try_from(meta.length()).unwrap_or(0),
                cover,
            });
        }
        if !contents.truncated() || contents.items.is_empty() || out.len() == before {
            break;
        }
    }
    Ok(out)
}

pub async fn playlist_tracks(session: &Session, id: &str) -> anyhow::Result<Vec<Track>> {
    let uris = context_uris(session, &format!("spotify:playlist:{id}"), 20_000).await?;
    tracks(session, &uris).await
}

pub async fn saved_tracks(session: &Session) -> anyhow::Result<Vec<Track>> {
    let user = session.username();
    let uris = context_uris(session, &format!("spotify:user:{user}:collection"), 50_000).await?;
    tracks(session, &uris).await
}

pub async fn album_tracks(session: &Session, id: &str) -> anyhow::Result<Vec<Track>> {
    let uris = context_uris(session, &format!("spotify:album:{id}"), 2_000).await?;
    tracks(session, &uris).await
}

/// Track search. Albums are the distinct albums among the matching tracks; Spotify's client
/// search doesn't return playlists.
pub async fn search(session: &Session, query: &str, limit: usize) -> anyhow::Result<SearchResults> {
    let q: String = url::form_urlencoded::byte_serialize(query.trim().as_bytes()).collect();
    if q.is_empty() {
        return Ok(SearchResults::default());
    }
    let uris = context_uris(session, &format!("spotify:search:{q}"), limit).await?;
    let found = track_metadata(session, &uris).await?;
    Ok(SearchResults {
        albums: albums_of(&found),
        tracks: found
            .iter()
            .map(|(id, t)| to_track(id.clone(), t))
            .collect(),
        playlists: Vec::new(),
    })
}

/// Distinct albums of `tracks`, in order of first appearance.
fn albums_of(tracks: &[(String, metadata::Track)]) -> Vec<AlbumSummary> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for album in tracks.iter().filter_map(|(_, t)| t.album.as_ref()) {
        let Ok(id) = SpotifyId::from_raw(album.gid()).and_then(|id| id.to_base62()) else {
            continue;
        };
        if !seen.insert(id.clone()) {
            continue;
        }
        out.push(AlbumSummary {
            id,
            name: album.name().to_owned(),
            artist: album.artist.first().map(|a| a.name().to_owned()),
            year: album
                .date
                .as_ref()
                .and_then(|d| u16::try_from(d.year()).ok())
                .filter(|&y| y > 0),
            tracks: 0,
            cover: album_cover(album),
        });
    }
    out
}

fn json_headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    h
}

fn now_ms() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
        .to_string()
}

/// A playlist change list that appends (or prepends) `uris`, as the web player sends it.
fn add_items(uris: &[String], first: bool) -> serde_json::Value {
    let ts = now_ms();
    let items: Vec<_> = uris
        .iter()
        .map(|u| {
            serde_json::json!({
                "uri": u,
                "attributes": { "timestamp": ts, "formatAttributes": [], "availableSignals": [] }
            })
        })
        .collect();
    serde_json::json!({
        "deltas": [{
            "ops": [{ "kind": "ADD", "add": { "items": items, "addFirst": first, "addLast": !first } }],
            "info": { "source": { "client": "WEBPLAYER" } }
        }],
        "wantResultingRevisions": false,
        "wantSyncResult": false,
        "nonces": []
    })
}

async fn post_json(
    session: &Session,
    endpoint: &str,
    body: &serde_json::Value,
) -> anyhow::Result<Vec<u8>> {
    let body = serde_json::to_vec(body)?;
    let res = session
        .spclient()
        .request_as_json(
            &Method::POST,
            endpoint,
            Some(json_headers()),
            Some(std::str::from_utf8(&body)?),
        )
        .await?;
    Ok(res.to_vec())
}

/// Appends Spotify tracks (base62 ids) to a playlist. Returns how many were sent.
pub async fn add_to_playlist(
    session: &Session,
    playlist: &str,
    ids: &[String],
) -> anyhow::Result<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    let id = playlist_id(playlist)?.to_base62()?;
    let uris: Vec<String> = ids.iter().map(|id| format!("spotify:track:{id}")).collect();
    for chunk in uris.chunks(100) {
        post_json(
            session,
            &format!("/playlist/v2/playlist/{id}/changes"),
            &add_items(chunk, false),
        )
        .await
        .context("adding to the Spotify playlist")?;
    }
    Ok(ids.len())
}

/// Creates a playlist, puts it at the top of the account's list and returns its id.
pub async fn create_playlist(session: &Session, name: &str) -> anyhow::Result<String> {
    let body = serde_json::json!({
        "ops": [{
            "kind": "UPDATE_LIST_ATTRIBUTES",
            "updateListAttributes": {
                "newAttributes": {
                    "values": { "name": name, "formatAttributes": [], "pictureSize": [] },
                    "noValue": []
                }
            }
        }]
    });
    let reply = post_json(session, "/playlist/v2/playlist", &body)
        .await
        .context("creating the Spotify playlist")?;
    #[derive(serde::Deserialize)]
    struct Created {
        uri: String,
    }
    let uri = serde_json::from_slice::<Created>(&reply)
        .map(|c| c.uri)
        .or_else(|_| {
            // Some servers answer in protobuf even when asked for JSON.
            librespot_protocol::playlist4_external::CreateListReply::parse_from_bytes(&reply)
                .map(|r| r.uri().to_owned())
        })
        .context("reading Spotify's reply")?;
    if !uri.starts_with("spotify:playlist:") {
        bail!("Spotify returned an unexpected playlist id");
    }
    let user = session.username();
    post_json(
        session,
        &format!("/playlist/v2/user/{user}/rootlist/changes"),
        &add_items(std::slice::from_ref(&uri), true),
    )
    .await
    .context("adding the new playlist to your library")?;
    Ok(uri.trim_start_matches("spotify:playlist:").to_owned())
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

    fn image(id: u8, size: metadata::image::Size, width: i32) -> metadata::Image {
        let mut i = metadata::Image::new();
        i.set_file_id(vec![id; 20]);
        i.set_size(size);
        if width > 0 {
            i.set_width(width);
        }
        i
    }

    #[test]
    fn maps_track_metadata() {
        use metadata::image::Size;
        let mut album = metadata::Album::new();
        album.set_name("Album".into());
        let mut a = metadata::Artist::new();
        a.set_name("Artist".into());
        album.artist.push(a.clone());
        album.date.mut_or_insert_default().set_year(2013);
        let group = album.cover_group.mut_or_insert_default();
        group.image.push(image(1, Size::SMALL, 0));
        group.image.push(image(2, Size::DEFAULT, 0));
        group.image.push(image(3, Size::LARGE, 0));
        let mut t = metadata::Track::new();
        t.set_name("Song".into());
        t.artist.push(a);
        let mut feat = metadata::Artist::new();
        feat.set_name("Feat".into());
        t.artist.push(feat);
        t.set_number(3);
        t.set_disc_number(1);
        t.set_duration(200_000);
        t.album = protobuf::MessageField::some(album);

        let track = to_track("abc".into(), &t);
        assert_eq!(track.locator, TrackLocator::Spotify { id: "abc".into() });
        assert_eq!(track.artists, vec!["Artist", "Feat"]);
        assert_eq!((track.year, track.track_number), (Some(2013), Some(3)));
        assert_eq!(track.duration, Some(Duration::from_secs(200)));
        assert_eq!(track.album_artist.as_deref(), Some("Artist"));
        assert_eq!(
            track.cover,
            Some(CoverRef::Url(format!(
                "https://i.scdn.co/image/{}",
                "02".repeat(20)
            )))
        );
    }

    #[test]
    fn playlist_add_body_lists_track_uris() {
        let body = add_items(&["spotify:track:x".into(), "spotify:track:y".into()], false);
        let add = &body["deltas"][0]["ops"][0]["add"];
        assert_eq!(add["items"][1]["uri"], "spotify:track:y");
        assert_eq!(add["addLast"], true);
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
