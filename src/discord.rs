//! Discord Rich Presence: "Listening to …" on your Discord profile, with the song, artist,
//! cover, a progress bar, and (in a jam) how many are listening along.
//!
//! It talks to the Discord desktop app over its local IPC (the `discord-ipc-N` named pipe on
//! Windows, a Unix socket elsewhere); nothing goes over the network from here, except a cover
//! lookup on Deezer's public API for your own songs (Discord shows images by URL). Discord
//! uses Khoostic's Discord application ([`APP_ID`]) unless the config names another.
//!
//! A background thread follows the player, reconnects when Discord starts later, and only
//! sends when something visible changed (Discord allows 5 updates per 20 seconds).

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::config::DiscordConfig;
use crate::model::{CoverRef, Track, TrackLocator};
use crate::player::{PlayerHandle, PlayerSnapshot, State};

/// Khoostic's Discord application (its name is what Discord shows). Used when the config names
/// none; it's public, like every application id.
pub const APP_ID: &str = "1378431160027451534";

/// The configured application id, or Khoostic's own.
fn app_id(configured: &str) -> String {
    match configured.trim() {
        "" => APP_ID.to_owned(),
        id => id.to_owned(),
    }
}

/// How often to try again while Discord isn't running.
const RETRY: Duration = Duration::from_secs(15);
/// Discord's limit for activity updates.
const RATE: (usize, Duration) = (5, Duration::from_secs(20));
/// Progress drifting less than this from what Discord shows isn't worth an update.
const DRIFT_MS: i64 = 2500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Turned off in the settings.
    Off,
    /// On, but there's no application id to connect with.
    NoClientId,
    /// Discord isn't running (or doesn't accept IPC); retried every few seconds.
    NotRunning,
    /// Connected; the Discord user's name.
    Connected(String),
    /// Discord refused (a wrong application id, usually).
    Failed(String),
}

/// What a jam adds to the presence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Party {
    /// Stable id for this jam (people in the same jam share it).
    pub id: String,
    /// Everyone in the jam, host included.
    pub size: u32,
    /// "Hosting a jam", "In Sam's jam".
    pub label: String,
}

pub struct Discord {
    inner: Arc<Inner>,
}

struct Inner {
    enabled: AtomicBool,
    client_id: Mutex<String>,
    status: Mutex<Status>,
    party: Mutex<Option<Party>>,
    /// Cover URLs for songs without one (looked up on Deezer), by "artist\ttitle".
    covers: Mutex<HashMap<String, Option<String>>>,
    wake: Sender<()>,
    http: Option<reqwest::blocking::Client>,
}

impl Discord {
    pub fn start(config: &DiscordConfig, player: PlayerHandle) -> Arc<Self> {
        let (wake, woken) = crossbeam_channel::bounded(1);
        let http = reqwest::blocking::Client::builder()
            .user_agent("Khoostic/0.1 ( music player )")
            .timeout(Duration::from_secs(10))
            .build()
            .ok();
        let inner = Arc::new(Inner {
            enabled: AtomicBool::new(config.enabled),
            client_id: Mutex::new(app_id(&config.client_id)),
            status: Mutex::new(Status::Off),
            party: Mutex::new(None),
            covers: Mutex::default(),
            wake,
            http,
        });
        let me = inner.clone();
        let _ = thread::Builder::new()
            .name("khoostic-discord".into())
            .spawn(move || Worker::new(me, player).run(&woken));
        Arc::new(Self { inner })
    }

    pub fn status(&self) -> Status {
        self.inner.status.lock().clone()
    }

    pub fn enabled(&self) -> bool {
        self.inner.enabled.load(Ordering::Acquire)
    }

    pub fn set_enabled(&self, on: bool) {
        self.inner.enabled.store(on, Ordering::Release);
        self.inner.wake();
    }

    /// Shows (or stops showing) a jam.
    pub fn set_party(&self, party: Option<Party>) {
        *self.inner.party.lock() = party;
        self.inner.wake();
    }
}

impl Inner {
    fn wake(&self) {
        let _ = self.wake.try_send(());
    }

