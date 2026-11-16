//! Spotify for Khoostic: browser sign-in, your library and search through Spotify's client API,
//! and streaming through librespot into Khoostic's own audio engine.
//!
//! Everything network-bound runs on a private tokio runtime. Frontends get non-blocking
//! [`Pending`] results they can poll once per frame, plus a [`Status`] to show.

pub mod api;
pub mod artist;
pub mod auth;
pub mod collection;
pub mod source;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Context;
use librespot_core::authentication::Credentials;
use librespot_core::cache::Cache;
use librespot_core::config::SessionConfig;
use librespot_core::session::Session;
use parking_lot::Mutex;
use tokio::sync::oneshot;

use crate::audio::{AudioSource, SourceError};
use crate::model::{Track, TrackLocator};
use crate::pending::Pending;
use crate::player::SourceResolver;

pub use api::{AlbumSummary, Playlist, SearchResults};
pub use artist::{ArtistPage, ArtistSummary};
pub use auth::Tokens;
pub use source::Bitrate;

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Disconnected,
    /// Browser is open; waiting for the user to approve.
    SigningIn,
    Connecting,
    Connected(Account),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    pub id: String,
    pub name: String,
    pub premium: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddOutcome {
    pub added: usize,
    /// Local (or other non-Spotify) tracks, which Spotify playlists can't hold.
    pub skipped_local: usize,
}

#[derive(Debug, Clone)]
pub struct SpotifyConfig {
    /// Override the OAuth client id (e.g. your own developer app). `None` = librespot's.
    pub client_id: Option<String>,
    pub bitrate: Bitrate,
    /// Where tokens and librespot credentials are stored.
    pub data_dir: PathBuf,
    /// Where streamed audio is cached (bounded).
    pub cache_dir: PathBuf,
}

impl SpotifyConfig {
    pub fn default_dirs(client_id: Option<String>, bitrate: Bitrate) -> anyhow::Result<Self> {
        let dirs =
            directories::ProjectDirs::from("", "", "khoostic").context("no home directory")?;
        Ok(Self {
            client_id: client_id.filter(|c| !c.trim().is_empty()),
            bitrate,
            data_dir: dirs.data_local_dir().join("spotify"),
            cache_dir: dirs.cache_dir().join("spotify-audio"),
        })
    }
}

struct Inner {
    cfg: SpotifyConfig,
    client_id: String,
    http: reqwest::Client,
    status: Mutex<Status>,
    session: tokio::sync::Mutex<Option<Session>>,
    tokens: tokio::sync::Mutex<Option<Tokens>>,
    cancel: Mutex<Option<oneshot::Sender<()>>>,
    /// Bumped on every status change so UIs can refresh cheaply.
    version: AtomicU64,
}

pub struct Spotify {
    rt: tokio::runtime::Runtime,
    inner: Arc<Inner>,
}

const AUDIO_CACHE_LIMIT: u64 = 1 << 30;
/// Starting a session; past this the UI shows an error instead of "connecting" forever.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Any one library/search request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(90);

impl Spotify {
    pub fn new(cfg: SpotifyConfig) -> anyhow::Result<Arc<Self>> {
        std::fs::create_dir_all(&cfg.data_dir)?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("khoostic-spotify")
            .enable_all()
            .build()?;
        let client_id = cfg
            .client_id
            .clone()
            .unwrap_or_else(|| auth::DEFAULT_CLIENT_ID.to_string());
        Ok(Arc::new(Self {
            rt,
            inner: Arc::new(Inner {
                cfg,
                client_id,
                http: reqwest::Client::builder().user_agent("khoostic").build()?,
                status: Mutex::new(Status::Disconnected),
                session: Default::default(),
                tokens: Default::default(),
                cancel: Mutex::new(None),
                version: AtomicU64::new(0),
            }),
        }))
    }

    pub fn status(&self) -> Status {
        self.inner.status.lock().clone()
    }

