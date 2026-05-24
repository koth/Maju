use anyhow::{Context, Result};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;
use workspace_model::{
    ClaudeWoaChannel, ClaudeWoaSettings, ClaudeWoaSettingsStatus, ClaudeWoaTokenStatus,
};

const WOA_SERVER: &str = "https://copilot.code.woa.com";
const WOA_CLIENT_ID: &str = "d15f1aada3db4be2be622afed0019a29";
const WOA_DEVICE_CODE_URL: &str = "https://copilot.code.woa.com/api/v2/auth/device/code";
const WOA_DEVICE_TOKEN_URL: &str = "https://copilot.code.woa.com/api/v2/auth/device/token";
const WOA_REFRESH_URL: &str = "https://copilot.code.woa.com/api/v2/auth/oauth_token/refresh";
pub const WOA_TOKEN_REFRESH_THRESHOLD_MS: u64 = 5 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WoaToken {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    pub expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WoaDeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_at_ms: u64,
    pub interval_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WoaPollResult {
    Pending,
    SlowDown,
    Token(WoaToken),
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default = "default_device_expires_in")]
    expires_in: u64,
    #[serde(default = "default_device_interval")]
    interval: u64,
}

#[derive(Debug, Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: u64,
}

#[derive(Debug, Deserialize)]
struct OAuthErrorResponse {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

fn default_device_expires_in() -> u64 {
    600
}

fn default_device_interval() -> u64 {
    5
}

pub fn default_token_path() -> PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".kodex")
        .join("claude-woa-token.json")
}

pub fn effective_token_path(settings: &ClaudeWoaSettings) -> PathBuf {
    settings
        .token_path
        .clone()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(default_token_path)
}

pub fn status(settings: &ClaudeWoaSettings) -> ClaudeWoaSettingsStatus {
    let token_path = effective_token_path(settings);
    let token = token_status(&token_path, settings.channel, now_ms());
    ClaudeWoaSettingsStatus {
        channel: settings.channel,
        selected_profile_id: String::new(),
        profiles: Vec::new(),
        token_path,
        token,
    }
}

pub fn token_status(path: &Path, _channel: ClaudeWoaChannel, now: u64) -> ClaudeWoaTokenStatus {
    match load_token(path) {
        Ok(Some(token)) => {
            let remaining_ms = token.expires_at.saturating_sub(now);
            ClaudeWoaTokenStatus {
                exists: true,
                malformed: false,
                access_token: Some(mask_secret(Some(token.access_token.as_str()))),
                refresh_token: Some(mask_secret(token.refresh_token.as_deref())),
                expires_at: Some(token.expires_at.to_string()),
                valid_for_minutes: Some((remaining_ms / 60_000) as i64),
                refresh_needed: is_expiring_soon(&token, now),
                message: None,
            }
        }
        Ok(None) => ClaudeWoaTokenStatus {
            exists: false,
            malformed: false,
            access_token: None,
            refresh_token: None,
            expires_at: None,
            valid_for_minutes: None,
            refresh_needed: false,
            message: Some("Run WOA login to create a token.".into()),
        },
        Err(error) => ClaudeWoaTokenStatus {
            exists: true,
            malformed: true,
            access_token: None,
            refresh_token: None,
            expires_at: None,
            valid_for_minutes: None,
            refresh_needed: false,
            message: Some(redact(&error.to_string())),
        },
    }
}

pub fn load_token(path: &Path) -> Result<Option<WoaToken>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read WOA token {}", path.display()));
        }
    };
    let value: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("{} is not valid WOA token JSON", path.display()))?;
    validate_token(value).with_context(|| format!("malformed WOA token {}", path.display()))
}

pub fn save_token(path: &Path, token: &WoaToken) -> Result<()> {
    validate_token(serde_json::to_value(token)?)?
        .ok_or_else(|| anyhow::anyhow!("missing token"))?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)
        .with_context(|| format!("failed to create WOA token directory {}", dir.display()))?;
    let temp_path = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("claude-woa-token"),
        Uuid::new_v4()
    ));
    let content = serde_json::to_string_pretty(token)? + "\n";
    if let Err(error) = std::fs::write(&temp_path, content) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error).with_context(|| format!("failed to write {}", temp_path.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&temp_path)?.permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&temp_path, permissions)?;
    }
    std::fs::rename(&temp_path, path)
        .with_context(|| format!("failed to save WOA token {}", path.display()))
}

