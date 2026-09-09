//! Optional Kodex-managed `dsh web` process spawn.
//!
//! When no external `harness_endpoint` is configured, the bridge spawns
//! `dsh web` (the `dsh` CLI installed via `npm i -g @deepseek-ai/dsh`), lets
//! the OS pick a free loopback port (`--port 0`), discovers the bound port
//! from the readiness line dsh prints on stdout, and shares it across
//! sessions via the `HarnessHostRegistry`. A Kodex-spawned process is killed
//! when the last sharing session exits (SIGTERM grace then SIGKILL on Unix);
//! an externally-managed endpoint is never killed by Kodex. Two backstops
//! cover the paths where teardown never runs: a tiny exit-watchdog process
//! polls the Kodex pid and kills the server within seconds of any abrupt
//! Kodex death (force-quit, SIGKILL, panic), and [`reap_orphaned_dsh_web`]
//! reclaims at the next bring-up whatever outlived even the watchdog
//! (watchdog killed together with Kodex, or an uninterruptible server).
//!
//! The dsh web profile prints `dsh web: http://127.0.0.1:<port>` to stdout
//! the moment the server is listening (`packages/bundle/web-app/src/index.ts`).
//! That line is the readiness signal: we parse it to recover the endpoint.

use anyhow::{Context, anyhow};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// Maximum time to wait for the `dsh web` readiness line before giving up.
const READINESS_TIMEOUT: Duration = Duration::from_secs(60);
/// Windows `CREATE_NO_WINDOW` — spawn without a visible console window.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// A Kodex-spawned `dsh web` child process handle, owned by a `HarnessHost`.
pub struct DshChild {
    inner: Mutex<Option<Child>>,
    /// Windows job object that kills the whole process tree (dsh web + the
    /// node process behind the `dsh` shim) when the job handle closes. Kept
    /// alive for the child's lifetime so closing Kodex reaps everything.
    #[cfg(windows)]
    kill_on_drop_job: Option<WindowsKillOnDropJob>,
}

impl std::fmt::Debug for DshChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DshChild").finish_non_exhaustive()
    }
}

impl DshChild {
    pub fn new(child: Child) -> Self {
        Self {
            inner: Mutex::new(Some(child)),
            #[cfg(windows)]
            kill_on_drop_job: None,
        }
    }

    /// Attach the child to a Windows kill-on-close job object so the entire
    /// process tree is terminated when the job handle is closed (or killed
    /// explicitly). Best-effort: if job creation fails the child is still
    /// killed directly by [`kill_child`].
    #[cfg(windows)]
    pub fn enable_kill_on_drop_job(&mut self) {
        if let Ok(guard) = self.inner.lock()
            && let Some(child) = guard.as_ref()
        {
            match WindowsKillOnDropJob::for_child(child) {
                Ok(job) => self.kill_on_drop_job = Some(job),
                // A failure here (common when Kodex is already inside a job that
                // disallows nesting) silently removes the only crash-path safety
                // net: a shim's orphaned `node` then survives app exit. Log it so
                // the port-kill fallback in `HarnessHost::teardown` is the known
                // last line of defense rather than an invisible gap.
                Err(err) => tracing::warn!(
                    target: "dsh-bridge::spawn",
                    error = %err,
                    "kill-on-close job not attached; dsh tree relies on port-kill fallback",
                ),
            }
        }
    }

    /// No-op off Windows: job objects are a Windows-only mechanism; teardown
    /// on Unix kills the child directly via [`kill_child`].
    #[cfg(not(windows))]
    pub fn enable_kill_on_drop_job(&mut self) {}
}

/// Kill a spawned child: first close stdin (grace), then kill. Idempotent.
pub fn kill_child(child: DshChild) -> std::io::Result<()> {
    kill_child_reaped(child).map(|_| ())
}

