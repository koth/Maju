use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;
use std::collections::BTreeMap;
use crate::error::{SdkError, SdkResult};
pub const DEFAULT_CONTROL_TIMEOUT_MS: u64 = 60_000;
pub const ENV_CLI_PATH: &str = "CODEBUDDY_CODE_PATH";
pub const ENV_DISABLE_AUTOUPDATER: &str = "DISABLE_AUTOUPDATER";
pub const ENV_DISABLE_AUTO_MEMORY: &str = "CODEBUDDY_DISABLE_AUTO_MEMORY";
pub const ENV_CUSTOM_HEADERS: &str = "CODEBUDDY_CUSTOM_HEADERS";
const BINARY_NAMES: &[&str] = &[
    "codebuddy-headless.exe",
    "codebuddy-headless",
    "codebuddy.exe",
    "codebuddy",
];
static CLI_VERSION: OnceLock<String> = OnceLock::new();
pub fn resolve_cli_path() -> SdkResult<PathBuf> {
    resolve_cli_path_with(&std::env::var(ENV_CLI_PATH).ok(), bundled_cli_candidates)
}

/// Resolve the CLI binary from an explicit env override plus a list of bundled
/// candidate paths.
///
/// Every candidate — the env override and **all** bundled candidates — is
/// recorded into the returned error's `Searched:` list, even when none of them
/// exist on disk. Previously the bundled callback returned only the first
/// existing candidate (or `None`), so when no binary was present the error
/// listed zero searched paths, making "CodeBuddy CLI binary not found …
/// Searched: []" useless for diagnosing where to place the binary.
pub fn resolve_cli_path_with(
    env_var: &Option<String>,
    bundled: impl FnOnce() -> Vec<PathBuf>,
) -> SdkResult<PathBuf> {
    let mut tried: Vec<PathBuf> = Vec::new();
    if let Some(path) = env_var {
        let p = PathBuf::from(path);
        tried.push(p.clone());
        if p.is_file() {
            return Ok(p);
        }
    }
    for p in bundled() {
        tried.push(p.clone());
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(SdkError::CliNotFound(
        tried.into_iter().map(|p| p.to_string_lossy().to_string()).collect(),
    ))
}
/// First existing bundled CLI candidate, or `None` when no candidate exists.
pub fn bundled_cli_path() -> Option<PathBuf> {
    bundled_cli_candidates().into_iter().find(|p| p.is_file())
}

/// All bundled CLI candidate paths in search order (existence not checked).
///
/// Exposed so callers can enumerate every searched location for diagnostics
/// and so [`resolve_cli_path`] can record them all into its `Searched:` list.
pub fn bundled_cli_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in search_dirs() {
        for name in BINARY_NAMES {
            out.push(dir.join(name));
        }
    }
    out
}
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.join("bin"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            dirs.push(parent.join("bin"));
            dirs.push(parent.to_path_buf());
        }
    }
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        let p = PathBuf::from(manifest);
        if let Some(parent) = p.parent() {
            dirs.push(parent.join("bin"));
        }
        dirs.push(p.join("bin"));
    }
    dirs
}
pub fn cli_version(cli_path: &Path) -> String {
    if let Some(cached) = CLI_VERSION.get() {
        return cached.clone();
    }
    let resolved = read_version_from(cli_path);
    let value = resolved.unwrap_or_else(|| "unknown".to_string());
    let _ = CLI_VERSION.set(value.clone());
    value
}
fn read_version_from(cli_path: &Path) -> Option<String> {
    let dir = cli_path.parent()?;
    let parent_dir = dir.parent();
    let metadata_path = parent_dir?.join("metadata.json");
    if let Ok(text) = std::fs::read_to_string(&metadata_path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(tag) = v.get("tag").and_then(|s| s.as_str()) {
                if let Some(ver) = extract_version_after_at(tag) {
                    return Some(ver.to_string());
                }
            }
        }
    }
    for cand in [parent_dir?.join("package.json"), dir.join("package.json")] {
        if let Ok(text) = std::fs::read_to_string(&cand) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(ver) = v
                    .get("publishConfig")
                    .and_then(|p| p.get("customPackage"))
                    .and_then(|p| p.get("version"))
                    .and_then(|s| s.as_str())
                {
                    if ver != "0.0.0" {
                        return Some(ver.to_string());
                    }
                }
                if let Some(ver) = v.get("version").and_then(|s| s.as_str()) {
                    if ver != "0.0.0" {
                        return Some(ver.to_string());
                    }
                }
            }
        }
    }
    None
}
fn extract_version_after_at(s: &str) -> Option<&str> {
    // Find the LAST '@' (e.g. "@tencent-ai/codebuddy-code@2.41.6" → after the second '@').
    let idx = s.rfind('@')?;
    let rest = &s[idx + 1..];
    let mut end = 0usize;
    let mut dot_count = 0;
    for (i, ch) in rest.char_indices() {
        if ch.is_ascii_digit() {
            end = i + ch.len_utf8();
        } else if ch == '.' && dot_count < 2 {
            end = i + ch.len_utf8();
            dot_count += 1;
        } else {
            break;
        }
    }
    if dot_count == 2 && end > 0 {
        Some(&rest[..end])
    } else {
        None
    }
}
pub fn build_child_env(
    user_env: &BTreeMap<String, String>,
    cli_path: &Path,
    rust_version: &str,
) -> Vec<(String, String)> {
    let sdk_managed = [
        ENV_DISABLE_AUTOUPDATER.to_string(),
        ENV_DISABLE_AUTO_MEMORY.to_string(),
        ENV_CUSTOM_HEADERS.to_string(),
    ];
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| !sdk_managed.iter().any(|m| m == k))
        .collect();
    env.push((ENV_DISABLE_AUTOUPDATER.to_string(), "1".to_string()));
    env.push((ENV_DISABLE_AUTO_MEMORY.to_string(), "1".to_string()));
    let user_agent = format!(
        "User-Agent: CodeBuddy Agent SDK/{rust_version} (Rust/{rust_version}) CodeBuddy Code/{}",
        cli_version(cli_path),
    );
    let mut existing = String::new();
    for (k, v) in &env {
        if k == ENV_CUSTOM_HEADERS {
            existing = v.clone();
            break;
        }
    }
    let combined = if existing.is_empty() {
        user_agent
    } else {
        format!("{user_agent}\n{existing}")
    };
    env.push((ENV_CUSTOM_HEADERS.to_string(), combined));
    for (k, v) in user_env {
        env.retain(|(ek, _)| ek != k);
        env.push((k.clone(), v.clone()));
    }
    env
}
pub fn default_control_timeout() -> Duration {
    Duration::from_millis(DEFAULT_CONTROL_TIMEOUT_MS)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn env_var_takes_precedence_when_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("codebuddy");
        std::fs::write(&f, b"#!/bin/sh\n").unwrap();
        let env = Some(f.to_string_lossy().to_string());
        let bundled = || Vec::new();
        let p = resolve_cli_path_with(&env, bundled).unwrap();
        assert_eq!(p, f);
    }
    #[test]
    fn env_var_ignored_when_file_missing_falls_through_to_bundled() {
        let dir = tempfile::tempdir().unwrap();
        let bundled_path = dir.path().join("codebuddy");
        std::fs::write(&bundled_path, b"#!/bin/sh\n").unwrap();
        let env = Some(dir.path().join("does-not-exist").to_string_lossy().to_string());
        let bundled = || vec![bundled_path.clone()];
        let p = resolve_cli_path_with(&env, bundled).unwrap();
        assert_eq!(p, bundled_path);
    }
    #[test]
    fn both_missing_returns_cli_not_found_with_searched_paths() {
        let dir = tempfile::tempdir().unwrap();
        let bogus_env = dir.path().join("missing-env");
        let bogus_bundled = dir.path().join("missing-bundled");
        let env = Some(bogus_env.to_string_lossy().to_string());
        let bundled = || vec![bogus_bundled.clone()];
        let err = resolve_cli_path_with(&env, bundled).unwrap_err();
        match err {
            SdkError::CliNotFound(searched) => {
                assert!(
                    searched.iter().any(|s| s.contains("missing-env")),
                    "env path missing from searched: {searched:?}"
                );
                assert!(
                    searched.iter().any(|s| s.contains("missing-bundled")),
                    "bundled candidate missing from searched: {searched:?}"
                );
            }
            other => panic!("expected CliNotFound, got {other:?}"),
        }
    }
    #[test]
    fn build_child_env_injects_defaults_and_user_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join("codebuddy-headless");
        std::fs::write(&cli, b"#!/bin/sh\n").unwrap();
        let mut user = std::collections::BTreeMap::new();
        user.insert("MY_API_KEY".to_string(), "abc".to_string());
        user.insert("DISABLE_AUTOUPDATER".to_string(), "0".to_string());
        let env = build_child_env(&user, &cli, "test-ver");
        let map: std::collections::BTreeMap<_, _> = env.into_iter().collect();
        assert_eq!(map.get(ENV_DISABLE_AUTOUPDATER).map(String::as_str), Some("0"));
        assert_eq!(map.get(ENV_DISABLE_AUTO_MEMORY).map(String::as_str), Some("1"));
        let ua = map.get(ENV_CUSTOM_HEADERS).unwrap();
        assert!(
            ua.starts_with("User-Agent: CodeBuddy Agent SDK/test-ver"),
            "ua={ua}"
        );
        assert_eq!(map.get("MY_API_KEY").map(String::as_str), Some("abc"));
    }
    #[test]
    fn extract_version_after_at_basic() {
        assert_eq!(
            extract_version_after_at("@tencent-ai/codebuddy-code@2.41.6"),
            Some("2.41.6")
        );
        assert_eq!(extract_version_after_at("tag@1.2.3-rc.1"), Some("1.2.3"));
        assert_eq!(extract_version_after_at("tag@1.2"), None);
    }
}
