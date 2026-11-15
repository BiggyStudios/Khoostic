//! Library views for the TUI: albums (with an open-album page) and songs, plus half-block
//! cover art rendering.

use std::collections::HashMap;
use std::path::Path;

use khoostic_core::{CoverRef, Track};
use khoostic_library::{Album, CoverSize, Library, TrackOrder};
use ratatui::style::Color;

use crate::app::ListView;

/// Half-block cells (top pixel, bottom pixel), cached per (cover, width, height).
type CoverCells = HashMap<(String, u16, u16), Option<Vec<(Color, Color)>>>;

#[derive(Default)]
pub struct LibState {
    version: Option<u64>,
    pub albums: Vec<Album>,
    pub songs: Vec<Track>,
    /// The album page currently open in the Albums view, with its tracks.
    pub open: Option<(Album, Vec<Track>)>,
    pub albums_view: ListView,
    pub album_view: ListView,
    pub songs_view: ListView,
    covers: CoverCells,
}

impl LibState {
    /// Re-queries the library when it changed (e.g. a background scan finished).
    pub fn refresh(&mut self, library: &Library) {
        let v = library.version();
        if self.version == Some(v) {
            return;
        }
        self.version = Some(v);
        self.albums = library.albums();
        self.songs = library.all_tracks(TrackOrder::Artist);
        if let Some((al, _)) = &self.open {
            let tracks = library.album_tracks(&al.name, al.artist.as_deref());
            let al = al.clone();
            self.open = Some((al, tracks));
        }
    }

    pub fn open_album(&mut self, library: &Library, index: usize) {
        if let Some(al) = self.albums.get(index).cloned() {
            let tracks = library.album_tracks(&al.name, al.artist.as_deref());
            self.open = Some((al, tracks));
            self.album_view.selected = 0;
            self.album_view.offset = 0;
        }
    }

    /// Cover art downsampled to `w`×`h` terminal cells, two pixels per cell (`▀` with
    /// foreground = top pixel, background = bottom pixel). Cached per size.
    pub fn cover_cells(
        &mut self,
        library: &Library,
        cover: Option<&CoverRef>,
        w: u16,
        h: u16,
    ) -> Option<&[(Color, Color)]> {
        let cover = cover?;
        let key = match cover {
            CoverRef::Cached(k) => k.clone(),
            CoverRef::Url(u) => u.clone(),
        };
        self.covers
            .entry((key, w, h))
            .or_insert_with(|| {
                let path = library.cover_path(cover, CoverSize::Small)?;
                half_blocks(&path, w, h)
            })
            .as_deref()
    }
}

fn half_blocks(path: &Path, w: u16, h: u16) -> Option<Vec<(Color, Color)>> {
    let img = image::open(path).ok()?.to_rgb8();
    let px = image::imageops::resize(
        &img,
        w as u32,
        h as u32 * 2,
        image::imageops::FilterType::Triangle,
    );
    let mut cells = Vec::with_capacity(w as usize * h as usize);
    for y in 0..h as u32 {
        for x in 0..w as u32 {
            let t = px.get_pixel(x, y * 2).0;
            let b = px.get_pixel(x, y * 2 + 1).0;
            cells.push((Color::Rgb(t[0], t[1], t[2]), Color::Rgb(b[0], b[1], b[2])));
        }
    }
    Some(cells)
}