    pub fn version(&self) -> u64 {
        self.inner.version.load(Ordering::Acquire)
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.status(), Status::Connected(_))
    }

    /// Opens the browser for sign-in. Watch [`Spotify::status`] for the outcome.
    pub fn sign_in(&self) {
        if matches!(self.status(), Status::SigningIn | Status::Connecting) {
            return;
        }
        let (tx, rx) = oneshot::channel();
        *self.inner.cancel.lock() = Some(tx);
        self.inner.set(Status::SigningIn);
        let inner = self.inner.clone();
        self.rt.spawn(async move {
            let result = async {
                let tokens = auth::sign_in(&inner.http, &inner.client_id, rx).await?;
                inner.connect(Some(tokens)).await
            }
            .await;
            if let Err(e) = result {
                tracing::warn!("Spotify sign-in failed: {e:#}");
                inner.set(Status::Failed(format!("{e:#}")));
            }
        });
    }

    pub fn cancel_sign_in(&self) {
        if let Some(tx) = self.inner.cancel.lock().take() {
            let _ = tx.send(());
        }
    }

    /// Reconnects silently with what an earlier sign-in saved (call at startup).
    pub fn restore(&self) {
        let saved = self.inner.tokens_path().exists()
            || self.inner.cfg.data_dir.join("credentials.json").exists();
        if !saved || !matches!(self.status(), Status::Disconnected | Status::Failed(_)) {
            return;
        }
        self.inner.set(Status::Connecting);
        let inner = self.inner.clone();
        self.rt.spawn(async move {
            if let Err(e) = inner.connect(None).await {
                tracing::warn!("Spotify reconnect failed: {e:#}");
                inner.set(Status::Failed(format!(
                    "couldn't reconnect ({e:#}); sign in again"
                )));
            }
        });
    }

    pub fn sign_out(&self) {
        let inner = self.inner.clone();
        self.rt.block_on(async move {
            if let Some(s) = inner.session.lock().await.take() {
                s.shutdown();
            }
            *inner.tokens.lock().await = None;
        });
        let _ = std::fs::remove_file(self.inner.tokens_path());
        let _ = std::fs::remove_file(self.inner.cfg.data_dir.join("credentials.json"));
        self.inner.set(Status::Disconnected);
    }

    /// Runs `f` with the streaming session on the runtime, bounded by [`REQUEST_TIMEOUT`].
    fn with_session<T, F, Fut>(&self, f: F) -> Pending<T>
    where
        T: Send + 'static,
        F: FnOnce(Session) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<T>> + Send,
    {
        let (tx, rx) = Pending::new();
        let inner = self.inner.clone();
        self.rt.spawn(async move {
            let work = async {
                let session = inner.session().await?;
                f(session).await
            };
            let result = tokio::time::timeout(REQUEST_TIMEOUT, work)
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!("Spotify didn't answer in time")));
            tx.resolve(result);
        });
        rx
    }

    pub fn search(&self, query: &str, limit: u32) -> Pending<SearchResults> {
        let q = query.to_owned();
        self.with_session(move |s| async move { api::search(&s, &q, limit as usize).await })
    }

    /// An artist's profile page.
    pub fn artist(&self, id: &str) -> Pending<ArtistPage> {
        let id = id.to_owned();
        self.with_session(move |s| async move { artist::page(&s, &id).await })
    }

    /// Names and pictures for artist ids.
    pub fn artist_summaries(&self, ids: Vec<String>) -> Pending<Vec<ArtistSummary>> {
        self.with_session(move |s| async move { artist::summaries(&s, &ids).await })
    }

    /// The artists this account follows, most recent first.
    pub fn followed_artists(&self) -> Pending<Vec<ArtistSummary>> {
        self.with_session(|s| async move {
            let ids: Vec<String> = collection::list(&s, collection::Set::Artist)
                .await?
                .iter()
                .filter_map(|u| u.strip_prefix("spotify:artist:").map(str::to_owned))
                .collect();
            artist::summaries(&s, &ids).await
        })
    }

    /// Follows or unfollows an artist.
    pub fn set_following(&self, artist_id: &str, follow: bool) -> Pending<()> {
        let uri = format!("spotify:artist:{artist_id}");
        self.with_session(move |s| async move {
            collection::write(&s, collection::Set::Artist, &[uri], follow).await
        })
    }

    /// Ids of every liked song (cheap: no metadata).
    pub fn liked_ids(&self) -> Pending<std::collections::HashSet<String>> {
        self.with_session(|s| async move {
            Ok(collection::list(&s, collection::Set::Collection)
                .await?
                .iter()
                .filter_map(|u| u.strip_prefix("spotify:track:").map(str::to_owned))
                .collect())
        })
    }

    /// Adds songs to (or removes them from) Liked Songs.
    pub fn set_liked(&self, track_ids: Vec<String>, liked: bool) -> Pending<()> {
        let uris: Vec<String> = track_ids
            .iter()
            .map(|id| format!("spotify:track:{id}"))
            .collect();
        self.with_session(move |s| async move {
            collection::write(&s, collection::Set::Collection, &uris, liked).await
        })
    }

    /// Lyrics for a Spotify track (`None` when Spotify has none).
    pub fn lyrics(&self, track_id: &str) -> Pending<Option<crate::model::Lyrics>> {
        let id = track_id.to_owned();
        self.with_session(move |s| async move { api::lyrics(&s, &id).await })
    }

    /// Songs like `track_id` (a Spotify base62 id), from Spotify's radio.
    pub fn radio(&self, track_id: &str, limit: usize) -> Pending<Vec<Track>> {
        let id = track_id.to_owned();
        self.with_session(move |s| async move { api::radio(&s, &id, limit).await })
    }

    pub fn playlists(&self) -> Pending<Vec<Playlist>> {
        self.with_session(|s| async move { api::playlists(&s).await })
    }

    pub fn playlist_tracks(&self, id: &str) -> Pending<Vec<Track>> {
        let id = id.to_owned();
        self.with_session(move |s| async move { api::playlist_tracks(&s, &id).await })
    }

    pub fn saved_tracks(&self) -> Pending<Vec<Track>> {
        self.with_session(|s| async move { api::saved_tracks(&s).await })
    }

    pub fn album_tracks(&self, id: &str) -> Pending<Vec<Track>> {
        let id = id.to_owned();
        self.with_session(move |s| async move { api::album_tracks(&s, &id).await })
    }

    /// Adds tracks to a Spotify playlist. Local tracks are skipped (Spotify can't store them)
    /// and reported in the outcome.
    pub fn add_to_playlist(&self, playlist_id: &str, tracks: &[Track]) -> Pending<AddOutcome> {
        let (ids, skipped_local) = api::spotify_ids(tracks);
        let pid = playlist_id.to_owned();
        self.with_session(move |s| async move {
            let added = api::add_to_playlist(&s, &pid, &ids).await?;
            Ok(AddOutcome {
                added,
                skipped_local,
            })
        })
    }

    /// Creates a playlist with the Spotify tracks among `tracks`.
    pub fn create_playlist(&self, name: &str, tracks: &[Track]) -> Pending<(String, AddOutcome)> {
        let (ids, skipped_local) = api::spotify_ids(tracks);
        let name = name.to_owned();
        self.with_session(move |s| async move {
            let id = api::create_playlist(&s, &name).await?;
            let added = api::add_to_playlist(&s, &id, &ids).await?;
            Ok((
                id,
                AddOutcome {
                    added,
                    skipped_local,
                },
            ))
        })
    }

    /// Blocking HTTP GET (cover art). Call from a plain thread, not from async code.
    pub fn fetch_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>> {
        let http = self.inner.http.clone();
        let url = url.to_owned();
        self.rt.block_on(async move {
            let r = http.get(&url).send().await?.error_for_status()?;
            Ok(r.bytes().await?.to_vec())
        })
    }

    /// The resolver that lets the player stream `TrackLocator::Spotify` tracks.
    pub fn resolver(self: &Arc<Self>) -> SpotifyResolver {
        SpotifyResolver {
            spotify: self.clone(),
        }
    }
}

