//! Last.fm for Khoostic: browser sign-in, scrobbling with an offline queue, "now playing",
//! and similar-track lookups for radio.
//!
//! Last.fm requires an API account per application; Khoostic uses the one in the user's config
//! (`[lastfm] api_key`, `api_secret`).

pub mod client;
pub mod tracker;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

use crate::pending::Pending;
use crate::player::PlayerHandle;

pub use client::{ApiError, Client, Scrobble, Session};

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// No API key/secret configured.
    NoApiKey,
    Disconnected,
    /// The browser is open; waiting for the user to allow access.
    SigningIn,
    Connected(String),
    Failed(String),
}

/// How long to wait for the user to approve in the browser.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);
/// Retry interval for queued scrobbles after a failure.
const RETRY: Duration = Duration::from_secs(60);

pub struct Lastfm {
    client: Mutex<Option<Client>>,
    session: Mutex<Option<Session>>,
    status: Mutex<Status>,
    version: AtomicU64,
    cancel: AtomicBool,
    /// Scrobbling switch (the config's `scrobble`, changeable at runtime).
    pub scrobbling: AtomicBool,
    dir: PathBuf,
    /// Scrobbles waiting to be sent (persisted, so they survive being offline or quitting).
    spool: Mutex<Vec<Scrobble>>,
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl Lastfm {
    pub fn new(api_key: &str, api_secret: &str, scrobble: bool) -> anyhow::Result<Arc<Self>> {
        let dir = directories::ProjectDirs::from("", "", "khoostic")
            .map(|d| d.data_local_dir().join("lastfm"))
            .ok_or_else(|| anyhow::anyhow!("no home directory"))?;
        Self::with_dir(api_key, api_secret, scrobble, dir)
    }

    pub fn with_dir(
        api_key: &str,
        api_secret: &str,
        scrobble: bool,
        dir: PathBuf,
    ) -> anyhow::Result<Arc<Self>> {
        std::fs::create_dir_all(&dir)?;
        let spool = std::fs::read(dir.join("pending.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let session: Option<Session> = std::fs::read(dir.join("session.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        let lf = Arc::new(Self {
            client: Mutex::new(None),
            status: Mutex::new(Status::NoApiKey),
            session: Mutex::new(session),
            version: AtomicU64::new(0),
            cancel: AtomicBool::new(false),
            scrobbling: AtomicBool::new(scrobble),
            dir,
            spool: Mutex::new(spool),
        });
        lf.set_keys(api_key, api_secret)?;
        Ok(lf)
    }

    /// Changes the API account (e.g. pasted into Settings).
    pub fn set_keys(&self, api_key: &str, api_secret: &str) -> anyhow::Result<()> {
        let has = !api_key.trim().is_empty() && !api_secret.trim().is_empty();
        *self.client.lock() = if has {
            Some(Client::new(api_key, api_secret)?)
        } else {
            None
        };
        let status = match (has, self.session.lock().clone()) {
            (false, _) => Status::NoApiKey,
            (true, Some(s)) => Status::Connected(s.name),
            (true, None) => Status::Disconnected,
        };
        self.set(status);
        Ok(())
    }

    pub fn status(&self) -> Status {
        self.status.lock().clone()
    }

    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.status(), Status::Connected(_))
    }

    /// Scrobbles waiting to be sent.
    pub fn queued(&self) -> usize {
        self.spool.lock().len()
    }

    fn set(&self, s: Status) {
        *self.status.lock() = s;
        self.version.fetch_add(1, Ordering::AcqRel);
    }

    fn client(&self) -> anyhow::Result<Client> {
        self.client
            .lock()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("add a Last.fm API key and secret first"))
    }

