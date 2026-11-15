//! Khoostic's terminal interface: modal, vim-flavored, minimal.

pub mod action;
pub mod app;
pub mod command;
pub mod input;
pub mod keymap;
pub mod library;
pub mod playlists;
pub mod theme;
pub mod ui;
pub mod visualizer;

use std::sync::Arc;
use std::time::{Duration, Instant};

use khoostic_config::Config;
use khoostic_core::Track;
use khoostic_library::Library;
use khoostic_player::{Player, PlayerCommand};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind,
};
use ratatui::crossterm::execute;

pub use app::App;

/// Redraw at least this often even when idle (clock, messages expiring).
const IDLE_REDRAW: Duration = Duration::from_millis(250);

pub struct Launch {
    pub tracks: Vec<Track>,
    pub autoplay: bool,
}

pub fn run(
    player: Player,
    library: Arc<Library>,
    spotify: Option<Arc<khoostic_spotify::Spotify>>,
    discovery: Arc<khoostic_discovery::Discovery>,
    config: &Config,
    launch: Launch,
) -> anyhow::Result<()> {
    let mut app = App::new(player, library, spotify, discovery, config);
    if !launch.tracks.is_empty() {
        app.player.send(if launch.autoplay {
            PlayerCommand::EnqueueAndPlay(launch.tracks)
        } else {
            PlayerCommand::Enqueue(launch.tracks)
        });
    }

    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableBracketedPaste);
    let result = event_loop(&mut terminal, &mut app, config.tui.fps.clamp(5, 240));
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    fps: u32,
) -> anyhow::Result<()> {
    let frame = Duration::from_secs(1) / fps;
    let mut last_draw = Instant::now() - IDLE_REDRAW;
    let mut dirty = true;
    loop {
        app.tick();
        if app.quit {
            return Ok(());
        }
        let animating = app.animating();
        let due = if animating { frame } else { IDLE_REDRAW };
        if dirty || last_draw.elapsed() >= due {
            terminal.draw(|f| ui::draw(f, app))?;
            last_draw = Instant::now();
            dirty = false;
        }

        let timeout = due.saturating_sub(last_draw.elapsed());
        if event::poll(timeout)? {
            // Drain everything that's queued so key repeat never lags behind rendering.
            loop {
                match event::read()? {
                    Event::Key(k) if k.kind != KeyEventKind::Release => app.on_key(k),
                    Event::Paste(text) => app.on_paste(&text),
                    _ => {}
                }
                dirty = true;
                if app.quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
    }
}
