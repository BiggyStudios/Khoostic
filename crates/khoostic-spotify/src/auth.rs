//! Spotify sign-in: OAuth 2 authorization code flow with PKCE, completed in the user's own
//! browser. A tiny one-shot HTTP listener on 127.0.0.1 receives the redirect.
//!
//! No client secret is involved (PKCE), so the client id can ship in the binary. By default we
//! use the same public client id as librespot and other open-source clients, which is what makes
//! "click a button, log in, done" possible without registering a developer app.

use std::time::{Duration, SystemTime};

use anyhow::{Context, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// Spotify's public desktop client id, as used by librespot.
pub const DEFAULT_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
pub const REDIRECT_PORT: u16 = 8898;
pub const REDIRECT_URI: &str = "http://127.0.0.1:8898/login";
const AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
/// How long we wait for the user to finish in the browser.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

pub const SCOPES: &[&str] = &[
    "streaming",
    "user-read-private",
    "user-read-email",
    "playlist-read-private",
    "playlist-read-collaborative",
    "playlist-modify-private",
    "playlist-modify-public",
    "user-library-read",
    "user-library-modify",
    "user-top-read",
    "user-read-recently-played",
    "user-follow-read",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: SystemTime,
}

impl Tokens {
    /// True if the access token is expired or about to be.
    pub fn stale(&self) -> bool {
        self.expires_at
            .duration_since(SystemTime::now())
            .ok()
            .is_none_or(|left| left < Duration::from_secs(60))
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    refresh_token: Option<String>,
}

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn new() -> anyhow::Result<Self> {
        let mut bytes = [0u8; 64];
        getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no OS randomness: {e}"))?;
        let verifier = URL_SAFE_NO_PAD.encode(bytes);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Ok(Self {
            verifier,
            challenge,
        })
    }
}

fn random_state() -> anyhow::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no OS randomness: {e}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub fn authorize_url(client_id: &str, challenge: &str, state: &str) -> String {
    let mut url = url::Url::parse(AUTHORIZE_URL).expect("static url");
    url.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("code_challenge_method", "S256")
        .append_pair("code_challenge", challenge)
        .append_pair("state", state)
        .append_pair("scope", &SCOPES.join(" "));
    url.into()
}

/// Runs the whole browser sign-in. Resolves when the user approves (or denies) in the browser,
/// when `cancel` fires, or after a timeout.
pub async fn sign_in(
    http: &reqwest::Client,
    client_id: &str,
    cancel: oneshot::Receiver<()>,
) -> anyhow::Result<Tokens> {
    let listener = TcpListener::bind(("127.0.0.1", REDIRECT_PORT))
        .await
        .with_context(|| format!("port {REDIRECT_PORT} is busy (is another sign-in open?)"))?;
    let pkce = Pkce::new()?;
    let state = random_state()?;
    let url = authorize_url(client_id, &pkce.challenge, &state);
    if let Err(e) = open::that_detached(&url) {
        tracing::warn!("couldn't open a browser ({e}); visit {url}");
    }

    let code = tokio::select! {
        r = wait_for_code(&listener, &state) => r?,
        _ = cancel => bail!("sign-in cancelled"),
        _ = tokio::time::sleep(LOGIN_TIMEOUT) => bail!("sign-in timed out"),
    };
    exchange(
        http,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", REDIRECT_URI),
            ("client_id", client_id),
            ("code_verifier", &pkce.verifier),
        ],
        None,
    )
    .await
}

pub async fn refresh(
    http: &reqwest::Client,
    client_id: &str,
    refresh_token: &str,
) -> anyhow::Result<Tokens> {
    exchange(
        http,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ],
        Some(refresh_token),
    )
    .await
}

async fn exchange(
    http: &reqwest::Client,
    form: &[(&str, &str)],
    old_refresh: Option<&str>,
) -> anyhow::Result<Tokens> {
    let resp = http
        .post(TOKEN_URL)
        .form(form)
        .send()
        .await
        .context("contacting Spotify")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!(
            "Spotify rejected the sign-in ({status}): {}",
            body.chars().take(200).collect::<String>()
        );
    }
    let t: TokenResponse = resp.json().await.context("reading token response")?;
    let refresh_token = t
        .refresh_token
        .or_else(|| old_refresh.map(str::to_owned))
        .context("Spotify returned no refresh token")?;
    Ok(Tokens {
        access_token: t.access_token,
        refresh_token,
        expires_at: SystemTime::now() + Duration::from_secs(t.expires_in),
    })
}

