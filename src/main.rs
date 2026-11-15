//! `khoostic`: opens the GUI. `khoostic --tui` opens the terminal interface instead; the
//! subcommands manage devices, config and accounts.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use khoostic::config::Config;
use khoostic::services::{self, AudioOverrides, Services};
use khoostic::{gui, lastfm, spotify, tui};

#[derive(Parser)]
#[command(name = "khoostic", version, about = "An overengineered music player")]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    /// Files or folders to queue.
    paths: Vec<PathBuf>,

    /// Use the terminal interface instead of the GUI.
    #[arg(long)]
    tui: bool,

    /// Start playing the given paths right away.
    #[arg(short, long)]
    play: bool,

    /// Shuffle the queue.
    #[arg(short, long)]
    shuffle: bool,

    /// Config file (default: the platform config dir; see `khoostic config`).
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Output device (case-insensitive substring of its name). Overrides the config.
    #[arg(long)]
    device: Option<String>,

    /// Startup volume 0-100. Overrides the config.
    #[arg(long)]
    volume: Option<u8>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// List audio output devices.
    Devices,
    /// Print the config file path; `--init` writes a default one if missing.
    Config {
        #[arg(long)]
        init: bool,
    },
    /// Spotify account: `login` opens your browser to sign in (Premium streams).
    Spotify {
        #[command(subcommand)]
        action: Account,
    },
    /// Last.fm scrobbling: `login` opens your browser (needs [lastfm] api_key and api_secret).
    Lastfm {
        #[command(subcommand)]
        action: Account,
    },
}

#[derive(Subcommand, Clone, Copy)]
enum Account {
    /// Sign in through your browser.
    Login,
    /// Forget the stored session.
    Logout,
    /// Show whether Khoostic is connected.
    Status,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_logging();
    let config = Config::load(cli.config.as_deref())?;

    match cli.command {
        None => {
            let audio = AudioOverrides {
                device: cli.device,
                volume: cli.volume,
            };
            let app = Services::start(&config, audio)?;
            let launch = app.launch(&cli.paths, cli.play, cli.shuffle)?;
            if cli.tui {
                tui::run(app, &config, launch)
            } else {
                gui::run(app, &config, launch)
            }
        }
        Some(Command::Devices) => {
            for name in khoostic::audio::output::device_names()? {
                println!("{name}");
            }
            Ok(())
        }
        Some(Command::Config { init }) => {
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
        Some(Command::Spotify { action }) => spotify_account(&config, action),
        Some(Command::Lastfm { action }) => lastfm_account(&config, action),
    }
}

fn spotify_account(config: &Config, action: Account) -> anyhow::Result<()> {
    use spotify::Status;
    let sp = services::open_spotify(config, false).context("Spotify support couldn't start")?;
    match action {
        Account::Logout => {
            sp.sign_out();
            println!("Signed out of Spotify.");
            return Ok(());
        }
        Account::Login => {
            println!("Opening your browser to sign in to Spotify…");
            sp.sign_in();
        }
        Account::Status => sp.restore(),
    }
    loop {
        match sp.status() {
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

fn lastfm_account(config: &Config, action: Account) -> anyhow::Result<()> {
    use lastfm::Status;
    let lf = services::open_lastfm(config).context("Last.fm support couldn't start")?;
    match action {
        Account::Logout => {
            lf.sign_out();
            println!("Signed out of Last.fm.");
            return Ok(());
        }
        Account::Login => {
            if lf.status() == Status::NoApiKey {
                anyhow::bail!(
                    "set [lastfm] api_key and api_secret first (create them at https://www.last.fm/api/account/create)"
                );
            }
            println!("Opening your browser to allow Khoostic on Last.fm…");
            lf.sign_in();
        }
        Account::Status => {}
    }
    loop {
        match lf.status() {
            Status::SigningIn => std::thread::sleep(Duration::from_millis(200)),
            Status::Connected(name) => {
                println!("Scrobbling to Last.fm as {name} ({} queued).", lf.queued());
                return Ok(());
            }
            Status::NoApiKey => {
                println!("No Last.fm API account set ([lastfm] api_key, api_secret).");
                return Ok(());
            }
            Status::Disconnected => {
                println!("Not connected. Run `khoostic lastfm login`.");
                return Ok(());
            }
            Status::Failed(e) => anyhow::bail!(e),
        }
    }
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
