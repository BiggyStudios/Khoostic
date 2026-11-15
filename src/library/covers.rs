//! Cover art cache: images are decoded once, resized, and stored by content hash, so an album's
//! twelve identical embedded covers become one file.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use image::imageops::FilterType;
use parking_lot::Mutex;

/// Thumbnail sizes. Lists use `Small`, the now-playing view uses `Large`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoverSize {
    Small,
    Large,
}

impl CoverSize {
    fn px(self) -> u32 {
        match self {
            Self::Small => 128,
            Self::Large => 640,
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::Small => "s",
            Self::Large => "l",
        }
    }
}

#[derive(Clone, Debug)]
pub struct CoverStore {
    dir: PathBuf,
    /// Keys stored (or being stored) this session: skips re-decoding the same art when a
    /// whole album is scanned in parallel.
    seen: Arc<Mutex<HashSet<String>>>,
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Image files that commonly hold a folder's album art, best candidates first.
const FOLDER_NAMES: &[&str] = &[
    "cover",
    "folder",
    "front",
    "album",
    "albumart",
    "albumartlarge",
];
const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp"];

impl CoverStore {
    pub fn new(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            seen: Arc::default(),
        })
    }

    pub fn path(&self, key: &str, size: CoverSize) -> PathBuf {
        self.dir.join(format!("{key}-{}.jpg", size.suffix()))
    }

    /// Stores image bytes (any common format) and returns the cache key.
    /// Already-cached images are not decoded again.
    pub fn store(&self, bytes: &[u8]) -> anyhow::Result<String> {
        let key = format!("{:016x}", fnv1a(bytes));
        if !self.seen.lock().insert(key.clone()) {
            return Ok(key);
        }
        if self.path(&key, CoverSize::Small).exists() && self.path(&key, CoverSize::Large).exists()
        {
            return Ok(key);
        }
        let result = self.write(&key, bytes);
        if result.is_err() {
            self.seen.lock().remove(&key);
        }
        result.map(|()| key)
    }

    fn write(&self, key: &str, bytes: &[u8]) -> anyhow::Result<()> {
        let img = image::load_from_memory(bytes)?.to_rgb8();
        for size in [CoverSize::Small, CoverSize::Large] {
            let px = size.px();
            let thumb = if img.width() > px || img.height() > px {
                image::imageops::resize(
                    &img,
                    px,
                    px * img.height() / img.width().max(1),
                    FilterType::Lanczos3,
                )
            } else {
                img.clone()
            };
            let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let tmp = self.dir.join(format!(
                "{key}-{}.{}-{n}.tmp",
                size.suffix(),
                std::process::id()
            ));
            let mut out = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 88)
                .encode_image(&thumb)?;
            drop(out);
            // Atomic rename: readers never see a half-written file.
            std::fs::rename(&tmp, self.path(key, size))?;
        }
        Ok(())
    }

    pub fn store_file(&self, path: &Path) -> anyhow::Result<String> {
        self.store(&std::fs::read(path)?)
    }

    /// A remote image (e.g. Spotify album art), downloaded once with `fetch` and kept on disk.
    pub fn cached_url(
        &self,
        url: &str,
        fetch: impl FnOnce(&str) -> anyhow::Result<Vec<u8>>,
    ) -> Option<PathBuf> {
        let path = self
            .dir
            .join(format!("url-{:016x}.img", fnv1a(url.as_bytes())));
        if path.exists() {
            return Some(path);
        }
        let bytes = fetch(url)
            .map_err(|e| tracing::debug!("cover {url}: {e:#}"))
            .ok()?;
        let tmp = path.with_extension(format!(
            "{}.tmp",
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&tmp, bytes).ok()?;
        std::fs::rename(&tmp, &path).ok()?;
        Some(path)
    }
}

/// Finds a cover image in `dir` by conventional file names.
pub fn find_folder_cover(dir: &Path) -> Option<PathBuf> {
    let entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| Some(e.ok()?.path()))
        .collect();
    let stem_ext = |p: &Path| {
        let stem = p.file_stem()?.to_str()?.to_ascii_lowercase();
        let ext = p.extension()?.to_str()?.to_ascii_lowercase();
        Some((stem, ext))
    };
    let mut best: Option<(usize, PathBuf)> = None;
    for p in entries {
        let Some((stem, ext)) = stem_ext(&p) else {
            continue;
        };
        if !IMAGE_EXTS.contains(&ext.as_str()) {
            continue;
        }
        if let Some(rank) = FOLDER_NAMES
            .iter()
            .position(|n| stem == *n || stem.starts_with(&format!("{n}_")))
            && best.as_ref().is_none_or(|(r, _)| rank < *r)
        {
            best = Some((rank, p));
        }
    }
    best.map(|(_, p)| p)
}

/// 64-bit FNV-1a: stable across Rust versions (unlike `DefaultHasher`), fast enough for images.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn png_bytes(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb(rgb));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn store_dedupes_and_resizes() {
        let dir = std::env::temp_dir().join(format!("khoostic-covers-{}", std::process::id()));
        let store = CoverStore::new(&dir).unwrap();
        let bytes = png_bytes(1000, 500, [200, 30, 30]);
        let a = store.store(&bytes).unwrap();
        let b = store.store(&bytes).unwrap();
        assert_eq!(a, b);
        let small = image::open(store.path(&a, CoverSize::Small)).unwrap();
        assert_eq!((small.width(), small.height()), (128, 64));
        let large = image::open(store.path(&a, CoverSize::Large)).unwrap();
        assert_eq!(large.width(), 640);
        assert!(store.store(b"not an image").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn folder_cover_preference() {
        let dir = std::env::temp_dir().join(format!("khoostic-folder-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["Folder.JPG", "cover.png", "scan_back.jpg", "notes.txt"] {
            std::fs::write(dir.join(f), b"").unwrap();
        }
        assert_eq!(
            find_folder_cover(&dir).unwrap().file_name().unwrap(),
            "cover.png"
        );
        std::fs::remove_file(dir.join("cover.png")).unwrap();
        assert_eq!(
            find_folder_cover(&dir).unwrap().file_name().unwrap(),
            "Folder.JPG"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