fn validate_token(value: serde_json::Value) -> Result<Option<WoaToken>> {
    if value.is_null() {
        return Ok(None);
    }
    let token: WoaToken = serde_json::from_value(value)?;
    if token.access_token.trim().is_empty() {
        anyhow::bail!("WOA token is missing accessToken");
    }
    if token.expires_at == 0 {
        anyhow::bail!("WOA token is missing expiresAt");
    }
    Ok(Some(token))
}

pub fn is_expiring_soon(token: &WoaToken, now: u64) -> bool {
    token.expires_at <= now.saturating_add(WOA_TOKEN_REFRESH_THRESHOLD_MS)
}

pub async fn request_device_code() -> Result<WoaDeviceCode> {
    let client = reqwest::Client::new();
    let response = client
        .post(WOA_DEVICE_CODE_URL)
        .form(&[("client_id", WOA_CLIENT_ID)])
        .send()
        .await
        .context("failed to request WOA device code")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("WOA device code request failed: {status} {}", redact(&body));
    }
    let data: DeviceCodeResponse = response
        .json()
        .await
        .context("WOA device code response was not valid JSON")?;
    let verification_uri = if data.verification_uri.starts_with("http") {
        data.verification_uri
    } else {
        format!("{WOA_SERVER}{}", data.verification_uri)
    };
    Ok(WoaDeviceCode {
        device_code: data.device_code,
        user_code: data.user_code,
        verification_uri,
        verification_uri_complete: data.verification_uri_complete,
        expires_at_ms: now_ms().saturating_add(data.expires_in.saturating_mul(1000)),
        interval_ms: data.interval.saturating_mul(1000),
    })
}

pub async fn poll_device_token(device_code: &str) -> Result<WoaPollResult> {
    let client = reqwest::Client::new();
    let response = client
        .post(WOA_DEVICE_TOKEN_URL)
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device_code),
            ("client_id", WOA_CLIENT_ID),
        ])
        .send()
        .await
        .context("failed to poll WOA token")?;
    if !response.status().is_success() {
        return poll_error(response).await;
    }
    let token: OAuthTokenResponse = response
        .json()
        .await
        .context("WOA token response was not valid JSON")?;
    Ok(WoaPollResult::Token(token_from_oauth(token, None)?))
}

async fn poll_error(response: reqwest::Response) -> Result<WoaPollResult> {
    let status = response.status();
    let body = response
        .json::<OAuthErrorResponse>()
        .await
        .unwrap_or(OAuthErrorResponse {
            error: None,
            error_description: None,
        });
    let code = body
        .error
        .or(body.error_description)
        .unwrap_or_else(|| format!("status_{}", status.as_u16()));
    match code.as_str() {
        "authorization_pending" => Ok(WoaPollResult::Pending),
        "slow_down" => Ok(WoaPollResult::SlowDown),
        _ => anyhow::bail!("WOA token exchange failed: {}", redact(&code)),
    }
}

pub async fn refresh_and_save(settings: &ClaudeWoaSettings) -> Result<WoaToken> {
    let path = effective_token_path(settings);
    let token = load_token(&path)?.ok_or_else(|| {
        anyhow::anyhow!("No WOA token found at {}. Run WOA login.", path.display())
    })?;
    let fresh = refresh_token(&token).await?;
    save_token(&path, &fresh)?;
    Ok(fresh)
}

pub async fn ensure_token_ready(settings: &ClaudeWoaSettings) -> Result<WoaToken> {
    let path = effective_token_path(settings);
    let token = load_token(&path)?.ok_or_else(|| {
        anyhow::anyhow!("No WOA token found at {}. Run WOA login.", path.display())
    })?;
    if !is_expiring_soon(&token, now_ms()) {
        return Ok(token);
    }
    let fresh = refresh_token(&token).await?;
    save_token(&path, &fresh)?;
    Ok(fresh)
}