/// Kill a spawned child and report whether the direct child was reaped;
/// callers may use `false` to decide whether to run the slower port-owner
/// fallback.
pub fn kill_child_reaped(child: DshChild) -> std::io::Result<bool> {
    #[cfg(windows)]
    let kill_on_drop_job = child.kill_on_drop_job;
    let mut reaped = false;
    if let Ok(mut guard) = child.inner.lock() {
        if let Some(mut proc) = guard.take() {
            // Graceful first, force second. The child's stdin is `Stdio::null`
            // (nothing reads it) and the `dsh web` command does not bind the
            // CLI's stdin-EOF shutdown (only the ACP stdio command does), so
            // "close stdin" cannot be the grace leg. On Unix send SIGTERM,
            // give the node process a moment to settle, then SIGKILL the
            // survivor. A hard kill is the last resort, not the plan.
            #[cfg(unix)]
            if let Some(pid) = proc.id() {
                let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
                for _ in 0..40 {
                    match proc.try_wait() {
                        Ok(Some(_)) => {
                            reaped = true;
                            break;
                        }
                        Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                        Err(_) => break,
                    }
                }
                if !reaped {
                    tracing::warn!(
                        target: "dsh-bridge::spawn",
                        pid,
                        "dsh web ignored SIGTERM for 1s; escalating to SIGKILL"
                    );
                }
            }
            // `start_kill()` (SIGKILL on Unix, TerminateProcess on Windows) is
            // the immediate kill on Windows and the escalation on Unix. Poll
            // `try_wait` briefly instead of the blocking `wait()`: the latter
            // requires a tokio runtime context and can hang when called from
            // a plain thread. The kill-on-drop job (when enabled) reaps the
            // whole tree on `DshChild` drop, so an unsettled direct child is
            // not a leak.
            let _ = proc.start_kill();
            for _ in 0..40 {
                match proc.try_wait() {
                    Ok(Some(_)) => {
                        reaped = true;
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                    Err(_) => break,
                }
            }
        }
    }
    // Close the job handle immediately after termination. With
    // KILL_ON_JOB_CLOSE this also terminates any surviving descendants of a
    // shim (e.g. the node process behind `volta run dsh`).
    #[cfg(windows)]
    drop(kill_on_drop_job);
    Ok(reaped)
}

/// Reap whatever process still holds the harness loopback port. This is the
/// last-resort fallback used after [`kill_child`] when the kill-on-close job
/// could not be attached and the `dsh` shim (`cmd.exe`/volta) has already
/// exited, leaving the real `node` server orphaned with no parent handle to
/// kill. No-op when the direct kill already freed the port.
///
/// Implemented via `netstat` + `taskkill` (no unsafe FFI). The exact
/// `127.0.0.1:<port>` local-address token is matched to avoid terminating an
/// unrelated process that happens to share a port prefix.
#[cfg(windows)]
pub(crate) fn kill_port_owner(port: u16) {
    let out = match std::process::Command::new("netstat")
        .args(["-ano", "-p", "tcp"])
        .output()
    {
        Ok(out) => out,
        Err(err) => {
            tracing::debug!(
                target: "dsh-bridge::spawn",
                error = %err,
                "netstat failed for port-kill"
            );
            return;
        }
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let local = format!("127.0.0.1:{port}");
    // netstat lists both LISTENING and ESTABLISHED rows for the same port/PID;
    // kill each owning PID at most once.
    let mut killed = std::collections::HashSet::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let proto = it.next();
        let Some(laddr) = it.next() else {
            continue;
        };
        if proto != Some("TCP") || laddr != local {
            continue;
        }
        // `netstat -ano` lists the owning PID as the last whitespace token.
        let Some(pid_token) = it.last() else { continue };
        let Ok(pid) = pid_token.parse::<u32>() else {
            continue;
        };
        if pid == 0 || killed.contains(&pid) {
            continue;
        }
        match std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID"])
            .arg(pid.to_string())
            .output()
        {
            Ok(o) if o.status.success() => {
                killed.insert(pid);
                tracing::info!(
                    target: "dsh-bridge::spawn",
                    port,
                    pid,
                    "killed orphan owning harness port"
                );
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                tracing::debug!(
                    target: "dsh-bridge::spawn",
                    port,
                    pid,
                    stderr = stderr.trim(),
                    "taskkill non-success for port owner"
                );
            }
            Err(err) => {
                tracing::debug!(
                    target: "dsh-bridge::spawn",
                    port,
                    pid,
                    error = %err,
                    "taskkill failed for port owner"
                );
            }
        }
    }
}

#[cfg(not(windows))]
pub(crate) fn kill_port_owner(_port: u16) {}

/// Self-terminating watchdog: a tiny `sh` child that polls the Kodex process
/// every few seconds and SIGTERM→SIGKILLs the `dsh web` pid the moment Kodex
/// disappears. Normal quits already kill `dsh web` via [`kill_child_reaped`];
/// this is the belt for every path that skips teardown — force-quit, SIGKILL,
/// panic — so the server can never outlive Kodex by more than one poll
/// interval. The watchdog also exits on its own when the `dsh web` pid dies
/// (nothing left to guard), so a clean quit leaves no stray watchdog behind.
///
/// The caller drops the returned handle on purpose: the watchdog is
/// intentionally untracked (teardown must not wait for it — it self-exits
/// within one poll interval), and tokio's orphan queue reaps it when it
/// exits. Returns the handle (tests hold it to observe the exit); a spawn
/// failure degrades to the next-launch orphan reap and is only a warning.
#[cfg(unix)]
fn spawn_exit_watchdog(parent_pid: u32, dsh_pid: u32) -> Option<Child> {
    // $1 = parent (Kodex) pid, $2 = dsh web pid.
    // - Parent alive + dsh alive → keep watching.
    // - dsh dead → exit (clean quit already handled the kill).
    // - Parent dead → SIGTERM, bounded grace, SIGKILL, exit.
    // The liveness re-check before SIGTERM keeps a `dsh web` that died
    // naturally in the same poll window from being signalled needlessly.
    const SCRIPT: &str = r#"while kill -0 "$1" 2>/dev/null; do
  kill -0 "$2" 2>/dev/null || exit 0
  sleep 3
done
kill -0 "$2" 2>/dev/null || exit 0
kill -TERM "$2" 2>/dev/null
i=0
while [ "$i" -lt 8 ]; do
  kill -0 "$2" 2>/dev/null || exit 0
  sleep 0.5
  i=$((i + 1))
done
kill -KILL "$2" 2>/dev/null
exit 0
"#;
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(SCRIPT)
        .arg("kodex-dsh-watchdog")
        .arg(parent_pid.to_string())
        .arg(dsh_pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match cmd.spawn() {
        Ok(child) => {
            tracing::info!(
                target: "dsh-bridge::spawn",
                watchdog_pid = child.id(),
                parent_pid,
                dsh_pid,
                "spawned dsh web exit watchdog"
            );
            Some(child)
        }
        Err(err) => {
            tracing::warn!(
                target: "dsh-bridge::spawn",
                error = %err,
                "failed to spawn dsh web exit watchdog; abrupt Kodex death relies on next-launch reap"
            );
            None
        }
    }
}

// ---- Orphaned `dsh web` reap (Unix) ----
//
// Kodex kills its spawned `dsh web` child on the normal exit paths (window
// close / ExitRequested → `shutdown_all` → teardown). A crash, `SIGKILL`, or
// force-quit runs no destructors, so the child outlives the app: its parent
// dies and the process is reparented to init/launchd. Windows covers that
// path with the kill-on-close job object (the job handle lives in the owning
// process, so a dead Kodex closes it and the tree dies with it). Unix has no
// equivalent, so the next Kodex run reclaims the leftovers: every `dsh web`
// process whose parent no longer exists AND whose `DSH_HOME` is Kodex's dsh
// home is terminated. Both checks are required — a live parent means some
// running Kodex still owns the host (another app instance), and a different
// `DSH_HOME` means the user launched that server themselves.

/// One parsed `ps` row (`pid ppid command`).
#[cfg(any(unix, test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PsRow {
    pub pid: u32,
    pub ppid: u32,
    pub command: String,
}

/// Parse `ps -o pid=,ppid=,command=` output. Leading whitespace and multiple
/// spaces between fields are tolerated; malformed rows are skipped.
#[cfg(any(unix, test))]
pub(crate) fn parse_ps_rows(output: &str) -> Vec<PsRow> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse::<u32>().ok()?;
            let ppid = fields.next()?.parse::<u32>().ok()?;
            let command = fields.collect::<Vec<_>>().join(" ");
            if command.is_empty() {
                return None;
            }
            Some(PsRow { pid, ppid, command })
        })
        .collect()
}

/// Whether a `ps` command line looks like a `dsh web` server: some token
/// whose basename is `dsh` (bare name, npm shim path, or absolute path all
/// end in `dsh`) followed by the `web` subcommand. Matching the adjacent
/// subcommand keeps `dsh acp`, `dsh --version`, or an editor running on a
/// file named `dsh` from matching.
#[cfg(any(unix, test))]
pub(crate) fn is_dsh_web_command(command: &str) -> bool {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    tokens
        .windows(2)
        .any(|pair| is_dsh_token(pair[0]) && pair[1] == "web")
}

#[cfg(any(unix, test))]
fn is_dsh_token(token: &str) -> bool {
    match token.rsplit('/').next() {
        Some(basename) => basename == "dsh",
        None => false,
    }
}

/// Orphaned = the parent is gone: reparented to init/launchd (`ppid == 1`) or
/// pointing at a pid that is not in the live table at all (parent exited but
/// the platform still reports the dead pid).
#[cfg(any(unix, test))]
fn is_orphaned(row: &PsRow, live_pids: &std::collections::HashSet<u32>) -> bool {
    row.ppid == 1 || !live_pids.contains(&row.ppid)
}

/// Whether an environment listing declares `DSH_HOME=<expected>`. Accepts
/// both shapes the platforms return: macOS `ps eww -p <pid> -o command=`
/// output (space-separated `KEY=VALUE` pairs prefixed to the command line)
/// and Linux `/proc/<pid>/environ` (NUL-separated `KEY=VALUE` records).
#[cfg(any(unix, test))]
pub(crate) fn env_declares_dsh_home(env_text: &str, expected: &str) -> bool {
    let wanted = format!("DSH_HOME={expected}");
    env_text
        .split(['\0', ' ', '\n'])
        .any(|record| record == wanted)
}

