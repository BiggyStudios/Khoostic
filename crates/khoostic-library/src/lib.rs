//! Khoostic's music library: tags, cover art, a SQLite index with full-text search, scanning,
//! and playlists that mix local and streaming tracks.

pub mod covers;
pub mod db;
pub mod files;
pub mod scan;
pub mod tags;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::Context;
use crossbeam_channel::Receiver;
use khoostic_core::{CoverRef, Track};
use parking_lot::Mutex;
use rusqlite::Connection;

pub use covers::{CoverSize, CoverStore};
pub use db::{Album, Artist, Playlist, TrackOrder};
pub use scan::ScanStats;

#[derive(Debug, Clone)]
pub enum ScanEvent {
    Progress { done: usize, total: usize },
    Finished(ScanStats),
    Failed(String),
}

pub struct Library {
    conn: Mutex<Connection>,
    db_path: PathBuf,
    pub covers: CoverStore,
    scanning: AtomicBool,
    /// Bumped whenever the index changes, so views know to re-query.
    version: AtomicU64,
}

impl Library {
    /// Opens the library in the platform's data/cache directories.
    pub fn open_default() -> anyhow::Result<Arc<Self>> {
        let dirs =
            directories::ProjectDirs::from("", "", "khoostic").context("no home directory")?;
        std::fs::create_dir_all(dirs.data_local_dir())?;
        Self::open(
            &dirs.data_local_dir().join("library.db"),
            &dirs.cache_dir().join("covers"),
        )
    }

    pub fn open(db_path: &Path, cover_dir: &Path) -> anyhow::Result<Arc<Self>> {
        if let Some(dir) = db_path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let conn = db::open(db_path).with_context(|| format!("opening {}", db_path.display()))?;
        Ok(Arc::new(Self {
            conn: Mutex::new(conn),
            db_path: db_path.to_path_buf(),
            covers: CoverStore::new(cover_dir)?,
            scanning: AtomicBool::new(false),
            version: AtomicU64::new(0),
        }))
    }

    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    pub fn is_scanning(&self) -> bool {
        self.scanning.load(Ordering::Acquire)
    }

    /// Scans in a background thread (with its own connection, so reads stay responsive).
    /// Returns `None` if a scan is already running.
    pub fn start_scan(self: &Arc<Self>, dirs: Vec<PathBuf>) -> Option<Receiver<ScanEvent>> {
        if self.scanning.swap(true, Ordering::AcqRel) {
            return None;
        }
        let (tx, rx) = crossbeam_channel::unbounded();
        let lib = self.clone();
        std::thread::Builder::new()
            .name("khoostic-scan".into())
            .spawn(move || {
                let result = db::open(&lib.db_path).and_then(|mut conn| {
                    scan::scan(&mut conn, &lib.covers, &dirs, |done, total| {
                        let _ = tx.send(ScanEvent::Progress { done, total });
                    })
                });
                lib.version.fetch_add(1, Ordering::AcqRel);
                lib.scanning.store(false, Ordering::Release);
                let _ = tx.send(match result {
                    Ok(stats) => ScanEvent::Finished(stats),
                    Err(e) => ScanEvent::Failed(format!("{e:#}")),
                });
            })
            .expect("spawn scan thread");
        Some(rx)
    }

    pub fn scan_blocking(&self, dirs: &[PathBuf]) -> anyhow::Result<ScanStats> {
        let stats = scan::scan(&mut self.conn.lock(), &self.covers, dirs, |_, _| {})?;
        self.version.fetch_add(1, Ordering::AcqRel);
        Ok(stats)
    }

    /// Turns files/folders into tracks with real tags: from the index when known, otherwise
    /// by reading the file (embedded covers are cached either way).
    pub fn tracks_for_paths<P: AsRef<Path>>(&self, paths: &[P]) -> Vec<Track> {
        let files = files::collect_files(paths);
        let conn = self.conn.lock();
        files
            .into_iter()
            .map(|f| {
                if let Ok(Some(t)) = db::track_by_path(&conn, &f) {
                    return t;
                }
                match tags::read(&f) {
                    Ok(read) => {
                        let mut t = read.track;
                        t.cover = read
                            .cover
                            .and_then(|b| self.covers.store(&b).ok())
                            .map(CoverRef::Cached);
                        t
                    }
                    Err(_) => Track::from_path(&f),
                }
            })
            .collect()
    }

    pub fn cover_path(&self, cover: &CoverRef, size: CoverSize) -> Option<PathBuf> {
        match cover {
            CoverRef::Cached(key) => Some(self.covers.path(key, size)),
            CoverRef::Url(_) => None,
        }
    }

    pub fn all_tracks(&self, order: TrackOrder) -> Vec<Track> {
        db::all_tracks(&self.conn.lock(), order).unwrap_or_default()
    }

    pub fn albums(&self) -> Vec<Album> {
        db::albums(&self.conn.lock()).unwrap_or_default()
    }

    pub fn album_tracks(&self, album: &str, artist: Option<&str>) -> Vec<Track> {
        db::album_tracks(&self.conn.lock(), album, artist).unwrap_or_default()
    }

    pub fn artists(&self) -> Vec<Artist> {
        db::artists(&self.conn.lock()).unwrap_or_default()
    }

    pub fn artist_tracks(&self, artist: &str) -> Vec<Track> {
        db::artist_tracks(&self.conn.lock(), artist).unwrap_or_default()
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<Track> {
        db::search(&self.conn.lock(), query, limit).unwrap_or_default()
    }

    pub fn playlists(&self) -> Vec<Playlist> {
        db::playlists(&self.conn.lock()).unwrap_or_default()
    }

    pub fn playlist_tracks(&self, id: i64) -> Vec<Track> {
        db::playlist_tracks(&self.conn.lock(), id).unwrap_or_default()
    }

    pub fn create_playlist(&self, name: &str, tracks: &[Track]) -> anyhow::Result<i64> {
        let mut conn = self.conn.lock();
        let id = db::create_playlist(&conn, name, None)?;
        db::set_playlist_tracks(&mut conn, id, tracks)?;
        self.version.fetch_add(1, Ordering::AcqRel);
        Ok(id)
    }

    pub fn set_playlist_tracks(&self, id: i64, tracks: &[Track]) -> anyhow::Result<()> {
        db::set_playlist_tracks(&mut self.conn.lock(), id, tracks)?;
        self.version.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    pub fn delete_playlist(&self, id: i64) -> anyhow::Result<()> {
        db::delete_playlist(&self.conn.lock(), id)?;
        self.version.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}
