//! SQLite index: tracks (with an FTS5 search index), albums/artists derived by grouping,
//! and playlists that can mix sources.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::model::{CoverRef, Track, TrackLocator};

const SCHEMA_VERSION: i32 = 1;
/// Separator for multi-valued text columns (ASCII unit separator: never appears in tags).
const SEP: char = '\u{1f}';

pub fn open(path: &Path) -> anyhow::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;",
    )?;
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> anyhow::Result<()> {
    let version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version >= SCHEMA_VERSION {
        return Ok(());
    }
    conn.execute_batch(
        "BEGIN;
         CREATE TABLE IF NOT EXISTS tracks (
             id INTEGER PRIMARY KEY,
             path TEXT NOT NULL UNIQUE,
             mtime INTEGER NOT NULL,
             size INTEGER NOT NULL,
             title TEXT NOT NULL,
             artists TEXT NOT NULL,
             album TEXT,
             album_artist TEXT,
             primary_artist TEXT,
             track_no INTEGER,
             disc_no INTEGER,
             year INTEGER,
             genre TEXT,
             duration_ms INTEGER,
             cover TEXT,
             added_at INTEGER NOT NULL DEFAULT (unixepoch())
         );
         CREATE INDEX IF NOT EXISTS tracks_album ON tracks(album, primary_artist);
         CREATE INDEX IF NOT EXISTS tracks_artist ON tracks(primary_artist);

         CREATE VIRTUAL TABLE IF NOT EXISTS tracks_fts USING fts5(
             title, artists, album, album_artist,
             content = 'tracks', content_rowid = 'id',
             tokenize = 'unicode61 remove_diacritics 2'
         );
         CREATE TRIGGER IF NOT EXISTS tracks_ai AFTER INSERT ON tracks BEGIN
             INSERT INTO tracks_fts(rowid, title, artists, album, album_artist)
             VALUES (new.id, new.title, new.artists, new.album, new.album_artist);
         END;
         CREATE TRIGGER IF NOT EXISTS tracks_ad AFTER DELETE ON tracks BEGIN
             INSERT INTO tracks_fts(tracks_fts, rowid, title, artists, album, album_artist)
             VALUES ('delete', old.id, old.title, old.artists, old.album, old.album_artist);
         END;
         CREATE TRIGGER IF NOT EXISTS tracks_au AFTER UPDATE ON tracks BEGIN
             INSERT INTO tracks_fts(tracks_fts, rowid, title, artists, album, album_artist)
             VALUES ('delete', old.id, old.title, old.artists, old.album, old.album_artist);
             INSERT INTO tracks_fts(rowid, title, artists, album, album_artist)
             VALUES (new.id, new.title, new.artists, new.album, new.album_artist);
         END;

         CREATE TABLE IF NOT EXISTS playlists (
             id INTEGER PRIMARY KEY,
             name TEXT NOT NULL,
             spotify_id TEXT UNIQUE,
             created_at INTEGER NOT NULL DEFAULT (unixepoch()),
             updated_at INTEGER NOT NULL DEFAULT (unixepoch())
         );
         CREATE TABLE IF NOT EXISTS playlist_items (
             playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
             position INTEGER NOT NULL,
             uri TEXT NOT NULL,
             title TEXT NOT NULL,
             artists TEXT NOT NULL,
             album TEXT,
             duration_ms INTEGER,
             cover TEXT,
             PRIMARY KEY (playlist_id, position)
         );
         PRAGMA user_version = 1;
         COMMIT;",
    )?;
    Ok(())
}

pub(crate) fn join(v: &[String]) -> String {
    v.join(&SEP.to_string())
}

