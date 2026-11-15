//! Reading tags and embedded art with lofty.

use std::path::Path;

use khoostic_core::{Track, TrackLocator};
use lofty::picture::PictureType;
use lofty::prelude::*;
use lofty::tag::Tag;

pub struct TagRead {
    pub track: Track,
    /// Raw bytes of the embedded front cover (or the first picture if none is marked front).
    pub cover: Option<Vec<u8>>,
}

/// Reads a file's tags. Never fails on missing tags: the title falls back to the file name.
pub fn read(path: &Path) -> anyhow::Result<TagRead> {
    let file = lofty::read_from_path(path)?;
    let mut track = Track::from_path(path);
    track.duration = Some(file.properties().duration()).filter(|d| !d.is_zero());

    let tag = file.primary_tag().or_else(|| file.first_tag());
    let mut cover = None;
    if let Some(tag) = tag {
        fill(&mut track, tag);
        let pics = tag.pictures();
        cover = pics
            .iter()
            .find(|p| p.pic_type() == PictureType::CoverFront)
            .or_else(|| pics.first())
            .map(|p| p.data().to_vec());
    }
    // Some files carry art only in a secondary tag (e.g. ID3v2 next to APE).
    if cover.is_none() {
        cover = file
            .tags()
            .iter()
            .flat_map(|t| t.pictures())
            .next()
            .map(|p| p.data().to_vec());
    }
    track.locator = TrackLocator::Local(path.to_path_buf());
    Ok(TagRead { track, cover })
}

fn clean(s: impl AsRef<str>) -> Option<String> {
    let s = s.as_ref().trim().trim_matches('\0').trim();
    (!s.is_empty()).then(|| s.to_owned())
}

fn fill(track: &mut Track, tag: &Tag) {
    if let Some(t) = tag.title().and_then(clean) {
        track.title = t;
    }
    // Prefer the multi-value ARTISTS field; otherwise split the display string on the
    // separators taggers commonly use for multiple artists.
    let multi: Vec<String> = tag
        .get_strings(ItemKey::TrackArtists)
        .filter_map(clean)
        .collect();
    track.artists = if multi.len() > 1 {
        multi
    } else {
        tag.artist()
            .map(|a| a.split([';', '\0']).filter_map(clean).collect())
            .unwrap_or_default()
    };
    track.album = tag.album().and_then(clean);
    track.album_artist = tag.get_string(ItemKey::AlbumArtist).and_then(clean);
    track.track_number = tag.track().filter(|&n| n > 0);
    track.disc_number = tag.disk().filter(|&n| n > 0);
    track.year = tag.date().map(|d| d.year).filter(|&y| y > 0).or_else(|| {
        tag.get_string(ItemKey::Year)
            .and_then(|y| y.get(..4)?.parse().ok())
    });
    track.genre = tag.genre().and_then(clean);
}