    fn set_status(&self, s: Status) {
        *self.status.lock() = s;
    }

    /// A cover URL Discord can show, if there is one (looked up in the background).
    fn cover(self: &Arc<Self>, track: &Track) -> Option<String> {
        if let Some(CoverRef::Url(u)) = &track.cover
            && u.starts_with("https://")
        {
            return Some(u.clone());
        }
        let artist = track.artists.first()?.clone();
        let key = format!("{artist}\t{}", track.title);
        let mut covers = self.covers.lock();
        if let Some(hit) = covers.get(&key) {
            return hit.clone();
        }
        covers.insert(key.clone(), None);
        let (me, title) = (self.clone(), track.title.clone());
        let _ = thread::Builder::new()
            .name("khoostic-discord-cover".into())
            .spawn(move || {
                let found = me.deezer_cover(&artist, &title).unwrap_or_else(|e| {
                    tracing::debug!("cover for {artist} – {title}: {e:#}");
                    None
                });
                if found.is_some() {
                    me.covers.lock().insert(key, found);
                    me.wake();
                }
            });
        None
    }

    fn deezer_cover(&self, artist: &str, title: &str) -> anyhow::Result<Option<String>> {
        let Some(http) = &self.http else {
            return Ok(None);
        };
        let q = format!("artist:\"{artist}\" track:\"{title}\"");
        let url = url::Url::parse_with_params(
            "https://api.deezer.com/search/track",
            &[("q", q.as_str()), ("limit", "1")],
        )?;
        let v: Value = http.get(url).send()?.error_for_status()?.json()?;
        Ok(v["data"][0]["album"]["cover_big"]
            .as_str()
            .filter(|u| u.starts_with("https://"))
            .map(str::to_owned))
    }
}

/// What's on Discord now, to tell whether an update is worth sending.
#[derive(Clone, PartialEq)]
struct Shown {
    activity: Option<Value>,
    /// When the song started (ms since the epoch), for drift checks.
    start: Option<i64>,
}

struct Worker {
    inner: Arc<Inner>,
    player: PlayerHandle,
    conn: Option<Conn>,
    /// The application id `conn` was opened with.
    conn_id: String,
    last_try: Option<Instant>,
    shown: Option<Shown>,
    sent: VecDeque<Instant>,
    /// Discord turned down a field we use (older clients): send the plainest activity.
    plain: bool,
}

impl Worker {
    fn new(inner: Arc<Inner>, player: PlayerHandle) -> Self {
        Self {
            inner,
            player,
            conn: None,
            conn_id: String::new(),
            last_try: None,
            shown: None,
            sent: VecDeque::new(),
            plain: false,
        }
    }

    fn run(&mut self, woken: &Receiver<()>) {
        loop {
            match woken.recv_timeout(Duration::from_secs(1)) {
                Ok(()) | Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(_) => return,
            }
            self.tick();
        }
    }

    fn tick(&mut self) {
        let id = self.inner.client_id.lock().clone();
        if !self.inner.enabled.load(Ordering::Acquire) {
            self.disconnect();
            return self.inner.set_status(Status::Off);
        }
        if id.is_empty() {
            self.disconnect();
            return self.inner.set_status(Status::NoClientId);
        }
        if self.conn.is_some() && self.conn_id != id {
            self.disconnect();
            self.last_try = None;
        }
        if self.conn.is_none() && !self.connect(&id) {
            return;
        }

        let snap = self.player.snapshot();
        let party = self.inner.party.lock().clone();
        let start = start_ms(&snap);
        let activity = activity(&self.inner, &snap, party.as_ref(), self.plain);
        let want = Shown { activity, start };
        if let Some(shown) = &self.shown {
            let same_start = match (shown.start, want.start) {
                (Some(a), Some(b)) => (a - b).abs() < DRIFT_MS,
                (a, b) => a == b,
            };
            if same_start && strip_times(&shown.activity) == strip_times(&want.activity) {
                return;
            }
        }
        // Over the limit: try again on a later tick.
        let now = Instant::now();
        while self.sent.front().is_some_and(|t| now - *t > RATE.1) {
            self.sent.pop_front();
        }
        if self.sent.len() >= RATE.0 {
            return;
        }
        self.sent.push_back(now);
        match self.set_activity(want.activity.clone()) {
            Ok(()) => self.shown = Some(want),
            Err(e) => {
                tracing::info!("Discord connection lost: {e}");
                self.disconnect();
                self.inner.set_status(Status::NotRunning);
            }
        }
    }

