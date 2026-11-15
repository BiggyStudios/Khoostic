//! Khoostic — one binary, several faces: `khoostic` (TUI), `khoostic gui`, `khoostic play`.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use khoostic_audio::EngineConfig;
use khoostic_config::Config;
use khoostic_player::{Player, Resolvers, scan};

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
    /// Vulkan-rendered interface with 3D visualizers (step 3).
    Gui,
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
        Mode::Gui => anyhow::bail!("the Vulkan GUI arrives in step 3"),
        Mode::Devices => {
            for name in khoostic_audio::output::device_names()? {
                println!("{name}");
            }
            Ok(())
        }
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
    let tracks = scan::collect_tracks(paths);
    if autoplay && tracks.is_empty() {
        anyhow::bail!("no playable audio files found");
    }
    let player = Player::start(engine, Resolvers::with_defaults())?;
    if shuffle {
        player.send(khoostic_player::PlayerCommand::SetShuffle(true));
    }
    khoostic_tui::run(player, config, khoostic_tui::Launch { tracks, autoplay })
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
