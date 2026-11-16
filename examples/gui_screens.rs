//! Renders the real GUI offscreen with scripted input and saves PNGs (volume 0). Uses a
//! throwaway library index in the temp directory.
//!
//! cargo run --example gui_screens -- <music folder> <out dir>

use std::time::Duration;

use khoostic::audio::EngineConfig;
use khoostic::config::Config;
use khoostic::gui::Headless;
use khoostic::gui::ui::{Input, Key, KeyPress};
use khoostic::library::Library;
use khoostic::player::{Player, PlayerCommand, Resolvers};

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
    // Lyrics from .lrc files only: no network in screenshots.
    config.lyrics.providers = vec!["local".into()];
    let tmp = std::env::temp_dir().join("khoostic-example-lib");
    let library = Library::open(&tmp.join("lib.db"), &tmp.join("covers"))?;
    let stats = library.scan_blocking(&[music.clone().into()])?;
    println!("scan: {stats:?}");

    // A disconnected Spotify client with throwaway storage, so the sidebar shows "Connect".
    let spotify = khoostic::spotify::Spotify::new(khoostic::spotify::SpotifyConfig {
        client_id: None,
        bitrate: khoostic::spotify::Bitrate::B320,
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
    let lastfm = khoostic::lastfm::Lastfm::with_dir("", "", true, tmp.join("lastfm"))?;
    let discovery = khoostic::discovery::Discovery::new(
        library.clone(),
        Some(spotify.clone()),
        Some(lastfm),
        true,
    );
    let mut h = Headless::new(
        &config,
        library.clone(),
        Some(spotify.clone()),
        discovery,
        (1280, 800),
        1.0,
    )?;
    let shot = |h: &mut Headless, name: &str, input: &Input| -> anyhow::Result<()> {
        let px = h.frame(input, &player)?;
        save(&format!("{out}/{name}.png"), h.size, &px)
    };

    shot(&mut h, "lib-albums", &Input::default())?;
    // Open the first album (top-left tile), then double-click its first track.
    h.frame(&click(420.0, 300.0, false), &player)?;
    h.frame(&click(700.0, 395.0, false), &player)?;
    h.frame(&click(700.0, 395.0, true), &player)?;
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

    // Like the playing song (the heart in the player bar fills in).
    if let Some(t) = library
        .all_tracks(khoostic::library::TrackOrder::Title)
        .into_iter()
        .find(|t| t.title.starts_with("Tsunami"))
    {
        library.set_liked(&t, true)?;
    }
    shot(&mut h, "lyrics", &keys(&[Key::Char('L')]))?;
    h.frame(&keys(&[Key::Char('L')]), &player)?;
    // Collapse Your Library to covers, and open it again.
    shot(&mut h, "collapsed", &click(60.0, 94.0, false))?;
    h.frame(&click(42.0, 94.0, false), &player)?;
    shot(&mut h, "lib-artists", &keys(&[Key::Char('2')]))?;
    shot(&mut h, "lib-songs", &keys(&[Key::Char('3')]))?;
    h.frame(&keys(&[Key::Char('/')]), &player)?;
    // Typed, then pasted (the trailing newline a copy often brings is dropped).
    shot(
        &mut h,
        "lib-search",
        &Input {
            paste: "a\r\n".into(),
            ..keys(&[Key::Char('b'), Key::Paste])
        },
    )?;
    shot(&mut h, "search-songs", &click(415.0, 108.0, false))?;
    h.frame(&click(360.0, 108.0, false), &player)?;
    let select_all = KeyPress {
        key: Key::Char('a'),
        shift: false,
        ctrl: true,
    };
    shot(
        &mut h,
        "search-selected",
        &Input {
            keys: vec![select_all],
            ..Default::default()
        },
    )?;
    player.send(khoostic::player::PlayerCommand::Enqueue(
        library
            .all_tracks(khoostic::library::TrackOrder::Title)
            .into_iter()
            .take(5)
            .collect(),
    ));
    h.frame(&keys(&[Key::Escape]), &player)?;
    shot(&mut h, "lib-queue", &keys(&[Key::Char('4')]))?;

    // Playlists: make a Khoostic playlist, open the context menu on a song, view Playlists.
    let some: Vec<_> = library
        .all_tracks(khoostic::library::TrackOrder::Title)
        .into_iter()
        .take(4)
        .collect();
    for p in library.playlists() {
        library.delete_playlist(p.id)?;
    }
    library.create_playlist("Road trip", &some)?;
    h.frame(&keys(&[Key::Char('3')]), &player)?;
    let row = Input {
        mouse: Some((600.0, 330.0)),
        right_pressed: true,
        ..Default::default()
    };
    shot(&mut h, "sp-menu", &row)?;
    h.frame(&keys(&[Key::Escape]), &player)?;
    shot(&mut h, "sp-playlists", &click(100.0, 258.0, false))?;
    shot(&mut h, "settings", &click(1116.0, 32.0, false))?;
    // Theme cards under Appearance: catch each swipe partway across, then settled.
    let mid = |h: &mut Headless, name: &str, input: &Input, ms: u64| -> anyhow::Result<()> {
        let px = h.frame_at(input, &player, Duration::from_millis(ms))?;
        save(&format!("{out}/{name}.png"), h.size, &px)
    };
    mid(&mut h, "theme-swipe", &click(610.0, 260.0, false), 380)?;
    shot(&mut h, "theme-pink", &Input::default())?;
    mid(&mut h, "theme-swipe-2", &click(792.0, 260.0, false), 520)?;
    shot(&mut h, "theme-catppuccin", &Input::default())?;
    shot(&mut h, "lyrics-catppuccin", &keys(&[Key::Char('L')]))?;
    h.frame(&keys(&[Key::Char('L')]), &player)?;
    // Back to the default theme for the rest.
    mid(&mut h, "theme-khoostic", &click(428.0, 260.0, false), 1200)?;
    shot(
        &mut h,
        "settings-lyrics",
        &Input {
            mouse: Some((700.0, 400.0)),
            scroll: 600.0,
            ..Default::default()
        },
    )?;
    // Visualizer settings: pick the Wild preset, open the Camera knobs, then reset.
    h.frame(&click(524.0, 391.0, false), &player)?;
    shot(&mut h, "settings-viz-camera", &click(613.0, 497.0, false))?;
    shot(&mut h, "settings-viz-reset", &click(740.0, 442.0, false))?;
    // Drag the Distance slider to the right.
    let drag = |x: f32, pressed: bool, down: bool, released: bool| Input {
        mouse: Some((x, 553.0)),
        pressed,
        down,
        released,
        ..Default::default()
    };
    h.frame(&drag(704.0, true, true, false), &player)?;
    h.frame(&drag(760.0, false, true, false), &player)?;
    h.frame(&drag(800.0, false, true, false), &player)?;
    shot(
        &mut h,
        "settings-viz-drag",
        &drag(800.0, false, false, true),
    )?;
    h.frame(&click(740.0, 442.0, false), &player)?;
    shot(&mut h, "visualizer", &keys(&[Key::Char('6')]))?;
    // 3D: the bars crumble into the storm, then fly back.
    mid(&mut h, "viz3d-1-crumble", &click(860.0, 109.0, false), 450)?;
    mid(&mut h, "viz3d-2-rising", &Input::default(), 900)?;
    mid(&mut h, "viz3d-3-storm", &Input::default(), 2500)?;
    mid(&mut h, "viz3d-4-storm", &Input::default(), 3000)?;
    for k in 0..4 {
        mid(&mut h, &format!("viz3d-storm-{k}"), &Input::default(), 1700)?;
    }
    // A quick burst, to see beats hit.
    for k in 0..8 {
        mid(&mut h, &format!("viz3d-burst-{k}"), &Input::default(), 150)?;
    }
    mid(&mut h, "viz3d-5-back", &click(800.0, 109.0, false), 900)?;
    mid(&mut h, "viz3d-6-flat", &Input::default(), 1600)?;
    h.frame(&keys(&[Key::Char('1')]), &player)?;
    h.frame(&keys(&[Key::Char('4')]), &player)?;
    let hover = |x: f32, y: f32| Input {
        mouse: Some((x, y)),
        ..Default::default()
    };
    shot(&mut h, "hover-tile", &hover(560.0, 260.0))?;
    shot(&mut h, "hover-seek", &hover(560.0, 774.0))?;
    // The smallest window the GUI allows, with the queue open.
    h.size = (720, 480);
    h.frame(&keys(&[Key::Char('4')]), &player)?;
    shot(&mut h, "narrow", &hover(10.0, 10.0))?;
    Ok(())
}