/// Reclaim `dsh web` processes orphaned by previous (crashed) Kodex runs.
/// Returns the pids that were terminated. Safety rules: a live parent means
/// the host belongs to a running Kodex and is left alone, and `DSH_HOME` must
/// match Kodex's dsh home so user-launched servers on other homes survive.
/// Processes whose environment cannot be read are left alone too — a
/// misidentified kill is worse than a surviving orphan.
#[cfg(unix)]
pub fn reap_orphaned_dsh_web(dsh_home: &str) -> Vec<u32> {
    // `-w -w` (both BSD ps and procps) lifts the command column truncation
    // for piped output; the `dsh web` match needs the full argument vector.
    let ps_args: &[&str] = if cfg!(target_os = "macos") {
        &["-ww", "-axo", "pid=,ppid=,command="]
    } else {
        &["-e", "-w", "-w", "-o", "pid=,ppid=,command="]
    };
    let output = match std::process::Command::new("ps").args(ps_args).output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
        Err(err) => {
            tracing::warn!(
                target: "dsh-bridge::spawn",
                error = %err,
                "orphan reap skipped: `ps` listing failed"
            );
            return Vec::new();
        }
    };
    let rows = parse_ps_rows(&output);
    let live_pids: std::collections::HashSet<u32> = rows.iter().map(|row| row.pid).collect();
    let candidates: Vec<u32> = rows
        .iter()
        .filter(|row| is_dsh_web_command(&row.command) && is_orphaned(row, &live_pids))
        .map(|row| row.pid)
        .collect();

    let mut reaped = Vec::new();
    for pid in candidates {
        if !process_env_declares_dsh_home(pid, dsh_home) {
            continue;
        }
        if terminate_non_child(pid) {
            reaped.push(pid);
            tracing::info!(
                target: "dsh-bridge::spawn",
                pid,
                "reaped orphaned dsh web process from a previous run"
            );
        } else {
            tracing::warn!(
                target: "dsh-bridge::spawn",
                pid,
                "orphaned dsh web process survived SIGTERM+SIGKILL"
            );
        }
    }
    reaped
}

/// Windows: the kill-on-close job object already reaps the spawned tree when
/// the owning Kodex process dies, so there is nothing to scan for.
#[cfg(not(unix))]
pub fn reap_orphaned_dsh_web(_dsh_home: &str) -> Vec<u32> {
    Vec::new()
}

/// Read a non-child process's environment and check `DSH_HOME`. Best-effort:
/// `false` on any failure so the caller leaves the process alone.
#[cfg(unix)]
fn process_env_declares_dsh_home(pid: u32, expected: &str) -> bool {
    if cfg!(target_os = "macos") {
        let out = std::process::Command::new("ps")
            .args(["eww", "-p"])
            .arg(pid.to_string())
            .arg("-o")
            .arg("command=")
            .output();
        match out {
            Ok(o) if o.status.success() => {
                env_declares_dsh_home(&String::from_utf8_lossy(&o.stdout), expected)
            }
            _ => false,
        }
    } else {
        match std::fs::read(format!("/proc/{pid}/environ")) {
            Ok(raw) => env_declares_dsh_home(&String::from_utf8_lossy(&raw), expected),
            Err(_) => false,
        }
    }
}

