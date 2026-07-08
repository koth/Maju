//! File-based debug logging for the CodeBuddy proxy.
//!
//! Mirrors the `codex-api-proxy` logger: lines are appended to
//! `~/.kodex/logs/codebuddy-proxy.log` (resolved via `USERPROFILE` on Windows
//! or `HOME` on Unix). In test builds the logger is a no-op so unit/e2e tests
//! do not touch the real log file.

use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

/// Append a single debug line to the codebuddy-proxy log file.
///
/// Each line is prefixed with a wall-clock time-of-day (UTC) so that the
/// request lifecycle can be correlated against the sibling
/// `codex-api-proxy.log`. Failures (missing home dir, unwritable file) are
/// silently dropped to keep the proxy hot path crash-free.
#[cfg(not(test))]
pub fn append_codebuddy_proxy_log(line: &str) {
    let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) else {
        return;
    };
    let path = std::path::PathBuf::from(home)
        .join(".kodex")
        .join("logs")
        .join("codebuddy-proxy.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(file, "[{}] {line}", now_ts());
    }
}

/// Test build no-op: never touches the filesystem.
#[cfg(test)]
pub fn append_codebuddy_proxy_log(_line: &str) {}

/// Compact `HH:MM:SS.mmm` timestamp derived from the Unix epoch. Time-of-day
/// only (no calendar date) keeps the prefix tiny while still ordering events;
/// good enough for a debug log and dependency-free.
fn now_ts() -> String {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    let day = secs % 86_400;
    let h = day / 3_600;
    let m = (day % 3_600) / 60;
    let s = day % 60;
    let ms = dur.subsec_millis();
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_is_well_formed() {
        let ts = now_ts();
        let parts: Vec<&str> = ts.split(':').collect();
        assert_eq!(parts.len(), 3, "ts={ts}");
        assert!(parts[2].contains('.'), "ts={ts}");
    }

    #[test]
    fn log_is_noop_in_tests() {
        // Should not panic and should not write any file.
        append_codebuddy_proxy_log("test-line");
    }
}