    fn connect(&mut self, id: &str) -> bool {
        if self.last_try.is_some_and(|t| t.elapsed() < RETRY) {
            return false;
        }
        self.last_try = Some(Instant::now());
        let Some(mut conn) = Conn::open() else {
            self.inner.set_status(Status::NotRunning);
            return false;
        };
        match conn.handshake(id) {
            Ok(user) => {
                tracing::info!(%user, "connected to Discord");
                self.inner.set_status(Status::Connected(user));
                self.conn = Some(conn);
                self.conn_id = id.to_owned();
                self.shown = None;
                self.plain = false;
                true
            }
            Err(e) => {
                self.inner.set_status(Status::Failed(e));
                false
            }
        }
    }

    fn disconnect(&mut self) {
        // Closing the connection clears the activity on Discord's side.
        self.conn = None;
        self.shown = None;
    }

    fn set_activity(&mut self, activity: Option<Value>) -> io::Result<()> {
        let Some(conn) = &mut self.conn else {
            return Ok(());
        };
        let reply = conn.command("SET_ACTIVITY", activity.clone())?;
        if reply["evt"] == "ERROR" {
            let msg = reply["data"]["message"].as_str().unwrap_or("?").to_owned();
            tracing::warn!("Discord turned down the activity: {msg}");
            if !self.plain && activity.is_some() {
                // Retry once without the newer fields.
                self.plain = true;
                let snap = self.player.snapshot();
                let party = self.inner.party.lock().clone();
                let plain = self::activity(&self.inner, &snap, party.as_ref(), true);
                conn.command("SET_ACTIVITY", plain)?;
            }
        }
        Ok(())
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// When the playing song started, by the wall clock (None unless playing).
fn start_ms(snap: &PlayerSnapshot) -> Option<i64> {
    (snap.state == State::Playing && snap.current.is_some())
        .then(|| now_ms() - snap.position.as_millis() as i64)
}

/// The activity minus its timestamps (those are compared with some slack instead).
fn strip_times(a: &Option<Value>) -> Option<Value> {
    a.clone().map(|mut v| {
        if let Some(o) = v.as_object_mut() {
            o.remove("timestamps");
        }
        v
    })
}

/// Discord wants 2–128 characters.
fn field(s: &str) -> String {
    let mut s: String = s.chars().take(128).collect();
    while s.chars().count() < 2 {
        s.push('\u{2800}'); // a blank that isn't whitespace (Discord trims spaces)
    }
    s
}

fn activity(
    inner: &Arc<Inner>,
    snap: &PlayerSnapshot,
    party: Option<&Party>,
    plain: bool,
) -> Option<Value> {
    let entry = snap.current.as_ref()?;
    if snap.state == State::Stopped && !snap.loading {
        return None;
    }
    let t = &entry.track;
    let artists = if t.artists.is_empty() {
        "Unknown artist".to_owned()
    } else {
        t.artists.join(", ")
    };
    let paused = snap.state == State::Paused;
    let mut a = json!({
        "details": field(&t.title),
        "state": field(&if paused { format!("Paused · {artists}") } else { artists }),
        "instance": false,
    });
    if let Some(start) = start_ms(snap) {
        let mut ts = json!({ "start": start });
        if let Some(d) = snap.duration.or(t.duration) {
            ts["end"] = json!(start + d.as_millis() as i64);
        }
        a["timestamps"] = ts;
    }
    let mut assets = serde_json::Map::new();
    if let Some(url) = inner.cover(t) {
        assets.insert("large_image".into(), json!(url));
        if let Some(album) = t.album.as_deref().filter(|a| !a.is_empty()) {
            assets.insert("large_text".into(), json!(field(album)));
        }
    }
    if !assets.is_empty() {
        a["assets"] = Value::Object(assets);
    }
    if let Some(p) = party {
        a["party"] = json!({ "id": p.id, "size": [p.size.max(1), p.size.max(1).max(8)] });
        a["state"] = json!(field(&format!(
            "{} · {}",
            a["state"].as_str().unwrap_or(""),
            p.label
        )));
    }
    if !plain {
        // "Listening to …" instead of "Playing …", and the song (not the app) in the member list.
        a["type"] = json!(2);
        a["status_display_type"] = json!(2);
        if let TrackLocator::Spotify { id } = &t.locator {
            a["buttons"] = json!([{
                "label": "Open in Spotify",
                "url": format!("https://open.spotify.com/track/{id}"),
            }]);
        }
    }
    Some(a)
}

/// One IPC connection to the Discord app.
struct Conn {
    stream: Box<dyn Stream>,
    nonce: u64,
}

trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const OP_CLOSE: u32 = 2;

impl Conn {
    /// The first Discord (stable, PTB, Canary each take a number) that answers.
    fn open() -> Option<Self> {
        (0..10).find_map(|i| open_socket(i).ok().map(|stream| Self { stream, nonce: 0 }))
    }

    /// Says hello; Discord answers with the signed-in user, or closes on a bad id.
    fn handshake(&mut self, client_id: &str) -> Result<String, String> {
        let hello = json!({ "v": 1, "client_id": client_id });
        let io = |e: io::Error| format!("Discord: {e}");
        self.write(OP_HANDSHAKE, &hello).map_err(io)?;
        let (op, v) = self.read().map_err(io)?;
        if op == OP_CLOSE || v["evt"] != "READY" {
            let msg = v["message"].as_str().unwrap_or("connection refused");
            return Err(format!("Discord said: {msg} (check the application id)"));
        }
        let user = &v["data"]["user"];
        Ok(user["global_name"]
            .as_str()
            .or(user["username"].as_str())
            .unwrap_or("you")
            .to_owned())
    }

    fn command(&mut self, cmd: &str, activity: Option<Value>) -> io::Result<Value> {
        self.nonce += 1;
        let msg = json!({
            "cmd": cmd,
            "args": { "pid": std::process::id(), "activity": activity },
            "nonce": self.nonce.to_string(),
        });
        self.write(OP_FRAME, &msg)?;
        loop {
            let (op, v) = self.read()?;
            if op == OP_CLOSE {
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "closed"));
            }
            if v["nonce"] == msg["nonce"] {
                return Ok(v);
            }
        }
    }