/// Terminate a non-child process: SIGTERM, bounded wait, then SIGKILL.
/// Returns `true` when the process is gone.
#[cfg(unix)]
fn terminate_non_child(pid: u32) -> bool {
    let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if rc != 0 {
        // ESRCH: already gone (nothing to do, but not a failure);
        // EPERM: not ours — never escalate.
        return rc == -1 && last_errno_is(libc::ESRCH);
    }
    for _ in 0..80 {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    for _ in 0..80 {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

/// `kill(pid, 0)` liveness probe: 0 = alive; EPERM = alive (another user's);
/// ESRCH = gone.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    rc == 0 || (rc == -1 && last_errno_is(libc::EPERM))
}

/// The errno of the immediately preceding libc call (macOS and glibc store it
/// in different TLS slots; `std::io::Error::last_os_error` abstracts that).
#[cfg(unix)]
fn last_errno_is(expected: i32) -> bool {
    std::io::Error::last_os_error().raw_os_error() == Some(expected)
}

/// Configuration for spawning a Kodex-managed `dsh web` process.
pub struct SpawnDshWebConfig {
    /// `DSH_HOME` — the harness home (`~/.kodex/dsh`). Required so dsh reads
    /// the Kodex-generated `settings.yaml` and keeps all state under Kodex's
    /// data root rather than `~/.dsh`.
    pub dsh_home: String,
    /// Provider API keys to inject, keyed by the env-var name written to
    /// `settings.yaml` as `apiKeyEnv` (e.g. `KODEX_DSH_DEEPSEEK_KEY` -> secret).
    /// dsh's `llm-pi-ai` resolves the credential named by `apiKeyEnv` from the
    /// launch environment, so the secret never lives in YAML.
    pub provider_keys: Vec<(String, String)>,
    /// Extra environment variables forwarded verbatim (e.g.
    /// `DSH_TELEMETRY_DISABLED=1`).
    pub extra_env: Vec<(String, String)>,
}

impl Default for SpawnDshWebConfig {
    fn default() -> Self {
        Self {
            dsh_home: String::new(),
            provider_keys: Vec::new(),
            extra_env: Vec::new(),
        }
    }
}

/// Spawn `dsh web --port 0` and return the discovered loopback endpoint plus
/// the child handle. The endpoint is recovered from the `dsh web: http://...`
/// readiness line printed on stdout once the server is listening.
///
/// `dsh` is resolved via `app_core`'s PATH search (which includes GUI-launched
/// process PATH gaps); if it is not installed, returns a diagnostic error so
/// the caller can prompt the user to `npm i -g @deepseek-ai/dsh`.
pub async fn spawn_dsh_web(config: SpawnDshWebConfig) -> anyhow::Result<(String, DshChild)> {
    let dsh = find_dsh_binary().ok_or_else(|| {
        anyhow!("dsh CLI not found on PATH; install it with `npm i -g @deepseek-ai/dsh`")
    })?;

    // On Windows, bypass the shim when possible. `dsh` on PATH is usually a
    // `.cmd` shim (`npm` or `volta`); launching it via `cmd.exe /C` with
    // `CREATE_NO_WINDOW` hides cmd.exe but the shim then re-launches a console
    // grandchild (`volta.exe`/`node.exe`). That grandchild has no parent console
    // (its parent was created with `CREATE_NO_WINDOW`), so Windows allocates a
    // fresh visible console — the "window that flashes by". Resolving the real
    // `node <dsh entry>` and spawning node directly keeps the console-subsystem
    // process itself under `CREATE_NO_WINDOW` (no grandchild, no flash).
    #[cfg(windows)]
    if let Some((node, entry)) = resolve_dsh_direct_command(&dsh) {
        tracing::info!(
            target: "dsh-bridge::spawn",
            node = %node.display(),
            entry = %entry.display(),
            "launching dsh web via node directly (bypassing shim)",
        );
        let mut cmd = Command::new(node);
        cmd.arg(entry);
        return spawn_with_args(cmd, &dsh, &["web", "--port", "0", "--no-open"], config).await;
    }

    // `--port 0` lets the OS pick a free loopback port; the readiness line
    // reports the actual bound port.
    //
    // The `dsh` CLI found on PATH is often a shim script: `dsh.cmd` (Windows
    // npm/volta shim → `cmd.exe /C`) or a `#!/bin/sh` wrapper (Unix). A raw
    // `Command::new(path)` cannot execute a batch script directly on Windows
    // and ignores shebangs on Unix, so wrap the same way kodex's ACP spawn
    // does (`agent_spawn_command`).
    let mut cmd = if cfg!(windows) && is_windows_batch_script(&dsh) {
        tracing::warn!(
            target: "dsh-bridge::spawn",
            dsh = %dsh.display(),
            "falling back to shim launch (direct node resolution unavailable)",
        );
        let mut wrapper = Command::new("cmd.exe");
        wrapper.arg("/C").arg(&dsh);
        wrapper
    } else if cfg!(not(windows)) && is_script_file(&dsh) {
        let mut wrapper = Command::new("/bin/sh");
        let mut cmd_str = dsh.to_string_lossy().to_string();
        for arg in ["web", "--port", "0", "--no-open"] {
            cmd_str.push(' ');
            cmd_str.push_str(&shell_words::quote(arg));
        }
        wrapper.arg("-c").arg(cmd_str);
        // Args are already embedded in the shell command; skip the common
        // `.args()` below.
        return spawn_with_args(wrapper, &dsh, &[], config).await;
    } else {
        Command::new(&dsh)
    };
    // `--no-open` suppresses the default-browser handoff. dsh 0.1.2 prints
    // an authenticated readiness URL containing the one-time launch token;
    // HttpClient exchanges that token for its shared auth cookie.
    spawn_with_args(cmd, &dsh, &["web", "--port", "0", "--no-open"], config).await
}

/// Shared spawn leg: set env, pipes, spawn, and read the readiness line.
async fn spawn_with_args(
    mut cmd: Command,
    dsh: &std::path::Path,
    args: &[&str],
    config: SpawnDshWebConfig,
) -> anyhow::Result<(String, DshChild)> {
    cmd.args(args);
    #[cfg(windows)]
    {
        // Spawn without a visible console window (the `dsh` shim would
        // otherwise pop a cmd/volta/node window).
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    // `dsh` is an npm shim (`#!/usr/bin/env node`): a GUI-launched app does
    // not inherit the user's interactive shell PATH, so hand the child the
    // same augmented search path used to locate the binary. Without this,
    // the shim exits immediately with `env: node: No such file or directory`
    // and no readiness line is ever printed.
    if let Ok(joined) = std::env::join_paths(search_paths()) {
        cmd.env("PATH", joined);
    }
    if !config.dsh_home.is_empty() {
        cmd.env("DSH_HOME", &config.dsh_home);
    }
    for (env_name, secret) in &config.provider_keys {
        cmd.env(env_name, secret);
    }
    for (k, v) in &config.extra_env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Inherit nothing else: the dsh profile composes its own env.

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn dsh web at {}", dsh.display()))?;

    let stdout = child
        .stdout
        .take()
        .context("dsh web stdout was not piped")?;
    let stderr = child
        .stderr
        .take()
        .context("dsh web stderr was not piped")?;

    // Drain stderr to a background task so the child's pipe does not block.
    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    tracing::debug!(target: "dsh-bridge::spawn", "dsh stderr: {}", line.trim_end());
                }
                Err(_) => break,
            }
        }
    });

    // Read stdout line-by-line until the readiness line appears, streaming
    // non-readiness lines to the debug log.
    let endpoint = tokio::time::timeout(READINESS_TIMEOUT, read_readiness_line(stdout))
        .await
        .map_err(|_| {
            anyhow!(
                "timed out waiting for dsh web readiness line ({}s)",
                READINESS_TIMEOUT.as_secs()
            )
        })??;

    // The stderr drain can keep running; it ends when the child exits.
    let _ = stderr_task;

    // Audit trail: tie the ps-visible pid to the endpoint so quit-time kills
    // and startup reaps can be matched against this line in app.log.
    if let Some(pid) = child.id() {
        tracing::info!(
            target: "dsh-bridge::spawn",
            pid,
            endpoint = %endpoint,
            "dsh web spawned"
        );
        // Unix only: parent-death watchdog so an abruptly-killed Kodex
        // (force-quit, SIGKILL, panic) cannot leave the server running.
        // Windows is covered by the kill-on-close job object below. The
        // handle is dropped immediately: the watchdog self-exits when either
        // side dies and tokio reaps the orphan.
        #[cfg(unix)]
        drop(spawn_exit_watchdog(std::process::id(), pid));
    }

    let mut child = DshChild::new(child);
    #[cfg(windows)]
    child.enable_kill_on_drop_job();
    Ok((endpoint, child))
}

/// Windows job object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: closing the
/// handle kills every process assigned to it, including descendants spawned
/// after assignment. Mirrors `acp-core`'s hidden-agent process handling so
/// closing Kodex reaps the whole `dsh web` tree (shim + node), not just the
/// direct child.
#[cfg(windows)]
struct WindowsKillOnDropJob(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl std::fmt::Debug for WindowsKillOnDropJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowsKillOnDropJob")
            .finish_non_exhaustive()
    }
}

#[cfg(windows)]
unsafe impl Send for WindowsKillOnDropJob {}

#[cfg(windows)]
impl WindowsKillOnDropJob {
    fn for_child(child: &Child) -> anyhow::Result<Self> {
        use std::mem::{size_of, zeroed};
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                anyhow::bail!("CreateJobObjectW failed");
            }

            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set_ok = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &mut info as *mut _ as *mut _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if set_ok == 0 {
                CloseHandle(job);
                anyhow::bail!("SetInformationJobObject failed");
            }

            let process = child.raw_handle().unwrap_or(std::ptr::null_mut()) as HANDLE;
            let assign_ok = AssignProcessToJobObject(job, process);
            if assign_ok == 0 {
                CloseHandle(job);
                anyhow::bail!("AssignProcessToJobObject failed");
            }

            Ok(Self(job))
        }
    }
}

#[cfg(windows)]
impl Drop for WindowsKillOnDropJob {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Read dsh stdout lines until one matches the readiness pattern, returning
/// the resolved `http://127.0.0.1:<port>` endpoint.
async fn read_readiness_line(stdout: tokio::process::ChildStdout) -> anyhow::Result<String> {
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(anyhow!("dsh web exited before printing the readiness line"));
        }
        let trimmed = line.trim_end();
        tracing::debug!(target: "dsh-bridge::spawn", "dsh stdout: {}", trimmed);
        if let Some(url) = parse_readiness_line(trimmed) {
            return Ok(url);
        }
    }
}

/// Parse the dsh web readiness line (`dsh web: http://127.0.0.1:<port>`) and
/// return the local loopback URL. dsh may also print a LAN URL suffix; we
/// keep the loopback one.
fn parse_readiness_line(line: &str) -> Option<String> {
    let prefix = "dsh web:";
    let rest = line.strip_prefix(prefix)?.trim_start();
    // Take the first whitespace-separated token (the URL). dsh may append
    // `(LAN: http://...)` after the local URL.
    let url = rest.split_whitespace().next()?;
    if url.starts_with("http://") || url.starts_with("https://") {
        Some(url.to_string())
    } else {
        None
    }
}