impl Inner {
    fn set(&self, status: Status) {
        *self.status.lock() = status;
        self.version.fetch_add(1, Ordering::AcqRel);
    }

    fn tokens_path(&self) -> PathBuf {
        self.cfg.data_dir.join("tokens.json")
    }

    fn load_tokens(&self) -> Option<Tokens> {
        serde_json::from_slice(&std::fs::read(self.tokens_path()).ok()?).ok()
    }

    fn save_tokens(&self, t: &Tokens) {
        if let Ok(json) = serde_json::to_vec(t) {
            let _ = std::fs::write(self.tokens_path(), json);
        }
    }

    /// A valid OAuth token (only needed to start a session), refreshed and saved when stale.
    async fn access_token(&self) -> anyhow::Result<String> {
        let mut guard = self.tokens.lock().await;
        if guard.is_none() {
            *guard = self.load_tokens();
        }
        let t = guard.as_mut().context("not signed in to Spotify")?;
        if t.stale() {
            *t = auth::refresh(&self.http, &self.client_id, &t.refresh_token).await?;
            self.save_tokens(t);
        }
        Ok(t.access_token.clone())
    }

    fn cache(&self) -> Option<Cache> {
        Cache::new(
            Some(self.cfg.data_dir.clone()),
            None,
            Some(self.cfg.cache_dir.clone()),
            Some(AUDIO_CACHE_LIMIT),
        )
        .ok()
    }

