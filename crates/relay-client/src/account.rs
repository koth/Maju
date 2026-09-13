//! Account login (passwordless email-OTP) + account-session persistence.
//!
//! Complements [`binding`]: `BoundDevice` holds the `auth_token` *after* a
//! successful bind, while `AccountSession` holds the `auth_token` *acquired
//! by login* and feeds it into the subsequent `BindDeviceRequest`. Both are
//! persisted as JSON in the app data dir, separate from the device key and
//! from the E2E session key (which is re-derived per pairing).
//!
//! The wire protocol is untouched: `BindDeviceRequest { auth_token }` keeps
//! treating `auth_token` as an opaque string — only its minting source
//! changed from a placeholder to the relay's `/auth/*` HTTP endpoints.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

/// Persisted account session acquired via the email-OTP login flow.
/// `auth_token` rotates on each login (server-side); `account_id` is
/// stable per email. None of these is the E2E session key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountSession {
    pub email: String,
    pub account_id: String,
    pub auth_token: String,
}

impl AccountSession {
    /// Persist the session as JSON at `path`. Mirrors `BoundDevice::persist`.
    pub fn persist(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create account session dir {:?}", parent))?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)
            .with_context(|| format!("write account session {:?}", path))?;
        Ok(())
    }

    /// Load a stored session, or `Ok(None)` if none exists.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let json =
            std::fs::read_to_string(path).with_context(|| format!("read account session {:?}", path))?;
        let session = serde_json::from_str(&json).context("parse account session")?;
        Ok(Some(session))
    }

    /// Delete the session (on explicit logout).
    pub fn clear(path: &Path) -> Result<()> {
        if path.exists() {
            std::fs::remove_file(path)
                .with_context(|| format!("remove account session {:?}", path))?;
        }
        Ok(())
    }
}

/// HTTP client for the relay's passwordless login endpoints
/// (`POST /auth/send-code`, `POST /auth/login`). `base_url` is the auth
/// HTTP origin (e.g. `https://relay.kodex.app` or `http://127.0.0.1:8789`);
/// the server serves `/auth/*` on a listener separate from the WebSocket.
pub struct LoginClient {
    base_url: String,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct LoginResponse {
    auth_token: String,
    account_id: String,
}

impl LoginClient {
    /// Build a client for the given auth HTTP origin. A trailing `/` is
    /// trimmed. The request timeout caps a stuck relay so the UI does not
    /// hang indefinitely on send-code/login.
    ///
    /// `insecure` skips TLS certificate verification for the HTTP requests
    /// (self-signed relay host in development). Must be gated behind an
    /// explicit opt-in by the caller.
    pub fn new(base_url: String, insecure: bool) -> Self {
        let mut base = base_url;
        while base.ends_with('/') {
            base.pop();
        }
        let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(15));
        if insecure {
            // Accept self-signed certs on the auth HTTP origin. Only enabled
            // by an explicit opt-in for development against a self-signed relay.
            builder = builder.danger_accept_invalid_certs(true);
        }
        let http = builder
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { base_url: base, http }
    }

    /// `POST /auth/send-code { email }`. Succeeds on 2xx; surfaces the
    /// server's `{ "error": "…" }` message on 4xx so the UI can show e.g.
    /// "请求过于频繁".
    pub async fn send_code(&self, email: &str) -> Result<()> {
        let url = format!("{}/auth/send-code", self.base_url);
        let response = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "email": email }))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("send-code request: {e}"))?;
        Self::ensure_ok(response).await
    }

    /// `POST /auth/login { email, code }`. On success returns the freshly
    /// issued `AccountSession` (the response carries `auth_token` +
    /// `account_id`; the email is the one the user typed).
    pub async fn login(&self, email: &str, code: &str) -> Result<AccountSession> {
        let url = format!("{}/auth/login", self.base_url);
        let response = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "email": email, "code": code }))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("login request: {e}"))?;
        if !response.status().is_success() {
            let message = Self::error_message(response).await;
            return Err(anyhow::anyhow!("{message}"));
        }
        let body: LoginResponse = response
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("parse login response: {e}"))?;
        Ok(AccountSession {
            email: email.to_string(),
            account_id: body.account_id,
            auth_token: body.auth_token,
        })
    }

    async fn ensure_ok(response: reqwest::Response) -> Result<()> {
        if response.status().is_success() {
            return Ok(());
        }
        let message = Self::error_message(response).await;
        Err(anyhow::anyhow!("{message}"))
    }

    /// Extract a human-readable error from a non-2xx response. The relay
    /// returns `{ "error": "…" }`; fall back to `relay <status>: <body>`.
    async fn error_message(response: reqwest::Response) -> String {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(error) = value.get("error").and_then(|v| v.as_str()) {
                return error.to_string();
            }
        }
        format!("relay {status}: {text}")
    }
}