/// Accepts browser connections until one carries the OAuth redirect for our `state`.
async fn wait_for_code(listener: &TcpListener, state: &str) -> anyhow::Result<String> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let mut buf = vec![0u8; 8192];
        let n = match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf)).await {
            Ok(Ok(n)) => n,
            _ => continue,
        };
        let request = String::from_utf8_lossy(&buf[..n]);
        let Some(target) = request
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
        else {
            continue;
        };
        let Ok(url) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
            continue;
        };
        if url.path() != "/login" {
            // Browsers also ask for /favicon.ico etc.
            let _ = stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await;
            continue;
        }
        let param = |k: &str| {
            url.query_pairs()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.into_owned())
        };
        let result = match (param("state"), param("code"), param("error")) {
            (Some(s), _, _) if s != state => {
                Err("This sign-in link is stale. Start again from Khoostic.".to_string())
            }
            (_, _, Some(e)) if e == "access_denied" => {
                Err("You declined access. You can close this tab.".to_string())
            }
            (_, _, Some(e)) => Err(format!("Spotify reported an error: {e}")),
            (_, Some(code), _) => Ok(code),
            _ => Err("Missing authorization code.".to_string()),
        };
        let (title, body) = match &result {
            Ok(_) => (
                "Connected",
                "Khoostic is connected to Spotify. You can close this tab.",
            ),
            Err(e) => ("Not connected", e.as_str()),
        };
        let page = landing_page(title, body);
        let head = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            page.len()
        );
        let _ = stream.write_all(head.as_bytes()).await;
        let _ = stream.write_all(page.as_bytes()).await;
        let _ = stream.shutdown().await;
        return result.map_err(|e| anyhow::anyhow!(e));
    }
}

fn landing_page(title: &str, body: &str) -> String {
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>Khoostic · {title}</title>
<style>body{{margin:0;height:100vh;display:grid;place-items:center;background:#0e0e12;color:#e6e6ef;
font:16px system-ui,sans-serif}}h1{{color:#89dceb;font-weight:700;margin:0 0 8px}}p{{color:#7c7c8c}}</style>
</head><body><div><h1>{title}</h1><p>{body}</p></div></body></html>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_shape() {
        let p = Pkce::new().unwrap();
        assert_eq!(p.verifier.len(), 86); // 64 bytes base64url, within RFC 7636's 43..=128
        assert!(
            p.verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(p.verifier.as_bytes()));
        assert_eq!(p.challenge, expected);
        assert_ne!(Pkce::new().unwrap().verifier, p.verifier);
    }

    #[test]
    fn url_has_everything() {
        let u = url::Url::parse(&authorize_url("cid", "chal", "st")).unwrap();
        let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
        assert_eq!(q["client_id"], "cid");
        assert_eq!(q["redirect_uri"], REDIRECT_URI);
        assert_eq!(q["code_challenge_method"], "S256");
        assert!(q["scope"].contains("streaming"));
    }

    /// Simulates the browser redirect against the real listener.
    #[tokio::test]
    async fn listener_extracts_code_and_checks_state() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let browser = tokio::spawn(async move {
            let get = |path: &'static str| async move {
                let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
                    .await
                    .unwrap();
                s.write_all(format!("GET {path} HTTP/1.1\r\nhost: x\r\n\r\n").as_bytes())
                    .await
                    .unwrap();
                let mut out = String::new();
                s.read_to_string(&mut out).await.unwrap();
                out
            };
            let favicon = get("/favicon.ico").await;
            assert!(favicon.starts_with("HTTP/1.1 404"));
            get("/login?code=abc123&state=good").await
        });
        let code = wait_for_code(&listener, "good").await.unwrap();
        assert_eq!(code, "abc123");
        assert!(browser.await.unwrap().contains("Connected"));
    }
}
