//! Drives the real TUI with scripted keys and prints rendered screens (volume 0).
//!
//! cargo run -p khoostic-tui --example headless -- <music folder>

use std::time::Duration;

use khoostic_audio::EngineConfig;
use khoostic_config::Config;
use khoostic_player::{Player, Resolvers};
use khoostic_tui::{App, ui};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn main() -> anyhow::Result<()> {
    let dir = std::env::args().nth(1).expect("music folder");
    let mut config = Config::default();
    config.library.music_dirs = vec![dir.into()];
    let player = Player::start(
        EngineConfig {
            volume: 0.0,
            ..Default::default()
        },
        Resolvers::with_defaults(),
    )?;
    let tmp = std::env::temp_dir().join("khoostic-example-lib");
    let library = khoostic_library::Library::open(&tmp.join("lib.db"), &tmp.join("covers"))?;
    let mut app = App::new(player, library, &config);
    let mut term = Terminal::new(TestBackend::new(100, 24))?;

    let mut screen = |app: &mut App, label: &str| -> anyhow::Result<()> {
        for _ in 0..3 {
            app.tick();
            std::thread::sleep(Duration::from_millis(60));
        }
        term.draw(|f| ui::draw(f, app))?;
        println!("=== {label} ===");
        let buf = term.backend().buffer();
        for y in 0..buf.area.height {
            let line: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
            println!("{}", line.trim_end());
        }
        Ok(())
    };
    let keys = |app: &mut App, s: &str| {
        for k in khoostic_tui::keymap::parse_keys(s).unwrap() {
            app.on_key(KeyEvent::new(k.code, k.mods));
        }
    };
    let typed = |app: &mut App, s: &str| {
        for c in s.chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    };

    screen(&mut app, "startup (empty queue)")?;
    keys(&mut app, "gb");
    screen(&mut app, "browser")?;
    keys(&mut app, "5a"); // add 5 entries from the cursor
    keys(&mut app, "gq");
    screen(&mut app, "queue after 5a")?;
    keys(&mut app, "<CR>");
    std::thread::sleep(Duration::from_millis(1500));
    screen(&mut app, "playing first track")?;
    keys(&mut app, "jVjd"); // visual-delete rows 2-3
    screen(&mut app, "after Vjd on rows 2-3")?;
    keys(&mut app, "Gp"); // paste them at the end
    screen(&mut app, "after G p")?;
    keys(&mut app, "/");
    typed(&mut app, "kitsune");
    screen(&mut app, "after /kitsune")?;
    keys(&mut app, ":");
    typed(&mut app, "vol 42");
    keys(&mut app, ":");
    typed(&mut app, "seek 1:00");
    keys(&mut app, "s");
    screen(&mut app, "after :vol 42, :seek 1:00, s")?;
    keys(&mut app, ":");
    typed(&mut app, "bogus");
    screen(&mut app, "error message")?;
    keys(&mut app, "gv");
    for _ in 0..20 {
        app.tick();
        std::thread::sleep(Duration::from_millis(16));
    }
    screen(&mut app, "visualizer: sandstorm")?;
    keys(&mut app, "c");
    screen(&mut app, "visualizer: spectrum")?;
    keys(&mut app, "?");
    screen(&mut app, "help")?;
    keys(&mut app, "q"); // closes help
    keys(&mut app, "q");
    println!("quit flag: {}", app.quit);
    Ok(())
}