/// Derive the auth HTTP origin from a WebSocket relay endpoint:
/// `wss://host[:port][/path]` → `https://host[:port]`, `ws://` → `http://`.
/// Path/query/fragment are stripped (the relay serves `/auth/*` at the
/// origin root). Returns `None` for a non-`ws`/`wss` endpoint.
pub fn auth_base_url_from_ws_endpoint(ws_endpoint: &str) -> Option<String> {
    let (scheme, rest) = if let Some(rest) = ws_endpoint.strip_prefix("wss://") {
        ("https", rest)
    } else if let Some(rest) = ws_endpoint.strip_prefix("ws://") {
        ("http", rest)
    } else {
        return None;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if authority.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{authority}"))
}

/// Split a relay WebSocket endpoint into its `(host, port)` pair. The port
/// defaults by scheme (443 for `wss://`, 80 for `ws://`); path/query/fragment
/// are stripped. Returns `None` for a non-`ws`/`wss` endpoint.
///
/// Used by the desktop to work out which local interface it would use to reach
/// the relay (see `local_egress_ip`), so it must not require a name to resolve
/// — the caller resolves it.
pub fn relay_host_port(ws_endpoint: &str) -> Option<(String, u16)> {
    let (default_port, rest) = if let Some(rest) = ws_endpoint.strip_prefix("wss://") {
        (443u16, rest)
    } else if let Some(rest) = ws_endpoint.strip_prefix("ws://") {
        (80u16, rest)
    } else {
        return None;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if authority.is_empty() {
        return None;
    }
    // Bracketed IPv6 literal: `[::1]:8443`.
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        let port = after
            .strip_prefix(':')
            .and_then(|p| p.parse().ok())
            .unwrap_or(default_port);
        return Some((host.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => match port.parse::<u16>() {
            Ok(parsed) => Some((host.to_string(), parsed)),
            // `host:not-a-port` — treat the whole authority as the host.
            Err(_) => Some((authority.to_string(), default_port)),
        },
        _ => Some((authority.to_string(), default_port)),
    }
}

/// Rank a candidate local address for DISPLAY to a user (lower is better), or
/// `None` when it must never be shown.
///
/// The point is to identify a machine to a human: a private LAN address
/// (`192.168.x.y`) is what people can compare, while VPN/fake-IP egress pools
/// are shared by every machine behind the same tunnel. Notably Clash-style
/// TUN mode reports `198.18.0.0/15` (RFC 2544 benchmarking space) as the
/// route-chosen source address — two PCs behind the same VPN would then show
/// the identical "IP" again, which is the bug this exists to prevent. Such
/// addresses are still returned (they are better than nothing) but ranked last
/// so any real interface address wins.
pub fn rank_local_address(ip: &str) -> Option<u8> {
    let parsed: std::net::IpAddr = ip.trim().parse().ok()?;
    match parsed {
        std::net::IpAddr::V4(v4) => {
            if v4.is_loopback() || v4.is_unspecified() || v4.is_link_local() {
                return None;
            }
            let o = v4.octets();
            let rank = if o[0] == 10 || (o[0] == 192 && o[1] == 168) {
                0
            } else if o[0] == 172 && (16..=31).contains(&o[1]) {
                1
            } else if o[0] == 198 && (o[1] == 18 || o[1] == 19) {
                9
            } else {
                5
            };
            Some(rank)
        }
        // IPv6 is only a last resort: the phone's row is 12px and an IPv6
        // address does not fit or read well.
        std::net::IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() {
                None
            } else {
                Some(12)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn relay_host_port_defaults_by_scheme_and_strips_path() {
        assert_eq!(
            relay_host_port("wss://relay.kodex.app"),
            Some(("relay.kodex.app".to_string(), 443))
        );
        assert_eq!(
            relay_host_port("ws://120.48.49.190"),
            Some(("120.48.49.190".to_string(), 80))
        );
        assert_eq!(
            relay_host_port("wss://relay.kodex.app:8443/ws?x=1"),
            Some(("relay.kodex.app".to_string(), 8443))
        );
        assert_eq!(
            relay_host_port("wss://[2001:db8::1]:9443"),
            Some(("2001:db8::1".to_string(), 9443))
        );
        assert_eq!(relay_host_port("https://relay.kodex.app"), None);
        assert_eq!(relay_host_port("wss://"), None);
    }

    #[test]
    fn local_address_ranking_prefers_private_lan_over_vpn_fake_ip() {
        // A real Wi-Fi address beats Clash's shared fake-IP egress.
        let lan = rank_local_address("192.168.3.127").unwrap();
        let fake = rank_local_address("198.18.0.1").unwrap();
        assert!(lan < fake, "LAN {lan} should outrank fake-IP {fake}");
        assert!(rank_local_address("10.1.2.3").unwrap() < rank_local_address("8.8.8.8").unwrap());
        assert!(rank_local_address("172.16.0.9").unwrap() < rank_local_address("8.8.8.8").unwrap());
        assert!(rank_local_address("8.8.8.8").unwrap() < fake);
    }

    #[test]
    fn local_address_ranking_rejects_unusable_addresses() {
        assert_eq!(rank_local_address("127.0.0.1"), None);
        assert_eq!(rank_local_address("0.0.0.0"), None);
        assert_eq!(rank_local_address("169.254.10.1"), None);
        assert_eq!(rank_local_address("::1"), None);
        assert_eq!(rank_local_address("not-an-ip"), None);
    }

    #[test]
    fn auth_base_url_maps_schemes_and_strips_path() {
        assert_eq!(
            auth_base_url_from_ws_endpoint("wss://relay.kodex.app").as_deref(),
            Some("https://relay.kodex.app")
        );
        assert_eq!(
            auth_base_url_from_ws_endpoint("ws://127.0.0.1:8787").as_deref(),
            Some("http://127.0.0.1:8787")
        );
        assert_eq!(
            auth_base_url_from_ws_endpoint("wss://relay.kodex.app/relay?token=x").as_deref(),
            Some("https://relay.kodex.app")
        );
        assert!(auth_base_url_from_ws_endpoint("https://relay.kodex.app").is_none());
        assert!(auth_base_url_from_ws_endpoint("relay.kodex.app").is_none());
        assert!(auth_base_url_from_ws_endpoint("wss:///").is_none());
    }

    #[test]
    fn account_session_persists_and_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("account.json");
        let session = AccountSession {
            email: "user@example.com".to_string(),
            account_id: "acc-1".to_string(),
            auth_token: "tok-1".to_string(),
        };
        session.persist(&path).unwrap();
        let loaded = AccountSession::load(&path).unwrap().unwrap();
        assert_eq!(loaded, session);
    }

    #[test]
    fn account_session_load_returns_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.json");
        assert!(AccountSession::load(&path).unwrap().is_none());
    }

    #[test]
    fn account_session_clear_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("account.json");
        AccountSession {
            email: "u@e.com".to_string(),
            account_id: "a".to_string(),
            auth_token: "t".to_string(),
        }
        .persist(&path)
        .unwrap();
        AccountSession::clear(&path).unwrap();
        assert!(!path.exists());
        // clear is idempotent when absent
        AccountSession::clear(&path).unwrap();
    }

    /// Records the last received `(method, path, body)` and returns a canned
    /// response, so we can assert the client posted the right JSON *and*
    /// observe how it handles 200/400.
    #[derive(Clone)]
    struct CannedResponder {
        status: u16,
        body: String,
        last: Arc<std::sync::Mutex<Option<(String, String, String)>>>,
    }

    impl CannedResponder {
        fn new(status: u16, body: impl Into<String>) -> Self {
            Self {
                status,
                body: body.into(),
                last: Arc::new(std::sync::Mutex::new(None)),
            }
        }

        fn respond(&self, method: &str, path: &str, body: &str) -> (u16, String) {
            *self.last.lock().unwrap() =
                Some((method.to_string(), path.to_string(), body.to_string()));
            (self.status, self.body.clone())
        }

        fn last_request(&self) -> Option<(String, String, String)> {
            self.last.lock().unwrap().clone()
        }
    }

    async fn spawn_mock_auth_http(responder: CannedResponder) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}");
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let responder = responder.clone();
                tokio::spawn(async move {
                    let _ = handle_http(&mut stream, &responder).await;
                });
            }
        });
        url
    }

    async fn handle_http(
        stream: &mut tokio::net::TcpStream,
        responder: &CannedResponder,
    ) -> std::io::Result<()> {
        let mut buf = Vec::with_capacity(4096);
        let mut tmp = [0u8; 4096];
        let header_end = loop {
            let n = stream.read(&mut tmp).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(idx) = find_subsequence(&buf, b"\r\n\r\n") {
                break idx;
            }
            if buf.len() > 65_536 {
                return Ok(());
            }
        };
        let header_len = header_end + 4;
        let head = std::str::from_utf8(&buf[..header_len]).unwrap_or("");
        let mut lines = head.lines();
        let request_line = lines.next().unwrap_or("");
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let path = parts.next().unwrap_or("").to_string();
        let content_length: usize = lines
            .find_map(|l| {
                let lower = l.to_ascii_lowercase();
                lower
                    .strip_prefix("content-length:")
                    .and_then(|v| v.trim().parse().ok())
            })
            .unwrap_or(0);
        let mut body = buf[header_len..].to_vec();
        while body.len() < content_length {
            let n = stream.read(&mut tmp).await?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&tmp[..n]);
        }
        let take = body.len().min(content_length);
        let body_str = String::from_utf8_lossy(&body[..take]).to_string();
        let (status, body_out) = responder.respond(&method, &path, &body_str);
        let reason = match status {
            200 => "OK",
            204 => "No Content",
            400 => "Bad Request",
            _ => "OK",
        };
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body_out.len(),
            body_out
        );
        stream.write_all(response.as_bytes()).await?;
        Ok(())
    }

    fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    #[tokio::test]
    async fn send_code_posts_email_and_succeeds_on_200() {
        let responder = CannedResponder::new(200, r#"{"ok":true}"#);
        let url = spawn_mock_auth_http(responder.clone()).await;
        let client = LoginClient::new(url, false);
        client.send_code("user@example.com").await.expect("200 ok");

        let (method, path, body) = responder.last_request().expect("request recorded");
        assert_eq!(method, "POST");
        assert_eq!(path, "/auth/send-code");
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["email"], "user@example.com");
    }

    #[tokio::test]
    async fn send_code_surfaces_error_message_on_400() {
        let responder = CannedResponder::new(400, r#"{"error":"请求过于频繁，请稍后再试"}"#);
        let url = spawn_mock_auth_http(responder.clone()).await;
        let client = LoginClient::new(url, false);
        let err = client
            .send_code("user@example.com")
            .await
            .expect_err("400 should error");
        assert!(err.to_string().contains("请求过于频繁"));
    }

    #[tokio::test]
    async fn login_parses_token_and_account_id() {
        let responder =
            CannedResponder::new(200, r#"{"auth_token":"tok-abc","account_id":"acc-7"}"#);
        let url = spawn_mock_auth_http(responder.clone()).await;
        let client = LoginClient::new(url, false);
        let session = client
            .login("user@example.com", "123456")
            .await
            .expect("200 ok");
        assert_eq!(session.email, "user@example.com");
        assert_eq!(session.account_id, "acc-7");
        assert_eq!(session.auth_token, "tok-abc");

        let (method, path, body) = responder.last_request().expect("request recorded");
        assert_eq!(method, "POST");
        assert_eq!(path, "/auth/login");
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["email"], "user@example.com");
        assert_eq!(parsed["code"], "123456");
    }

    #[tokio::test]
    async fn login_surfaces_error_message_on_400() {
        let responder = CannedResponder::new(400, r#"{"error":"验证码错误"}"#);
        let url = spawn_mock_auth_http(responder.clone()).await;
        let client = LoginClient::new(url, false);
        let err = client
            .login("user@example.com", "000000")
            .await
            .expect_err("400 should error");
        assert!(err.to_string().contains("验证码错误"));
    }

    #[tokio::test]
    async fn login_client_trims_trailing_slash_in_base_url() {
        // Base URL with a trailing slash must still produce /auth/login
        // (not //auth/login).
        let responder =
            CannedResponder::new(200, r#"{"auth_token":"t","account_id":"a"}"#);
        let url = spawn_mock_auth_http(responder.clone()).await;
        let client = LoginClient::new(format!("{url}/"), false);
        client.login("u@e.com", "1").await.unwrap();
        let (_, path, _) = responder.last_request().unwrap();
        assert_eq!(path, "/auth/login");
    }
}
