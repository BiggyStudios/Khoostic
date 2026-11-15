//! End-to-end library test on a generated folder tree.

use std::path::{Path, PathBuf};
use std::time::Duration;

use khoostic::library::{CoverSize, Library, TrackOrder};
use khoostic::model::{CoverRef, Track, TrackLocator};
use lofty::config::WriteOptions;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::prelude::*;
use lofty::tag::{Tag, TagType};

fn wav(path: &Path, seconds: f32) {
    let rate = 8000u32;
    let samples = (rate as f32 * seconds) as u32;
    let data_len = samples * 2;
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&1u16.to_le_bytes()); // mono
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    b.resize(b.len() + data_len as usize, 0);
    std::fs::write(path, b).unwrap();
}

fn png(rgb: [u8; 3]) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(300, 300, image::Rgb(rgb));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn tagged(path: &Path, title: &str, artist: &str, album: &str, n: u32, art: Option<Vec<u8>>) {
    wav(path, 0.5);
    let mut file = lofty::read_from_path(path).unwrap();
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title(title.into());
    tag.set_artist(artist.into());
    tag.set_album(album.into());
    tag.set_track(n);
    tag.insert_text(ItemKey::AlbumArtist, artist.into());
    tag.insert_text(ItemKey::RecordingDate, "2013".into());
    if let Some(bytes) = art {
        tag.push_picture(
            Picture::unchecked(bytes)
                .pic_type(PictureType::CoverFront)
                .mime_type(MimeType::Png)
                .build(),
        );
    }
    file.insert_tag(tag);
    file.save_to_path(path, WriteOptions::default()).unwrap();
}

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn scan_query_and_rescan() {
    let root = std::env::temp_dir().join(format!("khoostic-lib-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let fx = Fixture { root: root.clone() };
    let music = root.join("Music");
    let (ram, singles, folder_art) = (
        music.join("RAM"),
        music.join("Singles"),
        music.join("Folder Art"),
    );
    for d in [&ram, &singles, &folder_art] {
        std::fs::create_dir_all(d).unwrap();
    }

    // Album with embedded art on every track (same image → one cached cover).
    let art = png([200, 40, 40]);
    tagged(
        &ram.join("02.wav"),
        "Instant Crush",
        "Daft Punk",
        "Random Access Memories",
        2,
        Some(art.clone()),
    );
    tagged(
        &ram.join("01.wav"),
        "Give Life Back to Music",
        "Daft Punk",
        "Random Access Memories",
        1,
        Some(art.clone()),
    );
    tagged(
        &ram.join("10.wav"),
        "Doin' it Right",
        "Daft Punk",
        "Random Access Memories",
        10,
        Some(art),
    );
    // Album with only a folder image.
    tagged(
        &folder_art.join("a.wav"),
        "Alpha",
        "Folder Band",
        "Folder Album",
        1,
        None,
    );
    tagged(
        &folder_art.join("b.wav"),
        "Beta",
        "Folder Band",
        "Folder Album",
        2,
        None,
    );
    std::fs::write(folder_art.join("cover.png"), png([20, 200, 20])).unwrap();
    // Unrelated singles next to a stray Folder.jpg: must NOT get that art.
    tagged(
        &singles.join("x.wav"),
        "Song X",
        "Artist X",
        "Album X",
        1,
        None,
    );
    tagged(
        &singles.join("y.wav"),
        "Song Y",
        "Artist Y",
        "Album Y",
        1,
        None,
    );
    std::fs::write(singles.join("Folder.png"), png([0, 0, 200])).unwrap();
    // Untagged file falls back to its file name.
    wav(&music.join("loose track.wav"), 0.25);

    let lib = Library::open(&root.join("lib.db"), &root.join("covers")).unwrap();
    let stats = lib.scan_blocking(std::slice::from_ref(&music)).unwrap();
    assert_eq!(
        (
            stats.files,
            stats.added_or_updated,
            stats.removed,
            stats.failed
        ),
        (8, 8, 0, 0)
    );

    // Albums grouped and ordered, tracks in track-number order (not file order).
    let albums = lib.albums();
    let names: Vec<_> = albums.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Album X",
            "Album Y",
            "Random Access Memories",
            "Folder Album"
        ]
    );
    let ram_album = albums
        .iter()
        .find(|a| a.name == "Random Access Memories")
        .unwrap();
    assert_eq!((ram_album.tracks, ram_album.year), (3, Some(2013)));
    assert!((ram_album.duration.as_secs_f32() - 1.5).abs() < 0.05);
    let tracks = lib.album_tracks("random access memories", Some("Daft Punk"));
    let titles: Vec<_> = tracks.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(
        titles,
        ["Give Life Back to Music", "Instant Crush", "Doin' it Right"]
    );

    // Covers: embedded art deduplicated; folder art only for the single-album folder.
    let cover = |title: &str| {
        lib.all_tracks(TrackOrder::Title)
            .into_iter()
            .find(|t| t.title == title)
            .unwrap()
            .cover
    };
    let ram_cover = cover("Instant Crush").expect("embedded cover");
    assert_eq!(cover("Doin' it Right"), Some(ram_cover.clone()));
    let path = lib.cover_path(&ram_cover, CoverSize::Small).unwrap();
    assert_eq!(image::open(&path).unwrap().width(), 128);
    assert!(
        cover("Alpha").is_some(),
        "folder cover applied to a single-album folder"
    );
    assert_ne!(cover("Alpha"), Some(ram_cover));
    assert_eq!(
        cover("Song X"),
        None,
        "stray folder art must not apply to mixed folders"
    );
    assert_eq!(cover("loose track"), None);

    // Full-text, prefix, diacritic-insensitive search.
    let hits = lib.search("daf crus", 10);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].title, "Instant Crush");
    assert_eq!(lib.search("random", 10).len(), 3);
    assert!(lib.search("\"; DROP TABLE tracks; --", 10).is_empty());

    let artists = lib.artists();
    assert!(
        artists
            .iter()
            .any(|a| a.name == "Daft Punk" && a.tracks == 3 && a.albums == 1)
    );

    // Incremental: nothing changed → nothing re-read.
    let again = lib.scan_blocking(std::slice::from_ref(&music)).unwrap();
    assert_eq!((again.added_or_updated, again.removed), (0, 0));
    // A retagged file and a deleted file are picked up.
    std::thread::sleep(Duration::from_millis(1100)); // mtime has 1 s resolution
    tagged(
        &singles.join("x.wav"),
        "Song X (Remix)",
        "Artist X",
        "Album X",
        1,
        None,
    );
    std::fs::remove_file(singles.join("y.wav")).unwrap();
    let third = lib.scan_blocking(std::slice::from_ref(&music)).unwrap();
    assert_eq!((third.added_or_updated, third.removed), (1, 1));
    assert_eq!(lib.search("remix", 5)[0].title, "Song X (Remix)");
    assert!(lib.search("song y", 5).is_empty());

    // tracks_for_paths: indexed files come from the DB, unknown files are read on the spot.
    let outside = root.join("outside.wav");
    tagged(
        &outside,
        "Outsider",
        "Nobody",
        "Elsewhere",
        1,
        Some(png([9, 9, 9])),
    );
    let got = lib.tracks_for_paths(&[ram.clone(), outside.clone()]);
    let titles: Vec<_> = got.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(
        titles,
        [
            "Give Life Back to Music",
            "Instant Crush",
            "Doin' it Right",
            "Outsider"
        ]
    );
    assert!(matches!(got[3].cover, Some(CoverRef::Cached(_))));

    // Playlists mix local and Spotify tracks and keep order.
    let spotify = Track {
        locator: TrackLocator::Spotify {
            id: "4uLU6hMCjMI75M1A2tKUQC".into(),
        },
        title: "Never Gonna Give You Up".into(),
        artists: vec!["Rick Astley".into()],
        cover: Some(CoverRef::Url("https://i.scdn.co/image/x".into())),
        ..Default::default()
    };
    let id = lib
        .create_playlist("Mixed", &[got[1].clone(), spotify.clone(), got[0].clone()])
        .unwrap();
    let pl = lib.playlist_tracks(id);
    assert_eq!(pl.len(), 3);
    assert_eq!(pl[1].locator, spotify.locator);
    assert_eq!(pl[1].cover, spotify.cover);
    assert_eq!(pl[2].title, "Give Life Back to Music");
    assert_eq!(lib.playlists()[0].tracks, 3);
    lib.delete_playlist(id).unwrap();
    assert!(lib.playlists().is_empty());
    drop(fx);
}