/// Locate the `dsh` binary using the same PATH search kodex uses for agent
/// CLIs (including GUI-launched process PATH gaps). Mirrors
/// `app_core::settings::agent_cli::find_binary` without taking a dependency
/// on `app_core` (which would reverse the crate layering).
fn find_dsh_binary() -> Option<std::path::PathBuf> {
    find_binary("dsh")
}

/// Resolve the real `node <dsh entry>` launch for the installed `dsh` CLI on
/// this machine, bypassing the npm/volta shim. Returns `(node, entry)` when
/// resolvable. Used by callers that must run a `dsh` subcommand (e.g. a version
/// check) without letting the shim chain (`cmd.exe → volta.exe → node`) open a
/// visible console window.
pub fn resolve_dsh_launch() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let dsh = find_dsh_binary()?;
    #[cfg(windows)]
    {
        if let Some(direct) = resolve_dsh_direct_command(&dsh) {
            return Some(direct);
        }
    }
    None
}

/// Resolve the real `node <npm-cli.js>` launch behind the npm shim, so the npm
/// registry query can run without letting the npm/Volta shim chain open a
/// visible console window on Windows.
#[cfg(windows)]
pub fn resolve_npm_launch() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let npm = search_paths()
        .iter()
        .map(|dir| dir.join("npm.cmd"))
        .find(|candidate| candidate.is_file())?;
    let text = std::fs::read_to_string(&npm).ok()?;

    let node = if is_volta_npm_shim(&text, &npm) {
        find_volta_node().or_else(|| find_binary("node"))
    } else {
        find_binary("node").or_else(find_volta_node)
    }?;

    // npm-style shim: extract the `node_modules/.../npm-cli.js` path from the
    // shim text, if present.
    if let Some(entry) = parse_npm_shim_entry(&text, &npm) {
        return Some((node, entry));
    }

    // Volta's npm launcher is a tiny `npm.exe` (not a batch file with an npm
    // JS path in it). Locate the npm installation that Volta manages.
    if is_volta_npm_shim(&text, &npm) {
        let entry = resolve_volta_npm_entry()?;
        return Some((node, entry));
    }

    None
}

/// Whether `npm.cmd` is a Volta launcher shim rather than the standard npm shim.
#[cfg(windows)]
fn is_volta_npm_shim(text: &str, shim: &std::path::Path) -> bool {
    text.contains("volta")
        || text.contains("%~dpn0.exe")
        || shim.to_string_lossy().contains("Volta")
}

/// Locate the npm CLI entry managed by Volta under
/// `%LOCALAPPDATA%\Volta\tools\image\npm\<version>\bin\npm-cli.js`.
#[cfg(windows)]
fn resolve_volta_npm_entry() -> Option<std::path::PathBuf> {
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        roots.push(
            std::path::PathBuf::from(local)
                .join("Volta")
                .join("tools")
                .join("image")
                .join("npm"),
        );
    }
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        roots.push(
            std::path::PathBuf::from(program_files)
                .join("Volta")
                .join("tools")
                .join("image")
                .join("npm"),
        );
    }
    for npm_root in roots {
        let Ok(rd) = std::fs::read_dir(&npm_root) else {
            continue;
        };
        let mut versions: Vec<std::path::PathBuf> = rd
            .flatten()
            .map(|entry| entry.path())
            .filter(|p| p.is_dir())
            .collect();
        versions.sort();
        for version in versions.into_iter().rev() {
            let entry = version.join("bin").join("npm-cli.js");
            if entry.is_file() {
                return Some(entry);
            }
        }
    }
    None
}

/// No-op on non-Windows; npm on Unix can be spawned directly without a
/// console-window shim.
#[cfg(not(windows))]
pub fn resolve_npm_launch() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    None
}

/// Whether `path` is a Windows batch script (`.cmd`/`.bat`), which cannot be
/// executed directly by `Command::new` and must go through `cmd.exe /C`.
#[cfg(windows)]
fn is_windows_batch_script(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        })
}

#[cfg(not(windows))]
fn is_windows_batch_script(_path: &std::path::Path) -> bool {
    false
}

/// Whether `path` is a `#!/bin/sh`-style script, which `Command::new` cannot
/// execute directly on Unix (no shebang interpretation).
#[cfg(not(windows))]
fn is_script_file(path: &std::path::Path) -> bool {
    use std::io::Read;
    if let Ok(mut file) = std::fs::File::open(path) {
        let mut buf = [0u8; 2];
        if file.read_exact(&mut buf).is_ok() {
            return buf == [0x23, 0x21]; // "#!"
        }
    }
    false
}

#[cfg(windows)]
fn is_script_file(_path: &std::path::Path) -> bool {
    false
}

/// Resolve the real `node <dsh entry>` invocation behind a Windows shim, so the
/// console-subsystem `node` can be spawned directly under `CREATE_NO_WINDOW`
/// instead of through the shim chain (which lets `volta.exe`/`node.exe` allocate
/// a fresh visible console). Returns `None` when the shim cannot be resolved —
/// the caller then falls back to the shim path.
///
/// Handles the two common npm/volta shim shapes:
/// - npm `.cmd`: the entry `.js` path is spelled out in the shim text.
/// - volta `.cmd`: `volta run <name>`, resolved via Volta's package layout.
#[cfg(windows)]
fn resolve_dsh_direct_command(
    dsh: &std::path::Path,
) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    if !is_windows_batch_script(dsh) {
        return None;
    }
    let text = std::fs::read_to_string(dsh).ok()?;
    // For a volta shim, prefer Volta's real node image over `node` on PATH:
    // `C:\Program Files\Volta\node.exe` is itself a small shim that re-launches
    // the real node, which would allocate a fresh visible console (the flash)
    // just like the dsh shim does. The real node lives under
    // `%LOCALAPPDATA%\Volta\tools\image\node\<ver>\node.exe`.
    let node = if text.contains("volta") {
        find_volta_node().or_else(|| find_binary("node"))
    } else {
        find_binary("node").or_else(find_volta_node)
    }?;

    // npm-style shim: extract the `node_modules/.../bin.js` path from the text.
    if let Some(entry) = parse_npm_shim_entry(&text, dsh) {
        return Some((node, entry));
    }

    // volta-style shim: `volta run dsh ...` — locate the installed package by
    // its bin name under Volta's image package layout.
    if text.contains("volta") {
        let name = dsh.file_stem()?.to_str()?.to_string();
        let entry = resolve_volta_package_entry(&name)?;
        return Some((node, entry));
    }

    None
}

