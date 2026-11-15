//! Khoostic — one binary, several faces: `khoostic` (TUI), `khoostic gui`, `khoostic play`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use khoostic_audio::EngineConfig;
use khoostic_config::Config;
use khoostic_discovery::Discovery;
use khoostic_lastfm::Lastfm;
use khoostic_library::Library;
use khoostic_player::{Player, Resolvers};
use khoostic_spotify::{Bitrate, Spotify, SpotifyConfig, Status};

#[derive(Parser)]
#[command(name = "khoostic", version, about = "An overengineered music player")]
struct Cli {
    /// Config file (default: the platform config dir; see `khoostic config`).
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Output device (case-insensitive substring of its name). Overrides the config.
    #[arg(long, global = true)]
    device: Option<String>,

    /// Startup volume 0-100. Overrides the config.
    #[arg(long, global = true)]
    volume: Option<u8>,

    #[command(subcommand)]
    command: Option<Mode>,
}

#[derive(Subcommand)]
enum Mode {
    /// Terminal interface (the default). Optional files/folders are queued.
    Tui { paths: Vec<PathBuf> },
    /// Vulkan-rendered interface. Optional files/folders are queued.
    Gui {
        paths: Vec<PathBuf>,
        /// Start playing the given paths right away.
        #[arg(short, long)]
        play: bool,
    },
    /// Open the TUI and start playing these files/folders right away.
    Play {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Shuffle the queue.
        #[arg(short, long)]
        shuffle: bool,
    },
    /// List audio output devices.
    Devices,
    /// Print the config file path; `--init` writes a default one if missing.
    Config {
        #[arg(long)]
        init: bool,
    },
    /// Spotify account: `login` opens your browser to sign in.
    Spotify {
        #[command(subcommand)]
        action: SpotifyAction,
    },
    /// Last.fm scrobbling: `login` opens your browser (needs [lastfm] api_key and api_secret).
    Lastfm {
        #[command(subcommand)]
        action: SpotifyAction,
    },
}

#[derive(Subcommand)]
enum SpotifyAction {
    /// Sign in through your browser.
    Login,
    /// Forget the stored Spotify session.
    Logout,
    /// Show whether Khoostic is connected.
    Status,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_logging();

    let config = Config::load(cli.config.as_deref())?;
    let engine = EngineConfig {
        device: cli
            .device
            .clone()
            .or_else(|| (!config.audio.device.is_empty()).then(|| config.audio.device.clone())),
        volume: f32::from(cli.volume.unwrap_or(config.audio.volume).min(100)) / 100.0,
        buffer: Duration::from_millis(u64::from(config.audio.buffer_ms.clamp(100, 10_000))),
    };

    match cli.command.unwrap_or(Mode::Tui { paths: Vec::new() }) {
        Mode::Tui { paths } => tui(engine, &config, &paths, false, false),
        Mode::Play { paths, shuffle } => tui(engine, &config, &paths, true, shuffle),
        Mode::Gui { paths, play } => {
            let (player, s) = start(engine, &config)?;
            let tracks = s.library.tracks_for_paths(&paths);
            khoostic_gui::run(
                player,
                s.library,
                s.spotify,
                s.discovery,
                &config,
                tracks,
                play,
            )
        }
        Mode::Devices => {
            for name in khoostic_audio::output::device_names()? {
                println!("{name}");
            }
            Ok(())
        }
        Mode::Spotify { action } => spotify_cli(&config, action),
        Mode::Lastfm { action } => lastfm_cli(&config, action),
        Mode::Config { init } => {
            let path = match cli.config {
                Some(p) => p,
                None => Config::default_path().context("no config directory on this system")?,
            };
            if init && !path.exists() {
                Config::write_default(&path)?;
                println!("wrote {}", path.display());
            } else {
                println!("{}", path.display());
            }
            Ok(())
        }
    }
}

fn tui(
    engine: EngineConfig,
    config: &Config,
    paths: &[PathBuf],
    autoplay: bool,
    shuffle: bool,
) -> anyhow::Result<()> {
    let (player, s) = start(engine, config)?;
    let tracks = s.library.tracks_for_paths(paths);
    if autoplay && tracks.is_empty() {
        anyhow::bail!("no playable audio files found");
    }
    if shuffle {
        player.send(khoostic_player::PlayerCommand::SetShuffle(true));
    }
    khoostic_tui::run(
        player,
        s.library,
        s.spotify,
        s.discovery,
        config,
        khoostic_tui::Launch { tracks, autoplay },
    )
}

struct Services {
    library: Arc<Library>,
    spotify: Option<Arc<Spotify>>,
    discovery: Arc<Discovery>,
}