    fn write(&mut self, op: u32, v: &Value) -> io::Result<()> {
        let body = serde_json::to_vec(v)?;
        let mut frame = Vec::with_capacity(8 + body.len());
        frame.extend_from_slice(&op.to_le_bytes());
        frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
        frame.extend_from_slice(&body);
        self.stream.write_all(&frame)?;
        self.stream.flush()
    }

    fn read(&mut self) -> io::Result<(u32, Value)> {
        let mut head = [0u8; 8];
        self.stream.read_exact(&mut head)?;
        let op = u32::from_le_bytes(head[..4].try_into().expect("4 bytes"));
        let len = u32::from_le_bytes(head[4..].try_into().expect("4 bytes")) as usize;
        if len > 1 << 20 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too big"));
        }
        let mut body = vec![0; len];
        self.stream.read_exact(&mut body)?;
        Ok((op, serde_json::from_slice(&body).unwrap_or(Value::Null)))
    }
}

#[cfg(windows)]
fn open_socket(i: u32) -> io::Result<Box<dyn Stream>> {
    let pipe = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(format!(r"\\?\pipe\discord-ipc-{i}"))?;
    Ok(Box::new(pipe))
}

#[cfg(unix)]
fn open_socket(i: u32) -> io::Result<Box<dyn Stream>> {
    use std::os::unix::net::UnixStream;
    let mut dirs: Vec<std::path::PathBuf> = ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"]
        .iter()
        .filter_map(|k| std::env::var_os(k).map(Into::into))
        .collect();
    dirs.push("/tmp".into());
    let name = format!("discord-ipc-{i}");
    for dir in dirs {
        // Flatpak and Snap Discord keep the socket a level down.
        for sub in ["", "app/com.discordapp.Discord", "snap.discord"] {
            if let Ok(s) = UnixStream::connect(dir.join(sub).join(&name)) {
                return Ok(Box::new(s));
            }
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "no Discord socket"))
}

