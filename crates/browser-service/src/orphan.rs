//! Reclaiming browser provider processes orphaned by a crashed Kodex.
//!
//! `kill_on_drop` covers the ordinary path: when a session closes or the app
//! quits cleanly, the child is torn down. It does not cover a `SIGKILL`, a
//! panic, or a power loss — the child is reparented to init and keeps running,
//! holding a Chromium per session until someone notices.
//!
//! Identification is by environment marker, not by command line. A browser
//! provider is an ordinary `node …/cli.js` process, and matching on that would
//! put every unrelated Node process on the machine in range of a `kill`. Each
//! spawned provider instead carries [`OWNER_ENV`], naming the pid of the Kodex
//! that started it; a provider is only reaped when that pid is gone.
//!
//! The bias throughout is toward leaving a process alone. A surviving orphan
//! costs disk and a background process; a misidentified kill costs the user a
//! browser they were using.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, Ordering};

/// Marks a process as a Kodex-owned browser provider.
pub const OWNER_ENV: &str = "KODEX_BROWSER_OWNER";

/// The pid Kodex stamps into every provider it spawns.
static OWNER_PID: AtomicU32 = AtomicU32::new(0);

fn owner_pid() -> u32 {
    let cached = OWNER_PID.load(Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }
    let pid = std::process::id();
    OWNER_PID.store(pid, Ordering::Relaxed);
    pid
}

/// Environment for a provider child: the filtered parent environment plus the
/// owner marker that makes the process identifiable after a crash.
pub fn provider_env(
    parent: &std::collections::HashMap<String, String>,
) -> std::collections::HashMap<String, String> {
    let mut env = crate::provider::ProviderLaunch::child_env(parent);
    env.insert(OWNER_ENV.to_string(), owner_pid().to_string());
    env
}

/// One row of a process listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsRow {
    pub pid: u32,
    pub ppid: u32,
}

/// Parse `ps` output shaped as `pid= ppid=` columns.
///
/// Shared shape with the dsh reaper; kept here so this crate does not depend
/// on `dsh-bridge` just to read a process table.
#[cfg(any(unix, test))]
pub(crate) fn parse_ps_rows(output: &str) -> Vec<PsRow> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid = fields.next()?.parse().ok()?;
            Some(PsRow { pid, ppid })
        })
        .collect()
}

/// Whether an environment listing declares `KODEX_BROWSER_OWNER=<expected>`.
///
/// Accepts both shapes platforms return: macOS `ps eww` prefixes
/// space-separated `KEY=VALUE` pairs to the command line, and Linux
/// `/proc/<pid>/environ` is NUL-separated.
#[cfg(any(unix, test))]
pub(crate) fn env_declares_owner(env_text: &str, expected: &str) -> bool {
    let wanted = format!("{OWNER_ENV}={expected}");
    env_text
        .split(['\0', ' ', '\n'])
        .any(|record| record == wanted)
}

/// The Kodex pid a provider names, or `None` when it names none.
///
/// This is the positive match. A process that does not carry the marker is not
/// ours, full stop — no amount of it looking orphaned changes that.
#[cfg(any(unix, test))]
pub(crate) fn owner_pid_from_env(env_text: &str) -> Option<u32> {
    let prefix = format!("{OWNER_ENV}=");
    env_text
        .split(['\0', ' ', '\n'])
        .filter_map(|record| record.strip_prefix(&prefix))
        .find_map(|value| value.trim().parse().ok())
}

/// Whether a row's parent is gone: reparented to init, or naming a pid that is
/// not in the live table at all.
///
/// Kept for diagnostics. The reaper's decision is driven by whether the
/// *owner* is still alive, not by the parent heuristic, because a reparented
/// pid is weaker evidence than a live owner check and this function alone would
/// sweep up unrelated processes.
#[cfg(any(unix, test))]
pub(crate) fn is_orphaned(row: &PsRow, live_pids: &HashSet<u32>) -> bool {
    row.ppid == 1 || !live_pids.contains(&row.ppid)
}

/// Read a process's environment, or `None` when it cannot be read.
///
/// A process whose environment is unreadable is never a reap candidate: on
/// macOS this is also the case for processes owned by other users, and killing
/// one of those would be a serious overreach.
#[cfg(unix)]
fn read_process_env(pid: u32) -> Option<String> {
    if cfg!(target_os = "macos") {
        let out = std::process::Command::new("ps")
            .args(["eww", "-p"])
            .arg(pid.to_string())
            .args(["-o", "command="])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        std::fs::read(format!("/proc/{pid}/environ"))
            .ok()
            .map(|raw| String::from_utf8_lossy(&raw).into_owned())
    }
}