fn split(s: &str) -> Vec<String> {
    s.split(SEP)
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

fn cover_to_str(c: &Option<CoverRef>) -> Option<String> {
    match c {
        Some(CoverRef::Cached(k)) => Some(format!("cache:{k}")),
        Some(CoverRef::Url(u)) => Some(u.clone()),
        None => None,
    }
}

fn cover_from_str(s: Option<String>) -> Option<CoverRef> {
    let s = s?;
    Some(match s.strip_prefix("cache:") {
        Some(k) => CoverRef::Cached(k.to_owned()),
        None => CoverRef::Url(s),
    })
}

const TRACK_COLS: &str =
    "path, title, artists, album, album_artist, track_no, disc_no, year, genre, duration_ms, cover";

fn track_from_row(r: &Row) -> rusqlite::Result<Track> {
    let path: String = r.get(0)?;
    Ok(Track {
        locator: TrackLocator::Local(path.into()),
        title: r.get(1)?,
        artists: split(&r.get::<_, String>(2)?),
        album: r.get(3)?,
        album_artist: r.get(4)?,
        track_number: r.get(5)?,
        disc_number: r.get(6)?,
        year: r.get(7)?,
        genre: r.get(8)?,
        duration: r
            .get::<_, Option<i64>>(9)?
            .map(|ms| Duration::from_millis(ms.max(0) as u64)),
        cover: cover_from_str(r.get(10)?),
    })
}

pub struct FileStamp {
    pub mtime: i64,
    pub size: i64,
}

pub fn upsert_track(conn: &Connection, t: &Track, stamp: &FileStamp) -> rusqlite::Result<()> {
    let TrackLocator::Local(path) = &t.locator else {
        return Ok(());
    };
    conn.prepare_cached(
        "INSERT INTO tracks (path, mtime, size, title, artists, album, album_artist, primary_artist,
                             track_no, disc_no, year, genre, duration_ms, cover)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(path) DO UPDATE SET
             mtime = excluded.mtime, size = excluded.size, title = excluded.title,
             artists = excluded.artists, album = excluded.album,
             album_artist = excluded.album_artist, primary_artist = excluded.primary_artist,
             track_no = excluded.track_no, disc_no = excluded.disc_no, year = excluded.year,
             genre = excluded.genre, duration_ms = excluded.duration_ms, cover = excluded.cover",
    )?
    .execute(params![
        path.to_string_lossy(),
        stamp.mtime,
        stamp.size,
        t.title,
        join(&t.artists),
        t.album,
        t.album_artist,
        t.primary_artist(),
        t.track_number,
        t.disc_number,
        t.year,
        t.genre,
        t.duration.map(|d| d.as_millis() as i64),
        cover_to_str(&t.cover),
    ])?;
    Ok(())
}

/// Known files and their stamps, for incremental scans.
pub fn stamps(conn: &Connection) -> rusqlite::Result<Vec<(String, FileStamp)>> {
    let mut stmt = conn.prepare("SELECT path, mtime, size FROM tracks")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get(0)?,
            FileStamp {
                mtime: r.get(1)?,
                size: r.get(2)?,
            },
        ))
    })?;
    rows.collect()
}

pub fn delete_paths(conn: &Connection, paths: &[String]) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare_cached("DELETE FROM tracks WHERE path = ?1")?;
    for p in paths {
        stmt.execute([p])?;
    }
    Ok(())
}

pub fn track_by_path(conn: &Connection, path: &Path) -> rusqlite::Result<Option<Track>> {
    conn.prepare_cached(&format!("SELECT {TRACK_COLS} FROM tracks WHERE path = ?1"))?
        .query_row([path.to_string_lossy()], track_from_row)
        .optional()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackOrder {
    Artist,
    Title,
    RecentlyAdded,
}

pub fn all_tracks(conn: &Connection, order: TrackOrder) -> rusqlite::Result<Vec<Track>> {
    let order = match order {
        TrackOrder::Artist => {
            "primary_artist IS NULL, primary_artist COLLATE NOCASE, year, album COLLATE NOCASE, disc_no, track_no, title COLLATE NOCASE"
        }
        TrackOrder::Title => "title COLLATE NOCASE",
        TrackOrder::RecentlyAdded => "added_at DESC, path",
    };
    let mut stmt = conn.prepare(&format!("SELECT {TRACK_COLS} FROM tracks ORDER BY {order}"))?;
    stmt.query_map([], track_from_row)?.collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Album {
    pub name: String,
    pub artist: Option<String>,
    pub year: Option<u16>,
    pub tracks: u32,
    pub duration: Duration,
    pub cover: Option<CoverRef>,
}

pub fn albums(conn: &Connection) -> rusqlite::Result<Vec<Album>> {
    let mut stmt = conn.prepare(
        "SELECT album, primary_artist, MAX(year), COUNT(*), SUM(COALESCE(duration_ms, 0)),
                MAX(cover)
         FROM tracks WHERE album IS NOT NULL
         GROUP BY album COLLATE NOCASE, primary_artist COLLATE NOCASE
         ORDER BY primary_artist COLLATE NOCASE, MAX(year), album COLLATE NOCASE",
    )?;
    stmt.query_map([], |r| {
        Ok(Album {
            name: r.get(0)?,
            artist: r.get(1)?,
            year: r.get(2)?,
            tracks: r.get(3)?,
            duration: Duration::from_millis(r.get::<_, i64>(4)?.max(0) as u64),
            cover: cover_from_str(r.get(5)?),
        })
    })?
    .collect()
}

pub fn album_tracks(
    conn: &Connection,
    album: &str,
    artist: Option<&str>,
) -> rusqlite::Result<Vec<Track>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {TRACK_COLS} FROM tracks
         WHERE album = ?1 COLLATE NOCASE AND primary_artist IS ?2 COLLATE NOCASE
         ORDER BY disc_no, track_no, title COLLATE NOCASE"
    ))?;
    stmt.query_map(params![album, artist], track_from_row)?
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Artist {
    pub name: String,
    pub albums: u32,
    pub tracks: u32,
    pub cover: Option<CoverRef>,
}