/// Extract the `node_modules/<pkg>/<entry>.js` path an npm `.cmd` shim invokes.
/// The shim text uses `%~dp0` for the shim's directory, which we expand.
#[cfg(windows)]
fn parse_npm_shim_entry(text: &str, shim: &std::path::Path) -> Option<std::path::PathBuf> {
    let dir = shim.parent()?;
    let parts: Vec<&str> = text.split('"').collect();
    // After split on `"`, odd indices are the quoted token contents.
    for token in parts.iter().skip(1).step_by(2) {
        if !token.ends_with(".js") || !token.contains("node_modules") {
            continue;
        }
        let expanded = token.replace("%~dp0", dir.to_string_lossy().as_ref());
        let candidate = std::path::PathBuf::from(expanded.trim());
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Resolve a Volta-installed global package's entry file for the given shim
/// name. Volta stores packages under `%LOCALAPPDATA%\Volta\tools\image\packages`
/// in two shapes:
/// - unscoped: `packages\<name>\node_modules\<name>\package.json`
/// - scoped:   `packages\<scope>\<name>\node_modules\<scope>\<name>\package.json`
/// We only probe those two exact slots — never descending into dependency
/// `node_modules` — then read `package.json`'s `bin.<name>` entry.
#[cfg(windows)]
fn resolve_volta_package_entry(name: &str) -> Option<std::path::PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    let root = std::path::PathBuf::from(local)
        .join("Volta")
        .join("tools")
        .join("image")
        .join("packages");

    // Unscoped package: packages\<name>\node_modules\<name>\package.json
    let unscoped = root
        .join(name)
        .join("node_modules")
        .join(name)
        .join("package.json");
    if let Some(entry) = bin_entry_from_package_json(&unscoped, name) {
        return Some(entry);
    }

    // Scoped package: iterate `@scope` directories under `packages`.
    let rd = std::fs::read_dir(&root).ok()?;
    for scope_entry in rd.flatten() {
        let scope = scope_entry.path();
        let is_scope = scope
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with('@'));
        if !scope.is_dir() || !is_scope {
            continue;
        }
        let pkg_json = scope
            .join(name)
            .join("node_modules")
            .join(scope.file_name()?)
            .join(name)
            .join("package.json");
        if let Some(entry) = bin_entry_from_package_json(&pkg_json, name) {
            return Some(entry);
        }
    }
    None
}

/// Read `package.json` and return the absolute path of the `bin.<name>` entry.
#[cfg(windows)]
fn bin_entry_from_package_json(
    pkg_json: &std::path::Path,
    name: &str,
) -> Option<std::path::PathBuf> {
    let raw = std::fs::read_to_string(pkg_json).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let entry = match value.get("bin")? {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(map) => map.get(name)?.as_str()?.to_string(),
        _ => return None,
    };
    let candidate = pkg_json.parent()?.join(entry.as_str());
    candidate.is_file().then_some(candidate)
}

/// Locate Volta's real `node.exe` under `%LOCALAPPDATA%\Volta\tools\image\node`
/// (falling back to `%ProgramFiles%\Volta\tools\image\node`). This is the actual
/// node binary, not the `C:\Program Files\Volta\node.exe` shim that re-launches
/// it. Volta names version dirs like `22.22.1`; the newest is preferred.
#[cfg(windows)]
fn find_volta_node() -> Option<std::path::PathBuf> {
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        roots.push(
            std::path::PathBuf::from(local)
                .join("Volta")
                .join("tools")
                .join("image")
                .join("node"),
        );
    }
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        roots.push(
            std::path::PathBuf::from(program_files)
                .join("Volta")
                .join("tools")
                .join("image")
                .join("node"),
        );
    }
    for node_root in roots {
        let Ok(rd) = std::fs::read_dir(&node_root) else {
            continue;
        };
        let mut versions: Vec<std::path::PathBuf> = rd
            .flatten()
            .map(|entry| entry.path())
            .filter(|p| p.is_dir())
            .collect();
        versions.sort();
        for v in versions.into_iter().rev() {
            let node = v.join("node.exe");
            if node.is_file() {
                return Some(node);
            }
        }
    }
    None
}

fn find_binary(binary: &str) -> Option<std::path::PathBuf> {
    let names: Vec<String> = if cfg!(windows) {
        vec![
            format!("{binary}.exe"),
            format!("{binary}.cmd"),
            format!("{binary}.bat"),
        ]
    } else {
        vec![binary.to_string()]
    };

    search_paths()
        .into_iter()
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|path| path.is_file())
}