    /// Opens the browser to allow Khoostic access; finishes in the background.
    pub fn sign_in(self: &Arc<Self>) {
        let Ok(client) = self.client() else {
            return self.set(Status::NoApiKey);
        };
        if self.status() == Status::SigningIn {
            return;
        }
        self.cancel.store(false, Ordering::Release);
        self.set(Status::SigningIn);
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("khoostic-lastfm-auth".into())
            .spawn(move || match me.sign_in_blocking(&client) {
                Ok(s) => {
                    tracing::info!(user = %s.name, "signed in to Last.fm");
                    let _ = std::fs::write(
                        me.dir.join("session.json"),
                        serde_json::to_vec(&s).unwrap_or_default(),
                    );
                    let name = s.name.clone();
                    *me.session.lock() = Some(s);
                    me.set(Status::Connected(name));
                }
                Err(e) if me.cancel.load(Ordering::Acquire) => {
                    tracing::debug!("Last.fm sign-in cancelled: {e:#}");
                    me.set(Status::Disconnected);
                }
                Err(e) => me.set(Status::Failed(format!("{e:#}"))),
            });
    }

    fn sign_in_blocking(&self, client: &Client) -> anyhow::Result<Session> {
        let token = client.get_token()?;
        let url = client.auth_url(&token);
        if let Err(e) = open::that_detached(&url) {
            tracing::warn!("couldn't open a browser ({e}); visit {url}");
        }
        let start = Instant::now();
        loop {
            std::thread::sleep(Duration::from_secs(2));
            if self.cancel.load(Ordering::Acquire) {
                anyhow::bail!("cancelled");
            }
            match client.get_session(&token) {
                Ok(s) => return Ok(s),
                // 14: not approved yet.
                Err(e) if e.downcast_ref::<ApiError>().is_some_and(|a| a.code == 14) => {}
                Err(e) => return Err(e),
            }
            if start.elapsed() > SIGN_IN_TIMEOUT {
                anyhow::bail!("timed out waiting for the browser");
            }
        }
    }

    pub fn cancel_sign_in(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    pub fn sign_out(&self) {
        *self.session.lock() = None;
        let _ = std::fs::remove_file(self.dir.join("session.json"));
        let status = if self.client.lock().is_some() {
            Status::Disconnected
        } else {
            Status::NoApiKey
        };
        self.set(status);
    }

    /// Songs similar to `artist` – `title` (needs only the API key, not a sign-in). Falls back
    /// to top tracks of similar artists when Last.fm knows no similar tracks. Blocking.
    pub fn similar(
        &self,
        artist: &str,
        title: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<(String, String)>> {
        let c = self.client()?;
        let mut out: Vec<(String, String)> = c
            .similar_tracks(artist, title, limit)?
            .into_iter()
            .map(|(a, t, _)| (a, t))
            .collect();
        if out.len() < limit / 2 {
            for other in c.similar_artists(artist, 6).unwrap_or_default() {
                for (a, t, _) in c.top_tracks(&other, 3).unwrap_or_default() {
                    out.push((a, t));
                }
            }
        }
        out.truncate(limit);
        Ok(out)
    }

    /// Like [`Lastfm::similar`], off the calling thread.
    pub fn similar_pending(
        self: &Arc<Self>,
        artist: &str,
        title: &str,
        limit: usize,
    ) -> Pending<Vec<(String, String)>> {
        let (me, a, t) = (self.clone(), artist.to_owned(), title.to_owned());
        Pending::spawn("khoostic-lastfm-similar", move || me.similar(&a, &t, limit))
    }

    /// Starts scrobbling what `player` plays (a background thread for the app's lifetime).
    pub fn start_scrobbler(self: &Arc<Self>, player: PlayerHandle) {
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("khoostic-scrobbler".into())
            .spawn(move || me.scrobble_loop(player));
    }

    fn scrobble_loop(&self, player: PlayerHandle) {
        let mut tracker = tracker::Tracker::default();
        let mut last = Instant::now();
        let mut next_flush = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(1000));
            let dt = last.elapsed();
            last = Instant::now();
            let events = tracker.observe(&player.snapshot(), now_unix(), dt);
            if !self.scrobbling.load(Ordering::Acquire) {
                continue;
            }
            for ev in events {
                match ev {
                    tracker::Event::NowPlaying(s) => self.now_playing(&s),
                    tracker::Event::Scrobble(s) => {
                        tracing::debug!(artist = %s.artist, track = %s.track, "scrobble queued");
                        self.spool.lock().push(s);
                        self.save_spool();
                        next_flush = Instant::now();
                    }
                }
            }
            if Instant::now() >= next_flush && !self.spool.lock().is_empty() {
                next_flush = match self.flush() {
                    Ok(()) => Instant::now(),
                    Err(e) => {
                        tracing::debug!("scrobbles kept for later: {e:#}");
                        Instant::now() + RETRY
                    }
                };
            }
        }
    }

