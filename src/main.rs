//! Khoostic — one binary, several faces: `khoostic tui`, `khoostic gui`, `khoostic play`.

mod preview;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use khoostic_audio::{EngineConfig, is_supported_path};
use khoostic_core::Track;

#[derive(Parser)]
#[command(name = "khoostic", version, about = "An overengineered music player")]
struct Cli {
    /// Output device (case-insensitive substring of its name). Defaults to the system device.
    #[arg(long, global = true)]
    device: Option<String>,

    /// Initial volume, 0-100.
    #[arg(long, global = true, default_value_t = 80)]
    volume: u8,

    #[command(subcommand)]
    command: Option<Mode>,
}

#[derive(Subcommand)]
enum Mode {
    /// Terminal interface with vim-style controls (step 2).
    Tui,
    /// Vulkan-rendered interface with 3D visualizers (step 3).
    Gui,
    /// Play files or folders with a minimal live preview. Useful for testing the engine.
    Play {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Shuffle the queue before starting.
        #[arg(short, long)]
        shuffle: bool,
    },
    /// List audio output devices.
    Devices,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("KHOOSTIC_LOG")
                .unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let engine = EngineConfig {
        device: cli.device,
        volume: f32::from(cli.volume.min(100)) / 100.0,
        ..Default::default()
    };

    match cli.command {
        Some(Mode::Play { paths, shuffle }) => {
            let tracks = collect_tracks(&paths);
            if tracks.is_empty() {
                anyhow::bail!("no playable audio files found");
            }
            preview::run(engine, tracks, shuffle)
        }
        Some(Mode::Devices) => {
            for name in khoostic_audio::output::device_names()? {
                println!("{name}");
            }
            Ok(())
        }
        Some(Mode::Tui) | None => {
            anyhow::bail!("the TUI arrives in step 2 — try `khoostic play <files or folders>`")
        }
        Some(Mode::Gui) => anyhow::bail!("the Vulkan GUI arrives in step 3"),
    }
}

/// Expands folders recursively and keeps files we can decode, in a stable order.
fn collect_tracks(paths: &[PathBuf]) -> Vec<Track> {
    let mut files = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut found: Vec<PathBuf> = walkdir::WalkDir::new(p)
                .follow_links(true)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_file() && is_supported_path(e.path()))
                .map(|e| e.into_path())
                .collect();
            found.sort();
            files.extend(found);
        } else if p.is_file() {
            files.push(p.clone());
        } else {
            eprintln!("skipping {}: not found", p.display());
        }
    }
    files.into_iter().map(Track::from_path).collect()
}
