//! Your Spotify collection: liked songs and followed artists, read and changed through the
//! collection service (`/collection/v2`). librespot doesn't compile these messages, and they
//! are tiny, so they are encoded by hand here.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail};
use http::header::{ACCEPT, CONTENT_TYPE, HeaderValue};
use http::{HeaderMap, Method};
use librespot_core::session::Session;

const PROTO: &str = "application/vnd.collection-v2.spotify.proto";
/// Items per page; the service caps pages anyway.
const PAGE: u64 = 1000;

/// Which list in the collection: liked songs (and saved albums) or followed artists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Set {
    Collection,
    Artist,
}

impl Set {
    fn name(self) -> &'static str {
        match self {
            Set::Collection => "collection",
            Set::Artist => "artist",
        }
    }
}

fn headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(CONTENT_TYPE, HeaderValue::from_static(PROTO));
    h.insert(ACCEPT, HeaderValue::from_static(PROTO));
    h
}

/// Every uri in `set`, newest first.
pub async fn list(session: &Session, set: Set) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut token = String::new();
    for _ in 0..200 {
        let body = page_request(&session.username(), set.name(), &token, PAGE);
        let res = session
            .spclient()
            .request(
                &Method::POST,
                "/collection/v2/paging",
                Some(headers()),
                Some(&body),
            )
            .await
            .context("reading your Spotify library")?;
        let (items, next) = parse_page(&res)?;
        out.extend(items);
        if next.is_empty() {
            break;
        }
        token = next;
    }
    Ok(out)
}

/// Adds (`keep`) or removes uris in `set`: liking songs, following artists.
pub async fn write(session: &Session, set: Set, uris: &[String], keep: bool) -> anyhow::Result<()> {
    if uris.is_empty() {
        return Ok(());
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let body = write_request(&session.username(), set.name(), uris, now, !keep);
    session
        .spclient()
        .request(
            &Method::POST,
            "/collection/v2/write",
            Some(headers()),
            Some(&body),
        )
        .await
        .context("updating your Spotify library")?;
    Ok(())
}

// --- protobuf, by hand -------------------------------------------------------------------

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn bytes_field(out: &mut Vec<u8>, field: u32, b: &[u8]) {
    varint(out, u64::from(field) << 3 | 2);
    varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

fn varint_field(out: &mut Vec<u8>, field: u32, v: u64) {
    varint(out, u64::from(field) << 3);
    varint(out, v);
}

/// `PageRequest { username = 1; set = 2; pagination_token = 3; limit = 4 }`
fn page_request(user: &str, set: &str, token: &str, limit: u64) -> Vec<u8> {
    let mut b = Vec::new();
    bytes_field(&mut b, 1, user.as_bytes());
    bytes_field(&mut b, 2, set.as_bytes());
    if !token.is_empty() {
        bytes_field(&mut b, 3, token.as_bytes());
    }
    varint_field(&mut b, 4, limit);
    b
}

/// `WriteRequest { username = 1; set = 2; repeated CollectionItem items = 3 }` with
/// `CollectionItem { uri = 1; added_at = 2; is_removed = 3 }`.
fn write_request(user: &str, set: &str, uris: &[String], now: u64, removed: bool) -> Vec<u8> {
    let mut b = Vec::new();
    bytes_field(&mut b, 1, user.as_bytes());
    bytes_field(&mut b, 2, set.as_bytes());
    for uri in uris {
        let mut item = Vec::new();
        bytes_field(&mut item, 1, uri.as_bytes());
        varint_field(&mut item, 2, now);
        if removed {
            varint_field(&mut item, 3, 1);
        }
        bytes_field(&mut b, 3, &item);
    }
    b
}

/// Reads one field: `(number, wire value)`, advancing `pos`.
enum Wire<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
}

fn read_varint(b: &[u8], pos: &mut usize) -> anyhow::Result<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let Some(&byte) = b.get(*pos) else {
            bail!("truncated reply")
        };
        *pos += 1;
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(v);
        }
    }
    bail!("bad varint")
}

fn read_field<'a>(b: &'a [u8], pos: &mut usize) -> anyhow::Result<(u64, Wire<'a>)> {
    let key = read_varint(b, pos)?;
    let wire = match key & 7 {
        0 => Wire::Varint(read_varint(b, pos)?),
        2 => {
            let len = read_varint(b, pos)? as usize;
            let end = pos.checked_add(len).filter(|&e| e <= b.len());
            let Some(end) = end else {
                bail!("truncated reply")
            };
            let s = &b[*pos..end];
            *pos = end;
            Wire::Bytes(s)
        }
        1 => {
            *pos += 8;
            Wire::Varint(0)
        }
        5 => {
            *pos += 4;
            Wire::Varint(0)
        }
        w => bail!("unexpected wire type {w}"),
    };
    Ok((key >> 3, wire))
}

/// `PageResponse { repeated CollectionItem items = 1; next_page_token = 2 }`: the uris that
/// aren't marked removed, and the next page's token ("" at the end).
fn parse_page(b: &[u8]) -> anyhow::Result<(Vec<String>, String)> {
    let (mut items, mut next) = (Vec::new(), String::new());
    let mut pos = 0;
    while pos < b.len() {
        match read_field(b, &mut pos)? {
            (1, Wire::Bytes(item)) => {
                let (mut uri, mut removed) = (String::new(), false);
                let mut p = 0;
                while p < item.len() {
                    match read_field(item, &mut p)? {
                        (1, Wire::Bytes(u)) => uri = String::from_utf8_lossy(u).into_owned(),
                        (3, Wire::Varint(v)) => removed = v != 0,
                        _ => {}
                    }
                }
                if !removed && !uri.is_empty() {
                    items.push(uri);
                }
            }
            (2, Wire::Bytes(t)) => next = String::from_utf8_lossy(t).into_owned(),
            _ => {}
        }
    }
    Ok((items, next))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_request_bytes() {
        // Same bytes the service accepted in a live check.
        assert_eq!(
            page_request("u", "artist", "", 20),
            b"\x0a\x01u\x12\x06artist\x20\x14".to_vec()
        );
    }

    #[test]
    fn write_then_parse_round_trip() {
        let uris = vec!["spotify:track:a".to_string(), "spotify:track:b".to_string()];
        let w = write_request("me", "collection", &uris, 1_700_000_000, false);
        // Reuse the item encoding as a page reply: items are field 3 in a write, 1 in a page.
        let mut page = Vec::new();
        let mut pos = 0;
        while pos < w.len() {
            if let (3, Wire::Bytes(item)) = read_field(&w, &mut pos).unwrap() {
                bytes_field(&mut page, 1, item);
            }
        }
        bytes_field(&mut page, 2, b"next");
        let (items, next) = parse_page(&page).unwrap();
        assert_eq!(items, uris);
        assert_eq!(next, "next");
    }

    #[test]
    fn removed_items_are_skipped() {
        let w = write_request("me", "artist", &["spotify:artist:x".into()], 1, true);
        let mut page = Vec::new();
        let mut pos = 0;
        while pos < w.len() {
            if let (3, Wire::Bytes(item)) = read_field(&w, &mut pos).unwrap() {
                bytes_field(&mut page, 1, item);
            }
        }
        assert!(parse_page(&page).unwrap().0.is_empty());
    }
}
