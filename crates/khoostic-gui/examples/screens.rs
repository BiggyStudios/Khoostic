//! Renders the real GUI offscreen with scripted input and saves PNGs (volume 0). Uses a
//! throwaway library index in the temp directory.
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

fn keys(ks: &[Key]) -> Input {
    Input {
        keys: ks
            .iter()
            .map(|&key| KeyPress {
                key,
                shift: false,
                ctrl: false,
            })
            .collect(),
        ..Default::default()
    }
}

fn click(x: f32, y: f32, double: bool) -> Input {
    Input {
        mouse: Some((x, y)),
        pressed: true,
        double_click: double,
        ..Default::default()
    }
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let music = args.next().expect("music folder");
    let out = args.next().expect("output dir");
    let mut config = Config::default();
    config.library.music_dirs = vec![music.clone().into()];
    let tmp = std::env::temp_dir().join("khoostic-example-lib");
    let library = Library::open(&tmp.join("lib.db"), &tmp.join("covers"))?;
    let stats = library.scan_blocking(&[music.clone().into()])?;
    println!("scan: {stats:?}");

    // A disconnected Spotify client with throwaway storage, so the sidebar shows "Connect".
    let spotify = khoostic_spotify::Spotify::new(khoostic_spotify::SpotifyConfig {
        client_id: None,
        bitrate: khoostic_spotify::Bitrate::B320,
        data_dir: tmp.join("spotify"),
        cache_dir: tmp.join("spotify-cache"),
    })?;
    let _ = std::fs::remove_dir_all(tmp.join("spotify"));
    let player = Player::start(
        EngineConfig {
            volume: 0.0,
            ..Default::default()
        },
        Resolvers::with_defaults(),
    )?;
    let mut h = Headless::new(
        &config,
        library.clone(),
        Some(spotify.clone()),
        (1280, 800),
        1.0,
    )?;
    let shot = |h: &mut Headless, name: &str, input: &Input| -> anyhow::Result<()> {
        let px = h.frame(input, &player)?;
        save(&format!("{out}/{name}.png"), h.size, &px)
    };

    shot(&mut h, "lib-albums", &Input::default())?;
    // Open the first album (top-left tile), then double-click its first track.
    h.frame(&click(330.0, 190.0, false), &player)?;
    h.frame(&click(600.0, 252.0 + 20.0, false), &player)?;
    h.frame(&click(600.0, 252.0 + 20.0, true), &player)?;
    std::thread::sleep(Duration::from_millis(1200));
    player.send(PlayerCommand::Seek(Duration::from_secs(40)));
    std::thread::sleep(Duration::from_millis(600));
    shot(
        &mut h,
        "lib-album",
        &Input {
            mouse: Some((600.0, 500.0)),
            ..Default::default()
        },
    )?;

    shot(&mut h, "lib-artists", &keys(&[Key::Char('2')]))?;
    shot(&mut h, "lib-songs", &keys(&[Key::Char('3')]))?;
    h.frame(&keys(&[Key::Char('/')]), &player)?;
    shot(
        &mut h,
        "lib-search",
        &keys(&[Key::Char('b'), Key::Char('a')]),
    )?;
    shot(&mut h, "lib-queue", &keys(&[Key::Escape, Key::Char('4')]))?;

    // Playlists: make a Khoostic playlist, open the context menu on a song, view Playlists.
    let some: Vec<_> = library
        .all_tracks(khoostic_library::TrackOrder::Title)
        .into_iter()
        .take(4)
        .collect();
    for p in library.playlists() {
        library.delete_playlist(p.id)?;
    }
    library.create_playlist("Road trip", &some)?;
    h.frame(&keys(&[Key::Char('3')]), &player)?;
    let row = Input {
        mouse: Some((600.0, 110.0 + 36.0 + 2.0 * 46.0 + 20.0)),
        right_pressed: true,
        ..Default::default()
    };
    shot(&mut h, "sp-menu", &row)?;
    h.frame(&keys(&[Key::Escape]), &player)?;
    shot(&mut h, "sp-playlists", &click(100.0, 280.0, false))?;
    shot(&mut h, "settings", &click(100.0, 670.0, false))?;
    shot(&mut h, "visualizer", &keys(&[Key::Char('6')]))?;
    h.frame(&keys(&[Key::Char('1')]), &player)?;
    let hover = |x: f32, y: f32| Input {
        mouse: Some((x, y)),
        ..Default::default()
    };
    shot(&mut h, "hover-tile", &hover(560.0, 200.0))?;
    shot(&mut h, "hover-seek", &hover(560.0, 774.0))?;
    Ok(())
}