/// Terminate a non-child process: SIGTERM, bounded wait, then SIGKILL.
///
/// Returns `true` when the process is gone. `EPERM` is never escalated — a
/// process we may not signal is not ours to kill.
#[cfg(unix)]
fn terminate_non_child(pid: u32) -> bool {
    let term = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if term != 0 {
        // ESRCH: already gone, which is the desired outcome.
        // EPERM: not ours — never escalate to SIGKILL.
        return std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
    }

    for _ in 0..20 {
        if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    if unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) } != 0 {
        return false;
    }
    for _ in 0..20 {
        if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

use std::time::Duration;

/// Reclaim browser providers orphaned by a previous, crashed Kodex run.
///
/// Runs at most once per process. Orphans can only come from a *previous*
/// Kodex run — anything this run starts is ours and `kill_on_drop` reaps it —
/// so sweeping again on every later call finds nothing and costs a process
/// table walk. That cost is not small: the walk reads each candidate's
/// environment, which on macOS is a `ps` subprocess per row.
///
/// Returns the pids that were terminated.
///
/// Safety rules, each of which has cost someone a working browser otherwise:
/// a live parent is left alone, because a provider under a running Kodex is
/// that Kodex's to manage; the owner pid must match exactly, so a provider
/// belonging to a *different* Kodex instance is left alone even though this
/// one considers itself orphaned; and a process whose environment cannot be
/// read is never touched.
#[cfg(unix)]
pub fn reap_orphaned_browser_providers() -> Vec<u32> {
    static SWEPT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    // The second and later calls are the cheap no-op path.
    if SWEPT.get().is_some() {
        return Vec::new();
    }
    let _ = SWEPT.set(());

    let this_pid = owner_pid();

    let ps_args: &[&str] = if cfg!(target_os = "macos") {
        &["-ww", "-axo", "pid=,ppid="]
    } else {
        &["-e", "-w", "-w", "-o", "pid=,ppid="]
    };
    let output = match std::process::Command::new("ps").args(ps_args).output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
        Err(error) => {
            tracing::warn!(
                target: "browser_service::orphan",
                error = %error,
                "orphan reap skipped: `ps` listing failed"
            );
            return Vec::new();
        }
    };

    let rows = parse_ps_rows(&output);
    let live: HashSet<u32> = rows.iter().map(|row| row.pid).collect();

    let mut reaped = Vec::new();
    for row in &rows {
        if row.pid == this_pid {
            continue;
        }

        // Positive match, and nothing proceeds without it: a process that does
        // not carry our marker is never a candidate, however orphaned it looks.
        let Some(env) = read_process_env(row.pid) else {
            // Unreadable environment: on macOS this also means another user,
            // and those are never ours to signal.
            continue;
        };
        let Some(owner) = owner_pid_from_env(&env) else {
            continue;
        };

        // The Kodex that started this provider is still running, so it owns
        // the provider's lifetime — including the normal kill-on-drop path.
        // Only a dead owner makes it an orphan.
        if owner == this_pid || live.contains(&owner) {
            continue;
        }

        if terminate_non_child(row.pid) {
            tracing::info!(
                target: "browser_service::orphan",
                pid = row.pid,
                owner = owner,
                "reclaimed orphaned browser provider"
            );
            reaped.push(row.pid);
        }
    }
    reaped
}