pub async fn refresh_token(token: &WoaToken) -> Result<WoaToken> {
    let refresh_token = token
        .refresh_token
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("WOA token has no refresh token. Run WOA login."))?;
    let client = reqwest::Client::new();
    let response = client
        .post(WOA_REFRESH_URL)
        .header("OAUTH-TOKEN", &token.access_token)
        .form(&[
            ("refresh_token", refresh_token),
            ("client_id", WOA_CLIENT_ID),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await
        .context("failed to refresh WOA token")?;
    if response.status() != StatusCode::OK {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!(
            "WOA token refresh failed: {status} {}. Run WOA login.",
            redact(&body)
        );
    }
    let fresh: OAuthTokenResponse = response
        .json()
        .await
        .context("WOA refresh response was not valid JSON")?;
    token_from_oauth(fresh, token.refresh_token.clone())
}

fn token_from_oauth(
    value: OAuthTokenResponse,
    previous_refresh_token: Option<String>,
) -> Result<WoaToken> {
    if value.access_token.trim().is_empty() {
        anyhow::bail!("WOA token response missing access_token");
    }
    if value.expires_in == 0 {
        anyhow::bail!("WOA token response missing expires_in");
    }
    Ok(WoaToken {
        access_token: value.access_token,
        refresh_token: value.refresh_token.or(previous_refresh_token),
        expires_at: now_ms().saturating_add(value.expires_in.saturating_mul(1000)),
    })
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_millis() as u64
}

pub fn mask_secret(secret: Option<&str>) -> String {
    let Some(secret) = secret else {
        return "(none)".into();
    };
    if secret.len() <= 8 {
        let start = secret.chars().take(2).collect::<String>();
        let end = secret
            .chars()
            .rev()
            .take(2)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
        return format!("{start}...{end}");
    }
    let start = secret.chars().take(4).collect::<String>();
    let end = secret
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{start}...{end}")
}

pub fn redact(value: &str) -> String {
    value
        .split_whitespace()
        .map(|part| {
            if part.len() > 24
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
            {
                "[redacted]"
            } else {
                part
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn channel_arg(channel: ClaudeWoaChannel) -> &'static str {
    match channel {
        ClaudeWoaChannel::Default => "default",
        ClaudeWoaChannel::Offline => "offline",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn token_round_trip_uses_claude_woa_shape() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("token.json");
        let token = WoaToken {
            access_token: "access-secret".into(),
            refresh_token: Some("refresh-secret".into()),
            expires_at: 123,
        };

        save_token(&path, &token).unwrap();
        let loaded = load_token(&path).unwrap().unwrap();

        assert_eq!(loaded, token);
        let raw = std::fs::read_to_string(path).unwrap();
        assert!(raw.contains("accessToken"));
        assert!(raw.contains("refreshToken"));
        assert!(raw.contains("expiresAt"));
    }

    #[test]
    fn token_status_redacts_secrets() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("token.json");
        save_token(
            &path,
            &WoaToken {
                access_token: "access-secret-value".into(),
                refresh_token: Some("refresh-secret-value".into()),
                expires_at: 100_000,
            },
        )
        .unwrap();

        let status = token_status(&path, ClaudeWoaChannel::Default, 1);
        let serialized = serde_json::to_string(&status).unwrap();

        assert!(status.exists);
        assert!(!serialized.contains("access-secret-value"));
        assert!(!serialized.contains("refresh-secret-value"));
        assert!(serialized.contains("acce...alue"));
    }

    #[test]
    fn malformed_token_reports_non_secret_status() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("token.json");
        std::fs::write(&path, "{broken").unwrap();

        let status = token_status(&path, ClaudeWoaChannel::Default, 1);

        assert!(status.exists);
        assert!(status.malformed);
        assert!(status.message.unwrap().contains("not valid WOA token JSON"));
    }
}