/// Everything both interfaces share: library, Spotify, Last.fm (scrobbling in the background),
/// radio/autoplay, and the player wired to them.
fn start(engine: EngineConfig, config: &Config) -> anyhow::Result<(Player, Services)> {
    let library = open_library(config)?;
    let spotify = open_spotify(config, true);
    let lastfm = open_lastfm(config);
    let player = Player::start(engine, resolvers(&spotify))?;
    if let Some(l) = &lastfm {
        l.start_scrobbler(player.handle());
    }
    let discovery = Discovery::new(
        library.clone(),
        spotify.clone(),
        lastfm,
        config.playback.autoplay,
    );
    discovery.start_autoplay(player.handle());
    Ok((
        player,
        Services {
            library,
            spotify,
            discovery,
        },
    ))
}

fn open_lastfm(config: &Config) -> Option<Arc<Lastfm>> {
    let c = &config.lastfm;
    match Lastfm::new(&c.api_key, &c.api_secret, c.scrobble) {
        Ok(l) => Some(l),
        Err(e) => {
            tracing::warn!("Last.fm unavailable: {e:#}");
            None
        }
    }
}

fn lastfm_cli(config: &Config, action: SpotifyAction) -> anyhow::Result<()> {
    use khoostic_lastfm::Status as L;
    let lf = open_lastfm(config).context("Last.fm support couldn't start")?;
    match action {
        SpotifyAction::Logout => {
            lf.sign_out();
            println!("Signed out of Last.fm.");
            return Ok(());
        }
        SpotifyAction::Login => {
            if lf.status() == L::NoApiKey {
                anyhow::bail!(
                    "set [lastfm] api_key and api_secret first (create them at https://www.last.fm/api/account/create)"
                );
            }
            println!("Opening your browser to allow Khoostic on Last.fm…");
            lf.sign_in();
        }
        SpotifyAction::Status => {}
    }
    loop {
        match lf.status() {
            L::SigningIn => std::thread::sleep(Duration::from_millis(200)),
            L::Connected(name) => {
                println!("Scrobbling to Last.fm as {name} ({} queued).", lf.queued());
                return Ok(());
            }
            L::NoApiKey => {
                println!("No Last.fm API account set ([lastfm] api_key, api_secret).");
                return Ok(());
            }
            L::Disconnected => {
                println!("Not connected. Run `khoostic lastfm login`.");
                return Ok(());
            }
            L::Failed(e) => anyhow::bail!(e),
        }
    }
}

/// Spotify client, reconnecting a previous session in the background. `None` if it can't
/// start (no data dir); everything else keeps working.
fn open_spotify(config: &Config, restore: bool) -> Option<std::sync::Arc<Spotify>> {
    let c = &config.spotify;
    let cfg = SpotifyConfig::default_dirs(Some(c.client_id.clone()), Bitrate::from_kbps(c.bitrate));
    match cfg.and_then(Spotify::new) {
        Ok(s) => {
            if restore {
                s.restore();
            }
            Some(s)
        }
        Err(e) => {
            tracing::warn!("Spotify unavailable: {e:#}");
            None
        }
    }
}

fn resolvers(spotify: &Option<std::sync::Arc<Spotify>>) -> Resolvers {
    let r = Resolvers::with_defaults();
    match spotify {
        Some(s) => r.with(s.resolver()),
        None => r,
    }
}

fn spotify_cli(config: &Config, action: SpotifyAction) -> anyhow::Result<()> {
    let spotify = open_spotify(config, false).context("Spotify support couldn't start")?;
    match action {
        SpotifyAction::Logout => {
            spotify.sign_out();
            println!("Signed out of Spotify.");
            return Ok(());
        }
        SpotifyAction::Login => {
            println!("Opening your browser to sign in to Spotify…");
            spotify.sign_in();
        }
        SpotifyAction::Status => spotify.restore(),
    }
    loop {
        match spotify.status() {
            Status::SigningIn | Status::Connecting => {
                std::thread::sleep(Duration::from_millis(100))
            }
            Status::Connected(a) => {
                let plan = if a.premium {
                    "Premium"
                } else {
                    "not Premium: streaming is unavailable"
                };
                println!("Connected to Spotify as {} ({plan}).", a.name);
                return Ok(());
            }
            Status::Disconnected => {
                println!("Not connected. Run `khoostic spotify login`.");
                return Ok(());
            }
            Status::Failed(e) => anyhow::bail!(e),
        }
    }
}

/// Opens the library index and, if configured, refreshes it in the background.
fn open_library(config: &Config) -> anyhow::Result<std::sync::Arc<Library>> {
    let library = Library::open_default()?;
    if config.library.scan_on_startup && !config.library.music_dirs.is_empty() {
        library.start_scan(config.library.music_dirs.clone());
    }
    Ok(library)
}

/// Logs go to a file: anything on stderr would corrupt the TUI.
fn init_logging() {
    let Ok(filter) = tracing_subscriber::EnvFilter::try_from_env("KHOOSTIC_LOG") else {
        return;
    };
    let path = std::env::temp_dir().join("khoostic.log");
    if let Ok(file) = std::fs::File::create(&path) {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .init();
    }
}
