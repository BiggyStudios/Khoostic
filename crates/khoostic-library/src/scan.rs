//! Incremental library scan: only new or changed files (by mtime + size) are re-read, tags are
//! read in parallel, and files that disappeared are dropped from the index.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, UNIX_EPOCH};

use khoostic_core::{CoverRef, Track, TrackLocator};
use rusqlite::Connection;

use crate::covers::{CoverStore, find_folder_cover};
use crate::db::{self, FileStamp};
use crate::{files, tags};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanStats {
    pub files: usize,
    pub added_or_updated: usize,
    pub removed: usize,
    pub failed: usize,
    pub elapsed: Duration,
}

fn stamp(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some(FileStamp {
        mtime,
        size: meta.len() as i64,
    })
}

pub fn scan(
    conn: &mut Connection,
    covers: &CoverStore,
    dirs: &[PathBuf],
    mut progress: impl FnMut(usize, usize),
) -> anyhow::Result<ScanStats> {
    let started = Instant::now();
    let on_disk: Vec<(PathBuf, FileStamp)> = files::collect_files(dirs)
        .into_iter()
        .filter_map(|p| stamp(&p).map(|s| (p, s)))
        .collect();
    let known: HashMap<String, FileStamp> = db::stamps(conn)?.into_iter().collect();

    let present: HashSet<String> = on_disk
        .iter()
        .map(|(p, _)| p.to_string_lossy().into_owned())
        .collect();
    let removed: Vec<String> = known
        .keys()
        .filter(|k| !present.contains(*k))
        .cloned()
        .collect();
    let todo: Vec<(PathBuf, FileStamp)> = on_disk
        .into_iter()
        .filter(|(p, s)| {
            known
                .get(p.to_string_lossy().as_ref())
                .is_none_or(|k| k.mtime != s.mtime || k.size != s.size)
        })
        .collect();

    let total = todo.len();
    progress(0, total);
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<(Track, bool)>>> = Mutex::new((0..total).map(|_| None).collect());
    let workers = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .clamp(2, 8);

    // Tag reading and cover decoding are the slow parts: spread them over threads.
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((path, _)) = todo.get(i) else { break };
                    let read = match tags::read(path) {
                        Ok(read) => {
                            let mut track = read.track;
                            track.cover = read
                                .cover
                                .and_then(|bytes| covers.store(&bytes).ok())
                                .map(CoverRef::Cached);
                            Some((track, true))
                        }
                        Err(e) => {
                            tracing::warn!("{}: {e}", path.display());
                            Some((Track::from_path(path), false))
                        }
                    };
                    results.lock().expect("no panics hold this")[i] = read;
                    done.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
        // Report progress from the scanning thread while workers run.
        while done.load(Ordering::Relaxed) < total {
            progress(done.load(Ordering::Relaxed), total);
            std::thread::sleep(Duration::from_millis(100));
        }
    });

    let mut tracks: Vec<(Track, bool)> = results
        .into_inner()
        .expect("workers joined")
        .into_iter()
        .flatten()
        .collect();
    apply_folder_covers(conn, covers, &mut tracks);

    let tx = conn.transaction()?;
    let mut failed = 0;
    for ((track, ok), (_, stamp)) in tracks.iter().zip(&todo) {
        failed += usize::from(!ok);
        db::upsert_track(&tx, track, stamp)?;
    }
    db::delete_paths(&tx, &removed)?;
    tx.commit()?;
    progress(total, total);

    Ok(ScanStats {
        files: present.len(),
        added_or_updated: total,
        removed: removed.len(),
        failed,
        elapsed: started.elapsed(),
    })
}

/// Tracks without embedded art get the folder's cover image, but only when everything in the
/// folder belongs to one album — a loose `Folder.jpg` in a folder of singles would be wrong.
fn apply_folder_covers(conn: &Connection, covers: &CoverStore, tracks: &mut [(Track, bool)]) {
    let mut by_dir: HashMap<PathBuf, Vec<usize>> = HashMap::new();
    for (i, (t, _)) in tracks.iter().enumerate() {
        if let TrackLocator::Local(p) = &t.locator
            && let Some(dir) = p.parent()
        {
            by_dir.entry(dir.to_path_buf()).or_default().push(i);
        }
    }
    for (dir, idxs) in by_dir {
        if idxs.iter().all(|&i| tracks[i].0.cover.is_some()) {
            continue;
        }
        // Albums of every audio file in the folder: freshly read ones plus already indexed ones.
        let mut albums: HashSet<Option<String>> = idxs
            .iter()
            .map(|&i| tracks[i].0.album.as_ref().map(|a| a.to_lowercase()))
            .collect();
        for f in files::collect_files(&[&dir]) {
            if f.parent() != Some(dir.as_path()) {
                continue;
            }
            if let Ok(Some(t)) = db::track_by_path(conn, &f) {
                albums.insert(t.album.map(|a| a.to_lowercase()));
            }
        }
        if albums.len() != 1 || albums.contains(&None) {
            continue;
        }
        let Some(key) = find_folder_cover(&dir).and_then(|img| covers.store_file(&img).ok()) else {
            continue;
        };
        for i in idxs {
            tracks[i]
                .0
                .cover
                .get_or_insert_with(|| CoverRef::Cached(key.clone()));
        }
    }
}
