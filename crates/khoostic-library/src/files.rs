//! Finding playable files on disk.

use std::path::{Path, PathBuf};

use khoostic_audio::is_supported_path;
use khoostic_core::Track;
use khoostic_core::sort::natural_cmp;

/// Expands folders recursively (sorted, so albums stay in order) and keeps decodable files.
pub fn collect_tracks<P: AsRef<Path>>(paths: &[P]) -> Vec<Track> {
    collect_files(paths)
        .into_iter()
        .map(Track::from_path)
        .collect()
}

pub fn collect_files<P: AsRef<Path>>(paths: &[P]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for p in paths {
        let p = p.as_ref();
        if p.is_dir() {
            let mut found: Vec<PathBuf> = walkdir::WalkDir::new(p)
                .follow_links(true)
                .sort_by(|a, b| {
                    natural_cmp(
                        &a.file_name().to_string_lossy(),
                        &b.file_name().to_string_lossy(),
                    )
                })
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_file() && is_supported_path(e.path()))
                .map(|e| e.into_path())
                .collect();
            files.append(&mut found);
        } else if p.is_file() && is_supported_path(p) {
            files.push(p.to_path_buf());
        }
    }
    files
}
