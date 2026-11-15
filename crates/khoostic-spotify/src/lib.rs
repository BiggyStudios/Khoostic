//! Spotify for Khoostic: browser sign-in, Web API (search, playlists, liked songs, adding to
//! playlists) and streaming through librespot into Khoostic's own audio engine.
//!
//! Everything network-bound runs on a private tokio runtime. Frontends get non-blocking
//! [`Pending`] results they can poll once per frame, plus a [`Status`] to show.

pub mod api;
pub mod auth;
pub mod source;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context;
use crossbeam_channel::Receiver;
use khoostic_audio::{AudioSource, SourceError};
use khoostic_core::{Track, TrackLocator};
use khoostic_player::SourceResolver;
use librespot_core::authentication::Credentials;
use librespot_core::cache::Cache;
use librespot_core::config::SessionConfig;
use librespot_core::session::Session;
use parking_lot::Mutex;
use tokio::sync::oneshot;

pub use api::{AlbumSummary, Playlist, SearchResults};
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

/// A result being computed in the background. Poll it from a UI frame.
pub struct Pending<T>(Receiver<anyhow::Result<T>>);

impl<T> Pending<T> {
    pub fn poll(&self) -> Option<anyhow::Result<T>> {
        self.0.try_recv().ok()
    }

    pub fn wait(self) -> anyhow::Result<T> {
        self.0
            .recv()
            .unwrap_or_else(|_| Err(anyhow::anyhow!("Spotify worker stopped")))
    }
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
                inner.connect(tokens).await
            }
            .await;
            if let Err(e) = result {
                inner.set(Status::Failed(format!("{e:#}")));
            }
        });
    }

    pub fn cancel_sign_in(&self) {
        if let Some(tx) = self.inner.cancel.lock().take() {
            let _ = tx.send(());
        }
    }

    /// Reconnects silently using the stored refresh token (call at startup).
    pub fn restore(&self) {
        let Some(tokens) = self.inner.load_tokens() else {
            return;
        };
        self.inner.set(Status::Connecting);
        let inner = self.inner.clone();
        self.rt.spawn(async move {
            let result = async {
                let tokens =
                    auth::refresh(&inner.http, &inner.client_id, &tokens.refresh_token).await?;
                inner.connect(tokens).await
            }
            .await;
            if let Err(e) = result {
                tracing::warn!("Spotify restore failed: {e:#}");
                inner.set(Status::Failed(
                    "Spotify session expired — sign in again".into(),
                ));
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

    fn spawn<T, F>(&self, fut: F) -> Pending<T>
    where
        T: Send + 'static,
        F: std::future::Future<Output = anyhow::Result<T>> + Send + 'static,
    {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.rt.spawn(async move {
            let _ = tx.send(fut.await);
        });
        Pending(rx)
    }

    pub fn search(&self, query: &str, limit: u32) -> Pending<SearchResults> {
        let (inner, q) = (self.inner.clone(), query.to_owned());
        self.spawn(async move {
            let token = inner.access_token().await?;
            api::Api {
                http: &inner.http,
                token: &token,
            }
            .search(&q, limit)
            .await
        })
    }

    pub fn playlists(&self) -> Pending<Vec<Playlist>> {
        let inner = self.inner.clone();
        self.spawn(async move {
            let token = inner.access_token().await?;
            let me = inner.account()?.id;
            api::Api {
                http: &inner.http,
                token: &token,
            }
            .my_playlists(&me)
            .await
        })
    }

    pub fn playlist_tracks(&self, id: &str) -> Pending<Vec<Track>> {
        let (inner, id) = (self.inner.clone(), id.to_owned());
        self.spawn(async move {
            let token = inner.access_token().await?;
            api::Api {
                http: &inner.http,
                token: &token,
            }
            .playlist_tracks(&id)
            .await
        })
    }

    pub fn saved_tracks(&self) -> Pending<Vec<Track>> {
        let inner = self.inner.clone();
        self.spawn(async move {
            let token = inner.access_token().await?;
            api::Api {
                http: &inner.http,
                token: &token,
            }
            .saved_tracks()
            .await
        })
    }

    pub fn album_tracks(&self, id: &str) -> Pending<Vec<Track>> {
        let (inner, id) = (self.inner.clone(), id.to_owned());
        self.spawn(async move {
            let token = inner.access_token().await?;
            api::Api {
                http: &inner.http,
                token: &token,
            }
            .album_tracks(&id)
            .await
        })
    }

    /// Adds tracks to a Spotify playlist. Local tracks are skipped (Spotify can't store them)
    /// and reported in the outcome.
    pub fn add_to_playlist(&self, playlist_id: &str, tracks: &[Track]) -> Pending<AddOutcome> {
        let (ids, skipped_local) = api::spotify_ids(tracks);
        let (inner, pid) = (self.inner.clone(), playlist_id.to_owned());
        self.spawn(async move {
            let token = inner.access_token().await?;
            let added = api::Api {
                http: &inner.http,
                token: &token,
            }
            .add_to_playlist(&pid, &ids)
            .await?;
            Ok(AddOutcome {
                added,
                skipped_local,
            })
        })
    }

    /// Creates a private playlist with the Spotify tracks among `tracks`.
    pub fn create_playlist(&self, name: &str, tracks: &[Track]) -> Pending<(String, AddOutcome)> {
        let (ids, skipped_local) = api::spotify_ids(tracks);
        let (inner, name) = (self.inner.clone(), name.to_owned());
        self.spawn(async move {
            let token = inner.access_token().await?;
            let me = inner.account()?.id;
            let api = api::Api {
                http: &inner.http,
                token: &token,
            };
            let id = api.create_playlist(&me, &name).await?;
            let added = api.add_to_playlist(&id, &ids).await?;
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

    fn account(&self) -> anyhow::Result<Account> {
        match &*self.status.lock() {
            Status::Connected(a) => Ok(a.clone()),
            _ => anyhow::bail!("not connected to Spotify"),
        }
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

    /// A valid Web API token, refreshing (and persisting) it when needed.
    async fn access_token(&self) -> anyhow::Result<String> {
        let mut guard = self.tokens.lock().await;
        let t = guard.as_mut().context("not connected to Spotify")?;
        if t.stale() {
            *t = auth::refresh(&self.http, &self.client_id, &t.refresh_token).await?;
            self.save_tokens(t);
        }
        Ok(t.access_token.clone())
    }

    async fn new_session(&self, access_token: &str) -> anyhow::Result<Session> {
        let cache = Cache::new(
            Some(self.cfg.data_dir.clone()),
            None,
            Some(self.cfg.cache_dir.clone()),
            Some(AUDIO_CACHE_LIMIT),
        )
        .ok();
        let config = SessionConfig {
            client_id: self.client_id.clone(),
            ..Default::default()
        };
        let session = Session::new(config, cache);
        session
            .connect(Credentials::with_access_token(access_token), true)
            .await
            .context("connecting to Spotify")?;
        Ok(session)
    }

    async fn connect(&self, tokens: Tokens) -> anyhow::Result<()> {
        self.set(Status::Connecting);
        self.save_tokens(&tokens);
        let me = api::Api {
            http: &self.http,
            token: &tokens.access_token,
        }
        .me()
        .await?;
        let session = self.new_session(&tokens.access_token).await?;
        *self.session.lock().await = Some(session);
        *self.tokens.lock().await = Some(tokens);
        let account = Account {
            name: me.display_name.clone().unwrap_or_else(|| me.id.clone()),
            premium: me.product.as_deref() == Some("premium"),
            id: me.id,
        };
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
        let token = {
            drop(guard);
            let t = self.access_token().await?;
            guard = self.session.lock().await;
            t
        };
        let s = self.new_session(&token).await?;
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
        let (session, (file, len, duration)) =
            fetched.map_err(|e| SourceError::Unsupported(format!("{e:#}")))?;
        let src = source::open_decoder(session, file, len, duration)
            .map_err(|e| SourceError::Decode(format!("{e:#}")))?;
        Ok(Box::new(src))
    }
}
