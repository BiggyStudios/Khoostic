//! Finding playable files on disk.

use std::path::{Path, PathBuf};

use khoostic_audio::is_supported_path;
use khoostic_core::Track;

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

/// Case-insensitive comparison that orders embedded numbers numerically ("2" < "10").
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = a.peek().copied().filter(char::is_ascii_digit) {
                    na.push(c);
                    a.next();
                }
                let mut nb = String::new();
                while let Some(c) = b.peek().copied().filter(char::is_ascii_digit) {
                    nb.push(c);
                    b.next();
                }
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord.is_ne() {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord.is_ne() {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::natural_cmp;

    #[test]
    fn natural_order() {
        let mut v = vec!["track10.flac", "Track2.flac", "track1.flac", "b", "A"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["A", "b", "track1.flac", "Track2.flac", "track10.flac"]);
    }
}
