//! Filesystem browser: folders first, then playable files, naturally sorted.
//! (Stands in for the library views until the library database lands.)

use std::path::{Path, PathBuf};

use khoostic_audio::is_supported_path;
use khoostic_player::scan::natural_cmp;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    Audio,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub kind: EntryKind,
}

pub struct Browser {
    dir: PathBuf,
    entries: Vec<Entry>,
    error: Option<String>,
}

impl Browser {
    pub fn new(start: PathBuf) -> Self {
        let mut b = Self {
            dir: start.clone(),
            entries: Vec::new(),
            error: None,
        };
        b.load(start);
        b
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Opens `dir`. On failure the browser stays where it was and remembers the error.
    pub fn open(&mut self, dir: &Path) -> Result<(), String> {
        match read_dir(dir) {
            Ok(entries) => {
                self.dir = dir.to_path_buf();
                self.entries = entries;
                self.error = None;
                Ok(())
            }
            Err(e) => Err(format!("{}: {e}", dir.display())),
        }
    }

    /// Goes up one level. Returns the index of the folder we came from, to keep it selected.
    pub fn parent(&mut self) -> Option<usize> {
        let child = self.dir.clone();
        let parent = child.parent()?.to_path_buf();
        self.open(&parent).ok()?;
        self.entries.iter().position(|e| e.path == child)
    }

    pub fn refresh(&mut self) {
        let dir = self.dir.clone();
        self.load(dir);
    }

    fn load(&mut self, dir: PathBuf) {
        match read_dir(&dir) {
            Ok(e) => {
                self.entries = e;
                self.error = None;
            }
            Err(e) => {
                self.entries.clear();
                self.error = Some(format!("{}: {e}", dir.display()));
            }
        }
        self.dir = dir;
    }
}

fn read_dir(dir: &Path) -> std::io::Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter_map(|e| {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            // Follow symlinks so linked music folders behave like real ones.
            let kind = if path.is_dir() {
                EntryKind::Dir
            } else if is_supported_path(&path) {
                EntryKind::Audio
            } else {
                return None;
            };
            Some(Entry { path, name, kind })
        })
        .collect();
    entries.sort_by(|a, b| {
        (a.kind != EntryKind::Dir)
            .cmp(&(b.kind != EntryKind::Dir))
            .then_with(|| natural_cmp(&a.name, &b.name))
    });
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_dirs_first_and_only_audio() {
        let root = std::env::temp_dir().join(format!("khoostic-browser-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Album 10")).unwrap();
        std::fs::create_dir_all(root.join("Album 2")).unwrap();
        for f in ["b.flac", "a.mp3", "cover.jpg", ".hidden.flac", "notes.txt"] {
            std::fs::write(root.join(f), b"").unwrap();
        }
        let mut b = Browser::new(root.join("Album 2"));
        assert!(b.entries().is_empty());
        assert_eq!(b.parent(), Some(0));
        let names: Vec<_> = b.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Album 2", "Album 10", "a.mp3", "b.flac"]);
        assert!(b.open(&root.join("missing")).is_err());
        assert_eq!(b.dir(), root);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