pub fn artists(conn: &Connection) -> rusqlite::Result<Vec<Artist>> {
    let mut stmt = conn.prepare(
        "SELECT primary_artist, COUNT(DISTINCT album), COUNT(*), MAX(cover)
         FROM tracks WHERE primary_artist IS NOT NULL
         GROUP BY primary_artist COLLATE NOCASE
         ORDER BY primary_artist COLLATE NOCASE",
    )?;
    stmt.query_map([], |r| {
        Ok(Artist {
            name: r.get(0)?,
            albums: r.get(1)?,
            tracks: r.get(2)?,
            cover: cover_from_str(r.get(3)?),
        })
    })?
    .collect()
}

pub fn artist_tracks(conn: &Connection, artist: &str) -> rusqlite::Result<Vec<Track>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {TRACK_COLS} FROM tracks WHERE primary_artist = ?1 COLLATE NOCASE
         ORDER BY year, album COLLATE NOCASE, disc_no, track_no"
    ))?;
    stmt.query_map([artist], track_from_row)?.collect()
}

/// Full-text search with prefix matching on every word: "dai pun" finds "Daft Punk".
pub fn search(conn: &Connection, query: &str, limit: usize) -> rusqlite::Result<Vec<Track>> {
    let fts: Vec<String> = query
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\"*"))
        .collect();
    if fts.is_empty() {
        return Ok(Vec::new());
    }
    let cols = TRACK_COLS
        .split(", ")
        .map(|c| format!("t.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {cols} FROM tracks_fts f JOIN tracks t ON t.id = f.rowid
         WHERE tracks_fts MATCH ?1 ORDER BY bm25(tracks_fts, 10.0, 5.0, 3.0, 2.0) LIMIT ?2"
    ))?;
    stmt.query_map(params![fts.join(" AND "), limit as i64], track_from_row)?
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub spotify_id: Option<String>,
    pub tracks: u32,
}

pub fn playlists(conn: &Connection) -> rusqlite::Result<Vec<Playlist>> {
    let mut stmt = conn.prepare(
        "SELECT p.id, p.name, p.spotify_id, COUNT(i.uri) FROM playlists p
         LEFT JOIN playlist_items i ON i.playlist_id = p.id
         GROUP BY p.id ORDER BY p.name COLLATE NOCASE",
    )?;
    stmt.query_map([], |r| {
        Ok(Playlist {
            id: r.get(0)?,
            name: r.get(1)?,
            spotify_id: r.get(2)?,
            tracks: r.get(3)?,
        })
    })?
    .collect()
}

pub fn create_playlist(
    conn: &Connection,
    name: &str,
    spotify_id: Option<&str>,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO playlists (name, spotify_id) VALUES (?1, ?2)",
        params![name, spotify_id],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn delete_playlist(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM playlists WHERE id = ?1", [id])?;
    Ok(())
}

/// Replaces a playlist's contents.
pub fn set_playlist_tracks(
    conn: &mut Connection,
    id: i64,
    tracks: &[Track],
) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM playlist_items WHERE playlist_id = ?1", [id])?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO playlist_items (playlist_id, position, uri, title, artists, album, duration_ms, cover)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for (i, t) in tracks.iter().enumerate() {
            stmt.execute(params![
                id,
                i as i64,
                t.locator.to_uri(),
                t.title,
                join(&t.artists),
                t.album,
                t.duration.map(|d| d.as_millis() as i64),
                cover_to_str(&t.cover),
            ])?;
        }
    }
    tx.execute(
        "UPDATE playlists SET updated_at = unixepoch() WHERE id = ?1",
        [id],
    )?;
    tx.commit()
}

pub fn playlist_tracks(conn: &Connection, id: i64) -> rusqlite::Result<Vec<Track>> {
    let mut stmt = conn.prepare_cached(
        "SELECT uri, title, artists, album, duration_ms, cover FROM playlist_items
         WHERE playlist_id = ?1 ORDER BY position",
    )?;
    let rows = stmt.query_map([id], |r| {
        let uri: String = r.get(0)?;
        Ok(TrackLocator::from_uri(&uri).map(|locator| Track {
            locator,
            title: r.get(1).unwrap_or_default(),
            artists: split(&r.get::<_, String>(2).unwrap_or_default()),
            album: r.get(3).ok().flatten(),
            duration: r
                .get::<_, Option<i64>>(4)
                .ok()
                .flatten()
                .map(|ms| Duration::from_millis(ms.max(0) as u64)),
            cover: cover_from_str(r.get(5).ok().flatten()),
            ..Default::default()
        }))
    })?;
    Ok(rows.filter_map(|r| r.ok().flatten()).collect())
}
