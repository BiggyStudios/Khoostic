//! Startup wiring shared by both interfaces: the player and everything around it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::audio::EngineConfig;
use crate::config::Config;
use crate::discord::Discord;
use crate::discovery::Discovery;
use crate::jam::Jams;
use crate::lastfm::Lastfm;
use crate::library::Library;
use crate::model::Track;
use crate::player::{Player, PlayerCommand, Resolvers};
use crate::spotify::{Bitrate, Spotify, SpotifyConfig};

/// The running app minus its interface: player, library index, Spotify (if it could start) and
/// discovery (radio, autoplay; owns Last.fm).
pub struct Services {
    pub player: Player,
    pub library: Arc<Library>,
    pub spotify: Option<Arc<Spotify>>,
    pub discovery: Arc<Discovery>,
    /// Discord status (follows the player; idle until it has an application id).
    pub discord: Arc<Discord>,
    /// Listening together.
    pub jams: Arc<Jams>,
}

/// What the interface should do with files/folders given on the command line.
#[derive(Default)]
pub struct Launch {
    pub tracks: Vec<Track>,
    /// Start playing them right away (otherwise they're only queued).
    pub play: bool,
}

/// Command-line overrides for the audio settings in the config.
#[derive(Default)]
pub struct AudioOverrides {
    pub device: Option<String>,
    pub volume: Option<u8>,
}

impl Services {
    /// Opens the library (rescanning in the background if configured), reconnects Spotify,
    /// starts the audio engine and the background scrobbler and autoplay watcher.
    pub fn start(config: &Config, audio: AudioOverrides) -> anyhow::Result<Self> {
        let library = open_library(config)?;
        let spotify = open_spotify(config, true);
        let lastfm = open_lastfm(config);
        let player = Player::start(engine_config(config, audio), resolvers(&spotify))?;
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
        let discord = Discord::start(&config.discord, player.handle());
        let jams = Jams::new(
            &config.jam,
            player.handle(),
            discovery.clone(),
            spotify.clone(),
            Some(discord.clone()),
        );
        Ok(Self {
            player,
            library,
            spotify,
            discovery,
            discord,
            jams,
        })
    }

    /// Resolves command-line paths to tracks; `play` starts them, otherwise they're queued.
    pub fn launch(&self, paths: &[PathBuf], play: bool, shuffle: bool) -> anyhow::Result<Launch> {
        let tracks = self.library.tracks_for_paths(paths);
        if !paths.is_empty() && tracks.is_empty() {
            anyhow::bail!("no playable audio files found");
        }
        if shuffle {
            self.player.send(PlayerCommand::SetShuffle(true));
        }
        Ok(Launch { tracks, play })
    }
}

impl Launch {
    /// Hands the tracks to the player.
    pub fn apply(self, player: &Player) {
        if self.tracks.is_empty() {
            return;
        }
        player.send(if self.play {
            PlayerCommand::EnqueueAndPlay(self.tracks)
        } else {
            PlayerCommand::Enqueue(self.tracks)
        });
    }
}

fn engine_config(config: &Config, o: AudioOverrides) -> EngineConfig {
    let a = &config.audio;
    EngineConfig {
        device: o
            .device
            .or_else(|| (!a.device.is_empty()).then(|| a.device.clone())),
        volume: f32::from(o.volume.unwrap_or(a.volume).min(100)) / 100.0,
        buffer: Duration::from_millis(u64::from(a.buffer_ms.clamp(100, 10_000))),
    }
}

/// Opens the library index and, if configured, refreshes it in the background.
fn open_library(config: &Config) -> anyhow::Result<Arc<Library>> {
    let library = Library::open_default()?;
    if config.library.scan_on_startup && !config.library.music_dirs.is_empty() {
        library.start_scan(config.library.music_dirs.clone());
    }
    Ok(library)
}

/// Spotify client, reconnecting a previous session in the background if `restore`. `None` if
/// it can't start (no data dir); everything else keeps working.
pub fn open_spotify(config: &Config, restore: bool) -> Option<Arc<Spotify>> {
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

/// Last.fm client (signed out or without an API key it still works for nothing but status).
pub fn open_lastfm(config: &Config) -> Option<Arc<Lastfm>> {
    let c = &config.lastfm;
    match Lastfm::new(&c.api_key, &c.api_secret, c.scrobble) {
        Ok(l) => Some(l),
        Err(e) => {
            tracing::warn!("Last.fm unavailable: {e:#}");
            None
        }
    }
}

fn resolvers(spotify: &Option<Arc<Spotify>>) -> Resolvers {
    let r = Resolvers::with_defaults();
    match spotify {
        Some(s) => r.with(s.resolver()),
        None => r,
    }
}
