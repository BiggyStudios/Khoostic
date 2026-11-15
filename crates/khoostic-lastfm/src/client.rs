//! Last.fm API 2.0: signed calls, desktop auth, scrobbling and similar tracks.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Context;
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const API: &str = "https://ws.audioscrobbler.com/2.0/";

/// One listen, as Last.fm wants it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scrobble {
    pub artist: String,
    pub track: String,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub duration_secs: Option<u32>,
    /// Unix time the track started playing.
    pub timestamp: i64,
}

/// An error Last.fm returned (as opposed to a network failure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub code: i64,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Last.fm error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

impl ApiError {
    /// The session key is no longer valid; the user must sign in again.
    pub fn is_auth(&self) -> bool {
        matches!(self.code, 4 | 9 | 10 | 26)
    }
}

/// Signs a call: md5 of the parameters sorted by name, concatenated, plus the secret.
pub fn signature(params: &BTreeMap<String, String>, secret: &str) -> String {
    let mut h = Md5::new();
    for (k, v) in params {
        if k == "format" || k == "callback" {
            continue;
        }
        h.update(k.as_bytes());
        h.update(v.as_bytes());
    }
    h.update(secret.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Session {
    pub name: String,
    pub key: String,
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::blocking::Client,
    pub key: String,
    secret: String,
    base: String,
}

impl Client {
    pub fn new(key: &str, secret: &str) -> anyhow::Result<Self> {
        Ok(Self {
            http: reqwest::blocking::Client::builder()
                .user_agent("khoostic")
                .timeout(Duration::from_secs(15))
                .build()?,
            key: key.trim().to_owned(),
            secret: secret.trim().to_owned(),
            base: API.to_owned(),
        })
    }

    /// Points the client at another server (tests).
    pub fn with_base(mut self, base: &str) -> Self {
        self.base = base.to_owned();
        self
    }

    fn call(
        &self,
        method: &str,
        params: &[(&str, String)],
        signed: bool,
        post: bool,
    ) -> anyhow::Result<Value> {
        let mut all: BTreeMap<String, String> = params
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        all.insert("method".into(), method.into());
        all.insert("api_key".into(), self.key.clone());
        if signed {
            let sig = signature(&all, &self.secret);
            all.insert("api_sig".into(), sig);
        }
        all.insert("format".into(), "json".into());
        let req = if post {
            self.http.post(&self.base).form(&all)
        } else {
            let mut u = url::Url::parse(&self.base).context("bad Last.fm API url")?;
            u.query_pairs_mut().extend_pairs(&all);
            self.http.get(u.as_str())
        };
        let resp = req
            .send()
            .with_context(|| format!("calling Last.fm ({method})"))?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .with_context(|| format!("Last.fm answered {status} without JSON"))?;
        if let Some(code) = body.get("error").and_then(Value::as_i64) {
            return Err(ApiError {
                code,
                message: body["message"].as_str().unwrap_or("").to_owned(),
            }
            .into());
        }
        Ok(body)
    }

    /// Step 1 of desktop auth: a token the user approves in the browser.
    pub fn get_token(&self) -> anyhow::Result<String> {
        let v = self.call("auth.getToken", &[], true, false)?;
        v["token"]
            .as_str()
            .map(str::to_owned)
            .context("Last.fm sent no token")
    }

    pub fn auth_url(&self, token: &str) -> String {
        let mut u = url::Url::parse("https://www.last.fm/api/auth/").expect("static url");
        u.query_pairs_mut()
            .append_pair("api_key", &self.key)
            .append_pair("token", token);
        u.into()
    }

    /// Step 2: exchanges an approved token for a session. Error 14 means "not approved yet".
    pub fn get_session(&self, token: &str) -> anyhow::Result<Session> {
        let v = self.call("auth.getSession", &[("token", token.into())], true, false)?;
        Ok(Session {
            name: v["session"]["name"]
                .as_str()
                .context("no session name")?
                .to_owned(),
            key: v["session"]["key"]
                .as_str()
                .context("no session key")?
                .to_owned(),
        })
    }

    pub fn update_now_playing(&self, sk: &str, s: &Scrobble) -> anyhow::Result<()> {
        let mut p = vec![
            ("artist", s.artist.clone()),
            ("track", s.track.clone()),
            ("sk", sk.to_owned()),
        ];
        if let Some(a) = &s.album {
            p.push(("album", a.clone()));
        }
        if let Some(d) = s.duration_secs {
            p.push(("duration", d.to_string()));
        }
        self.call("track.updateNowPlaying", &p, true, true)?;
        Ok(())
    }

    /// Sends up to 50 scrobbles. Returns how many Last.fm accepted (ignored ones, e.g. with
    /// a bad timestamp, are dropped by the server and shouldn't be retried).
    pub fn scrobble(&self, sk: &str, batch: &[Scrobble]) -> anyhow::Result<usize> {
        let keys: Vec<String> = (0..batch.len()).flat_map(indexed_keys).collect();
        let mut p: Vec<(&str, String)> = vec![("sk", sk.to_owned())];
        for (i, s) in batch.iter().enumerate().take(50) {
            let k = |n: usize| keys[i * 5 + n].as_str();
            p.push((k(0), s.artist.clone()));
            p.push((k(1), s.track.clone()));
            p.push((k(2), s.timestamp.to_string()));
            if let Some(a) = &s.album {
                p.push((k(3), a.clone()));
            }
            if let Some(d) = s.duration_secs {
                p.push((k(4), d.to_string()));
            }
        }
        let v = self.call("track.scrobble", &p, true, true)?;
        Ok(v["scrobbles"]["@attr"]["accepted"]
            .as_i64()
            .unwrap_or(batch.len() as i64) as usize)
    }

    /// Songs similar to `artist` – `title`, best first: `(artist, title, match 0..1)`.
    pub fn similar_tracks(
        &self,
        artist: &str,
        title: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<(String, String, f32)>> {
        let v = self.call(
            "track.getSimilar",
            &[
                ("artist", artist.into()),
                ("track", title.into()),
                ("autocorrect", "1".into()),
                ("limit", limit.to_string()),
            ],
            false,
            false,
        )?;
        Ok(parse_tracks(&v["similartracks"]["track"]))
    }

    /// Artists similar to `artist`, best first.
    pub fn similar_artists(&self, artist: &str, limit: usize) -> anyhow::Result<Vec<String>> {
        let v = self.call(
            "artist.getSimilar",
            &[
                ("artist", artist.into()),
                ("autocorrect", "1".into()),
                ("limit", limit.to_string()),
            ],
            false,
            false,
        )?;
        Ok(v["similarartists"]["artist"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x["name"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default())
    }

    pub fn top_tracks(
        &self,
        artist: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<(String, String, f32)>> {
        let v = self.call(
            "artist.getTopTracks",
            &[
                ("artist", artist.into()),
                ("autocorrect", "1".into()),
                ("limit", limit.to_string()),
            ],
            false,
            false,
        )?;
        Ok(parse_tracks(&v["toptracks"]["track"]))
    }
}

fn indexed_keys(i: usize) -> [String; 5] {
    [
        format!("artist[{i}]"),
        format!("track[{i}]"),
        format!("timestamp[{i}]"),
        format!("album[{i}]"),
        format!("duration[{i}]"),
    ]
}

fn parse_tracks(v: &Value) -> Vec<(String, String, f32)> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| {
                    let title = t["name"].as_str()?;
                    let artist = t["artist"]["name"].as_str()?;
                    let m = match &t["match"] {
                        Value::String(s) => s.parse().unwrap_or(0.0),
                        Value::Number(n) => n.as_f64().unwrap_or(0.0) as f32,
                        _ => 0.0,
                    };
                    Some((artist.to_owned(), title.to_owned(), m))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_matches_the_documented_scheme() {
        // md5("api_keyxxxxmethodauth.getSessiontokenyyyy" + "ilovecher"), computed by hand
        // following https://www.last.fm/api/authspec#_8-signing-calls; `format` is excluded.
        let mut p = BTreeMap::new();
        p.insert("api_key".to_string(), "xxxx".to_string());
        p.insert("method".to_string(), "auth.getSession".to_string());
        p.insert("token".to_string(), "yyyy".to_string());
        p.insert("format".to_string(), "json".to_string());
        let mut h = Md5::new();
        h.update(b"api_keyxxxxmethodauth.getSessiontokenyyyyilovecher");
        let want: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(signature(&p, "ilovecher"), want);
        assert_eq!(want.len(), 32);
    }

    /// A one-request-per-connection HTTP server answering with `bodies` in order; returns its
    /// base URL and the raw requests it saw.
    fn fake_server(bodies: Vec<&'static str>) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/2.0/", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for body in bodies {
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = s.read(&mut chunk).unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text
                            .lines()
                            .find_map(|l| {
                                l.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if buf.len() >= end + 4 + len {
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                tx.send(String::from_utf8_lossy(&buf).to_string()).unwrap();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                s.write_all(resp.as_bytes()).unwrap();
            }
        });
        (base, rx)
    }

    #[test]
    fn talks_the_api_protocol() {
        let (base, seen) = fake_server(vec![
            r#"{"error":14,"message":"Unauthorized Token - This token has not been authorized"}"#,
            r#"{"session":{"name":"me","key":"SK","subscriber":0}}"#,
            r#"{"scrobbles":{"@attr":{"accepted":1,"ignored":0}}}"#,
        ]);
        let c = Client::new("KEY", "SECRET").unwrap().with_base(&base);
        let e = c.get_session("TOK").unwrap_err();
        assert_eq!(e.downcast_ref::<ApiError>().map(|a| a.code), Some(14));
        let first = seen.recv().unwrap();
        assert!(first.starts_with("GET /2.0/?"), "{first}");
        assert!(first.contains("method=auth.getSession") && first.contains("api_sig="));
        let s = c.get_session("TOK").unwrap();
        assert_eq!(
            s,
            Session {
                name: "me".into(),
                key: "SK".into()
            }
        );
        seen.recv().unwrap();
        let sc = Scrobble {
            artist: "Daft Punk".into(),
            track: "One More Time".into(),
            album: Some("Discovery".into()),
            album_artist: None,
            duration_secs: Some(320),
            timestamp: 1_700_000_000,
        };
        assert_eq!(c.scrobble("SK", &[sc]).unwrap(), 1);
        let post = seen.recv().unwrap();
        assert!(post.starts_with("POST "), "{post}");
        let body = post.split("\r\n\r\n").nth(1).unwrap();
        for want in [
            "method=track.scrobble",
            "sk=SK",
            "artist%5B0%5D=Daft+Punk",
            "timestamp%5B0%5D=1700000000",
            "api_sig=",
            "format=json",
        ] {
            assert!(body.contains(want), "missing {want} in {body}");
        }
    }

    #[test]
    fn parses_similar_tracks() {
        let v: Value = serde_json::from_str(
            r#"{"similartracks":{"track":[
                {"name":"Afraid","artist":{"name":"The Neighbourhood"},"match":0.92},
                {"name":"R.I.P. 2 My Youth","artist":{"name":"The Neighbourhood"},"match":"0.8"},
                {"name":"broken"}
            ]}}"#,
        )
        .unwrap();
        let t = parse_tracks(&v["similartracks"]["track"]);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0], ("The Neighbourhood".into(), "Afraid".into(), 0.92));
        assert!((t[1].2 - 0.8).abs() < 1e-6);
    }
}