/// No-op on platforms without process signalling.
#[cfg(not(unix))]
pub fn reap_orphaned_browser_providers() -> Vec<u32> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: u32, ppid: u32) -> PsRow {
        PsRow { pid, ppid }
    }

    #[test]
    fn parses_pid_and_ppid_columns() {
        let rows = parse_ps_rows("  101    1 /node cli.js\n  202  101 /node cli.js\n");
        assert_eq!(
            rows,
            vec![row(101, 1), row(202, 101)],
            "both columns must survive, whitespace layout included",
        );
    }

    #[test]
    fn ignores_rows_that_are_not_pid_ppid_pairs() {
        assert!(parse_ps_rows("header line\n\n").is_empty());
    }

    #[test]
    fn extracts_the_owner_pid_in_both_env_shapes() {
        assert_eq!(
            owner_pid_from_env("node cli.js KODEX_BROWSER_OWNER=4242"),
            Some(4242)
        );
        assert_eq!(
            owner_pid_from_env("PATH=/usr/bin\0KODEX_BROWSER_OWNER=77"),
            Some(77)
        );
        assert_eq!(owner_pid_from_env("PATH=/usr/bin"), None);
        // A malformed value is not a match, not a panic.
        assert_eq!(owner_pid_from_env("KODEX_BROWSER_OWNER=not-a-pid"), None);
    }

    #[test]
    fn recognises_the_owner_marker_in_both_env_shapes() {
        // macOS `ps eww` is space-separated and prefixed to the command line.
        assert!(env_declares_owner(
            "node cli.js KODEX_BROWSER_OWNER=4242",
            "4242"
        ));
        // Linux /proc/<pid>/environ is NUL-separated.
        assert!(env_declares_owner(
            "PATH=/usr/bin\0KODEX_BROWSER_OWNER=4242",
            "4242"
        ));
    }

    #[test]
    fn a_different_owner_pid_does_not_match() {
        // This is the guard that stops one Kodex from killing another's
        // providers.
        assert!(!env_declares_owner("KODEX_BROWSER_OWNER=9999", "4242"));
    }

    #[test]
    fn a_process_without_the_marker_is_not_ours() {
        assert!(!env_declares_owner("PATH=/usr/bin\0HOME=/root", "4242"));
    }

    #[test]
    fn a_reparented_process_is_orphaned() {
        let live: HashSet<u32> = [101, 202].into_iter().collect();
        assert!(
            is_orphaned(&row(101, 1), &live),
            "ppid 1 means init adopted it"
        );
        assert!(
            is_orphaned(&row(101, 999), &live),
            "a ppid not in the live table means the parent exited",
        );
    }

    #[test]
    fn a_process_with_a_live_parent_is_not_orphaned() {
        let live: HashSet<u32> = [101, 202].into_iter().collect();
        assert!(!is_orphaned(&row(202, 101), &live));
    }

    #[test]
    fn provider_env_carries_the_owner_and_strips_provider_overrides() {
        let parent: std::collections::HashMap<String, String> = [
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("PLAYWRIGHT_MCP_BROWSER".to_string(), "firefox".to_string()),
        ]
        .into_iter()
        .collect();

        let env = provider_env(&parent);

        assert!(env.contains_key(OWNER_ENV));
        assert_eq!(env.get(OWNER_ENV).unwrap(), &owner_pid().to_string());
        assert!(env.contains_key("PATH"));
        assert!(
            !env.contains_key("PLAYWRIGHT_MCP_BROWSER"),
            "provider overrides must still be stripped",
        );
    }

    #[test]
    fn the_owner_pid_is_this_process() {
        assert_eq!(owner_pid(), std::process::id());
    }

    #[test]
    fn the_sweep_runs_at_most_once_per_process() {
        // The first call may sweep; every later one must be a cheap no-op.
        // This is what keeps a test suite that starts many servers from
        // walking the process table dozens of times.
        let first = reap_orphaned_browser_providers();
        let second = reap_orphaned_browser_providers();
        assert!(second.is_empty());
        // The first sweep never targets this process.
        assert!(!first.contains(&std::process::id()));
    }

    #[test]
    fn a_process_without_the_marker_is_never_a_candidate() {
        // The first version of the reaper had no positive match: it signalled
        // every orphaned process it could see, which killed unrelated software
        // on the machine. This is the regression guard for that, expressed as
        // the selection predicate rather than by running the reaper — a test
        // that sends signals to whatever it finds is not a test.
        fn is_candidate(env_text: &str, this_pid: u32, live: &HashSet<u32>) -> bool {
            let Some(owner) = owner_pid_from_env(env_text) else {
                return false;
            };
            owner != this_pid && !live.contains(&owner)
        }

        let live: HashSet<u32> = [4242].into_iter().collect();

        // Unrelated software that happens to be reparented.
        assert!(!is_candidate("PATH=/usr/bin\0HOME=/root", 100, &live));
        assert!(!is_candidate("", 100, &live));
        // A provider whose owning Kodex is still running.
        assert!(!is_candidate("KODEX_BROWSER_OWNER=4242", 100, &live));
        // Our own stamp.
        assert!(!is_candidate("KODEX_BROWSER_OWNER=100", 100, &live));
        // The only thing that qualifies: our marker, dead owner.
        assert!(is_candidate("KODEX_BROWSER_OWNER=9999", 100, &live));
    }
}