#[cfg(not(any(windows, unix)))]
fn open_socket(_: u32) -> io::Result<Box<dyn Stream>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no Discord IPC here",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EntryId, QueueEntry};

    fn inner() -> Arc<Inner> {
        let (wake, _) = crossbeam_channel::bounded(1);
        Arc::new(Inner {
            enabled: AtomicBool::new(true),
            client_id: Mutex::new("1".into()),
            status: Mutex::new(Status::Off),
            party: Mutex::new(None),
            covers: Mutex::default(),
            wake,
            http: None,
        })
    }

    fn snap(state: State, pos: u64) -> PlayerSnapshot {
        PlayerSnapshot {
            state,
            position: Duration::from_secs(pos),
            duration: Some(Duration::from_secs(200)),
            current: Some(QueueEntry {
                id: EntryId(1),
                track: Arc::new(Track {
                    title: "X".into(),
                    artists: vec!["Kendrick Lamar".into()],
                    album: Some("good kid, m.A.A.d city".into()),
                    locator: TrackLocator::Spotify { id: "abc".into() },
                    cover: Some(CoverRef::Url("https://i.scdn.co/image/1".into())),
                    ..Default::default()
                }),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn playing_shows_song_progress_cover_and_link() {
        let a = activity(&inner(), &snap(State::Playing, 30), None, false).unwrap();
        assert_eq!(a["details"].as_str().unwrap().chars().count(), 2, "padded");
        assert_eq!(a["state"], "Kendrick Lamar");
        assert_eq!(a["type"], 2);
        let ts = &a["timestamps"];
        assert_eq!(
            ts["end"].as_i64().unwrap() - ts["start"].as_i64().unwrap(),
            200_000
        );
        assert!((now_ms() - ts["start"].as_i64().unwrap() - 30_000).abs() < 1000);
        assert_eq!(a["assets"]["large_image"], "https://i.scdn.co/image/1");
        assert_eq!(a["assets"]["large_text"], "good kid, m.A.A.d city");
        assert_eq!(a["buttons"][0]["url"], "https://open.spotify.com/track/abc");
    }

    #[test]
    fn paused_has_no_clock_stopped_shows_nothing_plain_drops_new_fields() {
        let i = inner();
        let a = activity(&i, &snap(State::Paused, 30), None, false).unwrap();
        assert!(a.get("timestamps").is_none());
        assert_eq!(a["state"], "Paused · Kendrick Lamar");
        assert!(activity(&i, &snap(State::Stopped, 0), None, false).is_none());
        let a = activity(&i, &snap(State::Playing, 0), None, true).unwrap();
        assert!(a.get("type").is_none() && a.get("buttons").is_none());
    }

    #[test]
    fn a_jam_shows_as_a_party() {
        let p = Party {
            id: "jam-1".into(),
            size: 3,
            label: "Hosting a jam".into(),
        };
        let a = activity(&inner(), &snap(State::Playing, 0), Some(&p), false).unwrap();
        assert_eq!(a["party"]["size"], json!([3, 8]));
        assert_eq!(a["state"], "Kendrick Lamar · Hosting a jam");
    }

    #[test]
    fn frames_round_trip() {
        struct Pipe(io::Cursor<Vec<u8>>);
        impl Read for Pipe {
            fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
                self.0.read(b)
            }
        }
        impl Write for Pipe {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                self.0.get_mut().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut c = Conn {
            stream: Box::new(Pipe(io::Cursor::new(vec![]))),
            nonce: 0,
        };
        c.write(OP_FRAME, &json!({"evt": "READY"})).unwrap();
        let (op, v) = c.read().unwrap();
        assert_eq!((op, v["evt"].as_str()), (OP_FRAME, Some("READY")));
    }
}