/// PATH search including GUI-launched process gaps. Mirrors
/// `app_core::settings::agent_cli::search_paths`.
fn search_paths() -> Vec<std::path::PathBuf> {
    let mut search_paths: Vec<std::path::PathBuf> = Vec::new();
    if let Some(paths) = std::env::var_os("PATH") {
        search_paths.extend(std::env::split_paths(&paths));
    }
    if let Some(home) = dirs_next::home_dir() {
        for suffix in [".local/bin", "bin"] {
            let p = home.join(suffix);
            if !search_paths.contains(&p) {
                search_paths.push(p);
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(app_data) = std::env::var_os("APPDATA") {
            let p = std::path::PathBuf::from(&app_data).join("npm");
            if !search_paths.contains(&p) {
                search_paths.push(p);
            }
        }
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
            let p = std::path::PathBuf::from(&local_app_data).join("npm");
            if !search_paths.contains(&p) {
                search_paths.push(p);
            }
            // Volta shims (dsh installed via `volta install`).
            let volta = std::path::PathBuf::from(&local_app_data)
                .join("Volta")
                .join("bin");
            if !search_paths.contains(&volta) {
                search_paths.push(volta);
            }
        }
        if let Some(user_profile) = std::env::var_os("USERPROFILE") {
            let volta = std::path::PathBuf::from(&user_profile)
                .join(".volta")
                .join("bin");
            if !search_paths.contains(&volta) {
                search_paths.push(volta);
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        for extra in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"] {
            let p = std::path::PathBuf::from(extra);
            if !search_paths.contains(&p) {
                search_paths.push(p);
            }
        }
        // Common version-manager install roots; GUI-launched apps do not
        // inherit the shell hooks that put these on PATH.
        if let Some(home) = dirs_next::home_dir() {
            for suffix in [".volta/bin", ".local/share/mise/shims", ".asdf/shims"] {
                let p = home.join(suffix);
                if !search_paths.contains(&p) {
                    search_paths.push(p);
                }
            }
            if let Some(entries) = nvm_version_bin_dirs(&home) {
                for p in entries {
                    if !search_paths.contains(&p) {
                        search_paths.push(p);
                    }
                }
            }
        }
    }
    search_paths
}

/// nvm keeps each installed Node version at `~/.nvm/versions/node/vX.Y.Z/bin`.
/// Return those bin directories, newest version first, so a GUI-launched app
/// can find a `dsh` shim installed through nvm without the shell hook.
#[cfg(target_os = "macos")]
fn nvm_version_bin_dirs(home: &std::path::Path) -> Option<Vec<std::path::PathBuf>> {
    let versions_dir = home.join(".nvm/versions/node");
    let mut dirs: Vec<std::path::PathBuf> = std::fs::read_dir(&versions_dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().join("bin"))
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs.reverse();
    if dirs.is_empty() { None } else { Some(dirs) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_readiness_line_local_url() {
        assert_eq!(
            parse_readiness_line("dsh web: http://127.0.0.1:41237"),
            Some("http://127.0.0.1:41237".to_string())
        );
    }

    #[test]
    fn parse_readiness_line_with_lan_suffix() {
        assert_eq!(
            parse_readiness_line("dsh web: http://127.0.0.1:41237 (LAN: http://192.168.1.5:41237)"),
            Some("http://127.0.0.1:41237".to_string())
        );
    }

    #[test]
    fn parse_readiness_line_rejects_non_dsh_line() {
        assert_eq!(parse_readiness_line("listening on port 3080"), None);
        assert_eq!(parse_readiness_line(""), None);
    }

    #[tokio::test]
    async fn kill_child_terminates_a_spawned_process() {
        // Spawn a long-running child and verify kill_child stops it promptly.
        // `kill_child` calls `start_kill()` then `wait()`, so a quick `Ok`
        // return proves the process was terminated (a live `ping`/`sleep`
        // would otherwise block `wait()` for its full duration).
        let mut cmd = tokio::process::Command::new(if cfg!(windows) { "cmd" } else { "sleep" });
        if cfg!(windows) {
            cmd.args(["/c", "ping", "-n", "60", "127.0.0.1"]);
        } else {
            cmd.arg("60");
        }
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let child = cmd.spawn().unwrap();
        let started = std::time::Instant::now();
        kill_child(DshChild::new(child)).unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "kill_child blocked; the child was not terminated"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exit_watchdog_kills_the_dsh_pid_when_the_parent_dies() {
        // Two `sleep` stand-ins: one as the "Kodex parent", one as the
        // "dsh web" server. SIGKILLing the parent (force-quit/crash) must
        // make the watchdog terminate the server within one poll interval
        // (3s) plus the SIGTERM grace (4s). `try_wait` — not `kill -0` —
        // decides liveness: a killed-but-unreaped zombie still answers
        // `kill -0`, and the victim is this test's own child.
        let spawn_sleeper = || {
            Command::new("sleep")
                .arg("60")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn sleeper")
        };
        let mut parent = spawn_sleeper();
        let mut victim = spawn_sleeper();
        let parent_pid = parent.id().unwrap();
        let victim_pid = victim.id().unwrap();
        assert!(spawn_exit_watchdog(parent_pid, victim_pid).is_some());

        let _ = unsafe { libc::kill(parent_pid as libc::pid_t, libc::SIGKILL) };
        let _ = parent.wait().await;

        let mut victim_dead = false;
        for _ in 0..200 {
            match victim.try_wait() {
                Ok(Some(_)) => {
                    victim_dead = true;
                    break;
                }
                _ => std::thread::sleep(Duration::from_millis(50)),
            }
        }
        // Best-effort cleanup if the watchdog failed.
        let _ = unsafe { libc::kill(victim_pid as libc::pid_t, libc::SIGKILL) };
        let _ = victim.wait().await;
        assert!(
            victim_dead,
            "exit watchdog did not kill the dsh pid within 10s of parent death"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exit_watchdog_exits_when_the_dsh_pid_dies_first() {
        // Clean-quit path: the server dies while Kodex lives on. The watchdog
        // must notice and exit by itself instead of lingering (or, worse,
        // signalling a reused pid later).
        let mut victim = Command::new("sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn victim");
        let victim_pid = victim.id().unwrap();
        // The test process itself is the "parent" and stays alive.
        let mut watchdog =
            spawn_exit_watchdog(std::process::id(), victim_pid).expect("watchdog spawn");

        let _ = unsafe { libc::kill(victim_pid as libc::pid_t, libc::SIGKILL) };
        let _ = victim.wait().await;

        // The watchdog polls every 3s; allow 8s for it to notice and exit.
        // `try_wait` — not `kill -0` — decides: an exited-but-unreaped
        // zombie still answers `kill -0`, and the watchdog is this test's
        // own child.
        let mut watchdog_gone = false;
        for _ in 0..160 {
            match watchdog.try_wait() {
                Ok(Some(_)) => {
                    watchdog_gone = true;
                    break;
                }
                _ => std::thread::sleep(Duration::from_millis(50)),
            }
        }
        assert!(
            watchdog_gone,
            "exit watchdog lingered after the dsh pid died"
        );
    }

    #[tokio::test]
    async fn spawn_dsh_web_fails_fast_without_a_dsh_binary() {
        // 11.5: with no resolvable `dsh` binary, spawn fails fast with a
        // diagnostic (no hang, no partial process). The PATH search only looks
        // at the current process PATH, so a developer machine that happens to
        // have `dsh` installed is skipped rather than flaky.
        if find_dsh_binary().is_some() {
            eprintln!("skipping: a real `dsh` binary is on PATH");
            return;
        }
        let config = SpawnDshWebConfig {
            dsh_home: std::env::temp_dir().display().to_string(),
            ..Default::default()
        };
        let result = spawn_dsh_web(config).await;
        let err = result.expect_err("spawn must fail without a dsh binary");
        assert!(
            err.to_string().contains("dsh CLI not found"),
            "unexpected error: {err}"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn spawn_dsh_web_runs_a_cmd_shim_and_parses_readiness() {
        // Reproduces the Windows `dsh.cmd` shim case: a batch script that
        // echoes the readiness line must be launched via `cmd.exe /C` and the
        // line parsed. Drives the same wrapper `spawn_dsh_web` builds for a
        // `.cmd` binary, without touching the process PATH.
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("dsh.cmd");
        std::fs::write(
            &shim,
            "@echo off\r\necho dsh web: http://127.0.0.1:41237\r\nping -n 30 127.0.0.1 >nul\r\n",
        )
        .unwrap();
        assert!(is_windows_batch_script(&shim));

        let mut cmd = Command::new("cmd.exe");
        cmd.arg("/C").arg(&shim);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let endpoint = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            read_readiness_line(stdout),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(endpoint, "http://127.0.0.1:41237");
        let _ = child.kill().await;
    }

    // ---- orphan reap ----

    #[test]
    fn parse_ps_rows_tolerates_field_padding_and_skips_malformed() {
        let raw = "  123  456 node /opt/homebrew/bin/dsh web --port 0 --no-open\n\
                   7    1 /bin/sleep 60\n\
                   not-a-pid 9 whatever\n\
                   10 11\n";
        let rows = parse_ps_rows(raw);
        assert_eq!(
            rows,
            vec![
                PsRow {
                    pid: 123,
                    ppid: 456,
                    command: "node /opt/homebrew/bin/dsh web --port 0 --no-open".into()
                },
                PsRow {
                    pid: 7,
                    ppid: 1,
                    command: "/bin/sleep 60".into()
                },
            ]
        );
    }

    #[test]
    fn is_dsh_web_command_matches_only_the_server_invocation() {
        // Kodex spawns `dsh web` via a shell wrapper, so the argv may carry an
        // absolute path (script shim), a bare `dsh`, or a node-prefixed line.
        for line in [
            "node /opt/homebrew/bin/dsh web --port 0 --no-open",
            "/opt/homebrew/bin/dsh web --port 0 --no-open",
            "dsh web",
            "/bin/sh -c /opt/homebrew/bin/dsh web --port 0",
        ] {
            assert!(is_dsh_web_command(line), "should match: {line}");
        }
        // The bigram (`dsh`, `web`) is deliberately a loose candidate filter:
        // a non-dsh argv that happens to contain the adjacent pair (e.g.
        // `node server.js dsh web`) cannot be told apart from a node-launched
        // dsh by argv alone. The real guards are the orphan check (dead
        // parent) and the DSH_HOME environment match, tested separately.
        for line in [
            "dsh acp",
            "dsh --version",
            "dshweb serve",
            "sleep 60",
            "/opt/homebrew/bin/dshx web",
            "/opt/homebrew/bin/dsh website",
        ] {
            assert!(!is_dsh_web_command(line), "should NOT match: {line}");
        }
    }

    #[test]
    fn orphan_rules_flag_dead_parents_only() {
        let live: std::collections::HashSet<u32> = [1u32, 100, 200, 300].into_iter().collect();
        // Live parent -> not an orphan.
        assert!(!is_orphaned(
            &PsRow {
                pid: 300,
                ppid: 100,
                command: "dsh web".into()
            },
            &live
        ));
        // Reparented to init after the parent died.
        assert!(is_orphaned(
            &PsRow {
                pid: 301,
                ppid: 1,
                command: "dsh web".into()
            },
            &live
        ));
        // Parent pid no longer in the live table (exited, still reported).
        assert!(is_orphaned(
            &PsRow {
                pid: 302,
                ppid: 9999,
                command: "dsh web".into()
            },
            &live
        ));
    }

    #[test]
    fn env_declares_dsh_home_matches_both_platform_shapes() {
        let home = "/Users/dev/.kodex/dsh";
        // macOS `ps eww -o command=`: space-separated KEY=VALUE pairs
        // prefixed to the command line.
        let mac = "DSH_HOME=/Users/dev/.kodex/dsh PATH=/usr/bin node /opt/homebrew/bin/dsh web --port 0 --no-open";
        assert!(env_declares_dsh_home(mac, home));
        // Linux /proc/<pid>/environ: NUL-separated records.
        let linux = "PATH=/usr/bin\0DSH_HOME=/Users/dev/.kodex/dsh\0TERM=xterm\0";
        assert!(env_declares_dsh_home(linux, home));
        // Any other home (a user-launched server) must NOT match.
        assert!(!env_declares_dsh_home(
            "DSH_HOME=/Users/dev/.dsh node dsh web",
            home
        ));
        assert!(!env_declares_dsh_home("", home));
    }

    /// End-to-end reap against REAL orphaned fake `dsh web` processes.
    /// Spawns each fake via a shell that exits immediately (reparenting the
    /// child to init — exactly what a crashed Kodex leaves behind), then
    /// verifies: 1. a reap targeting a DIFFERENT DSH_HOME leaves the orphans
    /// alone (user-launched servers on other homes survive); 2. a reap
    /// targeting the orphan's own DSH_HOME terminates exactly that one.
    ///
    /// The fake `dsh` script uses a `#!/usr/bin/env node` interpreter because
    /// the reap's env check reads `DSH_HOME` from the target's environment,
    /// and macOS hides the environment of platform binaries (SIP) — only
    /// user-installed binaries like `node` expose it to `ps eww`. The real
    /// kodex-spawned `dsh web` is a `node` process, so this is the same shape
    /// the production reap sees. Every poll is bounded — the test can fail
    /// fast, never hang.
    #[cfg(unix)]
    #[test]
    fn reap_kills_only_kodex_home_orphans() {
        // Skip (not fail) on machines without node: the fake's env must be
        // ps-visible, which requires a non-platform interpreter.
        if std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping: no `node` on PATH for the reap live test");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        // Two script dirs, both with a `dsh` script: distinct command lines
        // (one per dir) let the test tell the two orphans apart.
        let make_fake = |name: &str| {
            let fake_dir = dir.path().join(name);
            std::fs::create_dir_all(&fake_dir).unwrap();
            let script = fake_dir.join("dsh");
            std::fs::write(
                &script,
                "#!/usr/bin/env node\nsetInterval(() => {}, 1000);\n",
            )
            .unwrap();
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            (fake_dir, script.display().to_string())
        };
        let (dir_a, line_a) = make_fake("a");
        let (dir_b, line_b) = make_fake("b");
        // Kodex's dsh home (dir_a) vs a "user" home (dir_b): the env must
        // differ so the reap's DSH_HOME guard is what separates them.
        let home_a = dir_a.display().to_string();
        let home_b = dir_b.display().to_string();

        let spawn_orphan = |line: &str, home: &str| {
            std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(format!("'{line}' web &"))
                .env("DSH_HOME", home)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
        };
        spawn_orphan(&line_a, &home_a);
        spawn_orphan(&line_b, &home_b);

        // Wait (bounded) until both fakes appear as orphans in the table.
        let find_orphan = |line: &str| -> Option<u32> {
            let rows = list_ps_rows_for_test();
            let live: std::collections::HashSet<u32> = rows.iter().map(|r| r.pid).collect();
            rows.iter()
                .find(|r| r.command.contains(line) && is_orphaned(r, &live))
                .map(|r| r.pid)
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let (orphan_a, orphan_b) = loop {
            if let (Some(a), Some(b)) = (find_orphan(&line_a), find_orphan(&line_b)) {
                break (a, b);
            }
            assert!(
                std::time::Instant::now() < deadline,
                "fake dsh web orphans never appeared in the process table"
            );
            std::thread::sleep(Duration::from_millis(50));
        };

        // 1. Reaping a foreign home must leave BOTH orphans alone.
        let foreign_home = dir.path().join("home-c").display().to_string();
        let reaped = reap_orphaned_dsh_web(&foreign_home);
        assert!(
            !reaped.contains(&orphan_a) && !reaped.contains(&orphan_b),
            "reap with a foreign home killed orphans: {reaped:?}"
        );

        // 2. Reaping home_a terminates home_a's orphan and leaves home_b's
        //    (a "user" server on another home) running.
        let reaped = reap_orphaned_dsh_web(&home_a);
        assert!(
            reaped.contains(&orphan_a),
            "home_a orphan not reaped: {reaped:?}"
        );
        assert!(
            !reaped.contains(&orphan_b),
            "home_b orphan must survive: {reaped:?}"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while pid_alive(orphan_a) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(!pid_alive(orphan_a), "orphan survived the reap");
        assert!(
            pid_alive(orphan_b),
            "home_b orphan was killed by the home_a reap"
        );

        // Cleanup: home_b's fake is deliberately still running; remove it so
        // the test leaves nothing behind.
        let _ = unsafe { libc::kill(orphan_b as libc::pid_t, libc::SIGKILL) };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while pid_alive(orphan_b) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(!pid_alive(orphan_b), "test cleanup failed");
    }

    /// `ps` listing for tests — the same invocation `reap_orphaned_dsh_web`
    /// uses, so the live test observes the table exactly as the reap does.
    #[cfg(unix)]
    fn list_ps_rows_for_test() -> Vec<PsRow> {
        let ps_args: &[&str] = if cfg!(target_os = "macos") {
            &["-ww", "-axo", "pid=,ppid=,command="]
        } else {
            &["-e", "-w", "-w", "-o", "pid=,ppid=,command="]
        };
        let out = std::process::Command::new("ps")
            .args(ps_args)
            .output()
            .expect("ps listing for reap test");
        parse_ps_rows(&String::from_utf8_lossy(&out.stdout))
    }
}