    fn session_key(&self) -> Option<String> {
        self.session.lock().as_ref().map(|s| s.key.clone())
    }

    fn now_playing(&self, s: &Scrobble) {
        let (Some(sk), Ok(c)) = (self.session_key(), self.client()) else {
            return;
        };
        if let Err(e) = c.update_now_playing(&sk, s) {
            self.check_auth(&e);
            tracing::debug!("now playing: {e:#}");
        }
    }

    /// Sends queued scrobbles, oldest first, 50 at a time.
    pub fn flush(&self) -> anyhow::Result<()> {
        let sk = self
            .session_key()
            .ok_or_else(|| anyhow::anyhow!("not signed in"))?;
        let c = self.client()?;
        loop {
            let batch: Vec<Scrobble> = self.spool.lock().iter().take(50).cloned().collect();
            if batch.is_empty() {
                return Ok(());
            }
            if let Err(e) = c.scrobble(&sk, &batch) {
                self.check_auth(&e);
                return Err(e);
            }
            // Accepted or ignored by Last.fm: either way, never resend these.
            self.spool.lock().drain(..batch.len());
            self.save_spool();
        }
    }

    fn check_auth(&self, e: &anyhow::Error) {
        if e.downcast_ref::<ApiError>().is_some_and(ApiError::is_auth) {
            *self.session.lock() = None;
            let _ = std::fs::remove_file(self.dir.join("session.json"));
            self.set(Status::Failed(
                "Last.fm session expired; sign in again".into(),
            ));
        }
    }

    fn save_spool(&self) {
        let json = serde_json::to_vec(&*self.spool.lock()).unwrap_or_default();
        let _ = std::fs::write(self.dir.join("pending.json"), json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "khoostic-lastfm-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn status_follows_keys_and_session() {
        let dir = temp_dir("status");
        let lf = Lastfm::with_dir("", "", true, dir.clone()).unwrap();
        assert_eq!(lf.status(), Status::NoApiKey);
        lf.set_keys("k", "s").unwrap();
        assert_eq!(lf.status(), Status::Disconnected);
        std::fs::write(dir.join("session.json"), br#"{"name":"me","key":"sk"}"#).unwrap();
        let lf = Lastfm::with_dir("k", "s", true, dir.clone()).unwrap();
        assert_eq!(lf.status(), Status::Connected("me".into()));
        lf.sign_out();
        assert_eq!(lf.status(), Status::Disconnected);
        assert!(!dir.join("session.json").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn queued_scrobbles_survive_restarts() {
        let dir = temp_dir("spool");
        let lf = Lastfm::with_dir("k", "s", true, dir.clone()).unwrap();
        lf.spool.lock().push(Scrobble {
            artist: "A".into(),
            track: "T".into(),
            album: None,
            album_artist: None,
            duration_secs: Some(200),
            timestamp: 1,
        });
        lf.save_spool();
        assert!(lf.flush().is_err(), "not signed in: kept");
        let again = Lastfm::with_dir("k", "s", true, dir.clone()).unwrap();
        assert_eq!(again.queued(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
