//! cargo run -p khoostic-library --example scan -- <music dir> <scratch dir>

use khoostic_library::{Library, TrackOrder};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let music = std::path::PathBuf::from(args.next().expect("music dir"));
    let scratch = std::path::PathBuf::from(args.next().expect("scratch dir"));
    let lib = Library::open(&scratch.join("lib.db"), &scratch.join("covers"))?;
    let stats = lib.scan_blocking(std::slice::from_ref(&music))?;
    println!("first scan: {stats:?}");
    println!("rescan:     {:?}", lib.scan_blocking(&[music])?);
    for t in lib.all_tracks(TrackOrder::Artist) {
        println!(
            "{:<38.38} | {:<26.26} | {:<28.28} | {:>5} | {}",
            t.title,
            t.artist_line(),
            t.album.as_deref().unwrap_or("-"),
            t.duration
                .map(|d| format!("{}:{:02}", d.as_secs() / 60, d.as_secs() % 60))
                .unwrap_or_default(),
            if t.cover.is_some() { "cover" } else { "-" }
        );
    }
    println!(
        "albums: {}  artists: {}",
        lib.albums().len(),
        lib.artists().len()
    );
    Ok(())
}