    async fn new_session(&self, credentials: Credentials) -> anyhow::Result<Session> {
        let config = SessionConfig {
            client_id: self.client_id.clone(),
            ..Default::default()
        };
        let session = Session::new(config, self.cache());
        tokio::time::timeout(CONNECT_TIMEOUT, session.connect(credentials, true))
            .await
            .map_err(|_| anyhow::anyhow!("Spotify didn't answer in time"))?
            .context("connecting to Spotify")?;
        Ok(session)
    }

    /// Starts a session: librespot's saved credentials first (no sign-in server involved),
    /// then a fresh OAuth token.
    async fn open_session(&self) -> anyhow::Result<Session> {
        if let Some(creds) = self.cache().and_then(|c| c.credentials()) {
            match self.new_session(creds).await {
                Ok(s) => return Ok(s),
                Err(e) => tracing::info!("saved Spotify credentials didn't work: {e:#}"),
            }
        }
        let token = self.access_token().await?;
        self.new_session(Credentials::with_access_token(token))
            .await
    }

    /// Connects and publishes the account. `tokens` comes from a fresh browser sign-in.
    async fn connect(&self, tokens: Option<Tokens>) -> anyhow::Result<()> {
        self.set(Status::Connecting);
        let session = match tokens {
            Some(t) => {
                self.save_tokens(&t);
                let token = t.access_token.clone();
                *self.tokens.lock().await = Some(t);
                self.new_session(Credentials::with_access_token(token))
                    .await?
            }
            None => self.open_session().await?,
        };
        let (name, premium) = api::account(&session).await;
        let account = Account {
            id: session.username(),
            name,
            premium,
        };
        *self.session.lock().await = Some(session);
        tracing::info!(user = %account.name, premium = account.premium, "connected to Spotify");
        self.set(Status::Connected(account));
        Ok(())
    }

    /// The streaming session, reconnected if Spotify dropped it.
    async fn session(&self) -> anyhow::Result<Session> {
        let mut guard = self.session.lock().await;
        if let Some(s) = guard.as_ref().filter(|s| !s.is_invalid()) {
            return Ok(s.clone());
        }
        if !matches!(*self.status.lock(), Status::Connected(_)) {
            anyhow::bail!("not connected to Spotify");
        }
        let s = self.open_session().await?;
        *guard = Some(s.clone());
        Ok(s)
    }
}

pub struct SpotifyResolver {
    spotify: Arc<Spotify>,
}

impl SourceResolver for SpotifyResolver {
    fn accepts(&self, locator: &TrackLocator) -> bool {
        matches!(locator, TrackLocator::Spotify { .. })
    }

    fn open(&self, track: &Track) -> Result<Box<dyn AudioSource>, SourceError> {
        let TrackLocator::Spotify { id } = &track.locator else {
            return Err(SourceError::Unsupported("not a Spotify track".into()));
        };
        let s = &self.spotify;
        match s.status() {
            Status::Connected(a) if !a.premium => {
                return Err(SourceError::Unsupported(
                    "Spotify Premium is required to stream".into(),
                ));
            }
            Status::Connected(_) => {}
            _ => {
                return Err(SourceError::Unsupported(
                    "connect Spotify to play this".into(),
                ));
            }
        }
        let inner = s.inner.clone();
        let bitrate = inner.cfg.bitrate;
        let id = id.clone();
        // Network work on the runtime; the decoder probe (which reads the stream) on this thread.
        let fetched = s.rt.block_on(async move {
            let session = inner.session().await?;
            let parts = source::fetch(&session, &id, bitrate).await?;
            anyhow::Ok((session, parts))
        });
        // Anything but a track that can't be played (throttled key requests, network trouble,
        // a dropped session) is worth another try in a moment.
        let (session, (file, len, duration)) = fetched.map_err(|e| {
            if e.downcast_ref::<source::NotPlayable>().is_some() {
                SourceError::Unsupported(format!("{e:#}"))
            } else {
                SourceError::Unavailable(format!("{e:#}"))
            }
        })?;
        let src = source::open_decoder(session, file, len, duration)
            .map_err(|e| SourceError::Decode(format!("{e:#}")))?;
        Ok(Box::new(src))
    }
}
