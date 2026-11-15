//! Renders the real GUI offscreen with scripted input and saves PNGs (volume 0).
//!
//! cargo run -p khoostic-gui --example screens -- <music folder> <out dir>

use std::time::Duration;

use khoostic_audio::EngineConfig;
use khoostic_config::Config;
use khoostic_gui::Headless;
use khoostic_gui::ui::{Input, Key, KeyPress};
use khoostic_library::Library;
use khoostic_player::{Player, PlayerCommand, Resolvers};

fn save(path: &str, size: (u32, u32), px: &[u8]) -> anyhow::Result<()> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, size.0, size.1);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(px)?;
    println!("wrote {path}");
    Ok(())
}

fn key(k: Key) -> Input {
    Input {
        keys: vec![KeyPress {
            key: k,
            shift: false,
            ctrl: false,
        }],
        ..Default::default()
    }
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let music = args.next().expect("music folder");
    let out = args.next().expect("output dir");
    let mut config = Config::default();
    config.library.music_dirs = vec![music.clone().into()];
    let player = Player::start(
        EngineConfig {
            volume: 0.0,
            ..Default::default()
        },
        Resolvers::with_defaults(),
    )?;
    let tmp = std::env::temp_dir().join("khoostic-example-lib");
    let library = Library::open(&tmp.join("lib.db"), &tmp.join("covers"))?;
    library.scan_blocking(&[music.clone().into()])?;
    let mut h = Headless::new(&config, library.clone(), (1280, 780), 1.0)?;

    let tracks = library.tracks_for_paths(&[&music]);
    player.send(PlayerCommand::Enqueue(
        tracks.into_iter().take(14).collect(),
    ));
    player.send(PlayerCommand::Next);
    player.send(PlayerCommand::SetVolume(0.0));
    std::thread::sleep(Duration::from_millis(1000));
    player.send(PlayerCommand::Seek(Duration::from_secs(60)));
    std::thread::sleep(Duration::from_millis(1500));

    // Hover the 4th row, then a few frames so smoothing settles.
    let hover = Input {
        mouse: Some((600.0, 28.0 + 76.0 + 3.0 * 42.0 + 20.0)),
        ..Default::default()
    };
    for _ in 0..5 {
        h.frame(&hover, &player)?;
        std::thread::sleep(Duration::from_millis(30));
    }
    save(
        &format!("{out}/gui-queue.png"),
        h.size,
        &h.frame(&hover, &player)?,
    )?;

    h.frame(&key(Key::Char('2')), &player)?;
    h.frame(&key(Key::Down), &player)?;
    h.frame(&key(Key::Down), &player)?;
    save(
        &format!("{out}/gui-browse.png"),
        h.size,
        &h.frame(&Input::default(), &player)?,
    )?;

    h.frame(&key(Key::Char('3')), &player)?;
    for _ in 0..20 {
        h.frame(&Input::default(), &player)?;
        std::thread::sleep(Duration::from_millis(16));
    }
    save(
        &format!("{out}/gui-visualizer.png"),
        h.size,
        &h.frame(&Input::default(), &player)?,
    )?;

    // Empty queue state, at 1.5x UI scale to check DPI handling.
    player.send(PlayerCommand::Clear);
    std::thread::sleep(Duration::from_millis(200));
    let mut h2 = Headless::new(&config, library.clone(), (1280, 780), 1.5)?;
    save(
        &format!("{out}/gui-empty-1.5x.png"),
        h2.size,
        &h2.frame(&Input::default(), &player)?,
    )?;
    Ok(())
}
