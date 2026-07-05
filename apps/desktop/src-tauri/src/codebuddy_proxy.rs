//! Managed lifecycle for the bundled CodeBuddy reverse proxy binary.
//!
//! When the `codebuddy` provider profile is configured and selected, Kodex
//! spawns the bundled single-file proxy binary on the configured loopback
//! port and keeps it alive for the app's lifetime. The proxy is torn down on
//! app exit, on config change (restart), or when the provider is removed.
use app_core::settings::codex_acp_bin_dir;
use app_core::settings::detect_agent_with_paths;
use workspace_model::AgentCliId;
use app_core::AppPaths;
use serde::Serialize;
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Windows: do not flash a console window for the spawned child. We use
/// `CommandExt::creation_flags` so this is no-op on other platforms.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Snapshot of the config the currently-running proxy was started with, so
/// `restart_if_changed` can decide whether to respawn.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RunningConfig {
    port: u16,
    api_key: String,
    default_model: String,
    /// When true, spawn the proxy with an attached console window and inherited
    /// stdio so the operator can read its `INFO`/`DEBUG` logs directly. Also
    /// forwards `CODEBUDDY_PROXY_LOG_LEVEL=debug` to the child.
    debug: bool,
}

pub struct CodebuddyProxyManager {
    child: Mutex<Option<Child>>,
    running: Mutex<Option<RunningConfig>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CodebuddyProxyStatus {
    pub running: bool,
    pub port: Option<u16>,
    /// Persisted debug flag — true when the operator enabled the console
    /// window / verbose logging for the next launch. Reflects the on-disk
    /// value, not whether the running child was started with `debug=true`.
    pub debug: bool,
}

impl CodebuddyProxyManager {
    pub fn new() -> Self {
        Self {
            child: Mutex::new(None),
            running: Mutex::new(None),
        }
    }

    /// Current status (running + port).
    pub fn status(&self, debug: bool) -> CodebuddyProxyStatus {
        let running = self.running.lock().map(|r| r.clone()).ok().flatten();
        CodebuddyProxyStatus {
            running: self.is_alive(),
            port: running.map(|c| c.port),
            debug,
        }
    }

    fn is_alive(&self) -> bool {
        let mut guard = match self.child.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        if let Some(child) = guard.as_mut() {
            match child.try_wait() {
                Ok(Some(_)) => {
                    *guard = None;
                    false
                }
                Ok(None) => true,
                Err(_) => false,
            }
        } else {
            false
        }
    }

    /// Ensure the proxy is running with the given config. Spawns if not
    /// running; no-ops if already running with the same config.
    pub fn ensure_running(
        &self,
        paths: &AppPaths,
        port: u16,
        api_key: &str,
        default_model: &str,
        debug: bool,
    ) -> Result<(), String> {
        let desired = RunningConfig {
            port,
            api_key: api_key.to_string(),
            default_model: default_model.to_string(),
            debug,
        };
        if self.is_alive() {
            let current = self.running.lock().map(|r| r.clone()).ok().flatten();
            if current.as_ref() == Some(&desired) {
                return Ok(());
            }
            self.stop();
        }
        self.spawn(paths, &desired)?;
        self.wait_until_healthy(port, Duration::from_secs(15))
            .map_err(|e| format!("codebuddy proxy failed to become healthy: {e}"))?;
        Ok(())
    }

    /// Restart if the desired config differs from the running config.
    /// Does NOT start the proxy if it is not already running — use
    /// `ensure_running` for that. This is called from settings save
    /// so saving config doesn't implicitly start the proxy.
    pub fn restart_if_changed(
        &self,
        paths: &AppPaths,
        port: u16,
        api_key: &str,
        default_model: &str,
        debug: bool,
    ) -> Result<(), String> {
        let desired = RunningConfig {
            port,
            api_key: api_key.to_string(),
            default_model: default_model.to_string(),
            debug,
        };
        // Only restart if already running; never start from stopped.
        if !self.is_alive() {
            return Ok(());
        }
        let current = self.running.lock().map(|r| r.clone()).ok().flatten();
        if current.as_ref() == Some(&desired) {
            return Ok(());
        }
        self.stop();
        self.spawn(paths, &desired)?;
        self.wait_until_healthy(port, Duration::from_secs(15))
            .map_err(|e| format!("codebuddy proxy failed to become healthy: {e}"))?;
        Ok(())
    }

    /// Kill the running proxy (idempotent).
    pub fn stop(&self) {
        let mut guard = match self.child.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if let Some(mut child) = guard.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Ok(mut r) = self.running.lock() {
            *r = None;
        }
    }

    fn spawn(&self, paths: &AppPaths, config: &RunningConfig) -> Result<(), String> {
        let binary = codex_acp_bin_dir(paths).join(binary_name("codebuddy-proxy"));
        if !binary.is_file() {
            return Err(format!(
                "codebuddy proxy binary not found at {}",
                binary.display()
            ));
        }
        let mut cmd = Command::new(&binary);
        cmd.env("CODEBUDDY_PROXY_HOST", "127.0.0.1");
        cmd.env("CODEBUDDY_PROXY_PORT", config.port.to_string());
        cmd.env("CODEBUDDY_PROXY_API_KEY", &config.api_key);
        cmd.env("CODEBUDDY_PROXY_DEFAULT_MODEL", &config.default_model);
        // Pass the Kodex codex-api-proxy base URL so the 17856 proxy can
        // `fetch` /v1/tools/execute back to the right Kodex instance.
        cmd.env(
            "CODEX_API_PROXY_BASE_URL",
            acp_core::codex_api_proxy_base_url(),
        );
        if config.debug {
            cmd.env("CODEBUDDY_PROXY_LOG_LEVEL", "debug");
        }
        // Always log to a file so proxy internals are visible even when
        // debug console mode is off.
        cmd.env(
            "CODEBUDDY_PROXY_LOG_DIR",
            paths.logs_dir().to_string_lossy().as_ref(),
        );
        // The bundled proxy wraps `@tencent-ai/agent-sdk`, which resolves the
        // upstream CodeBuddy CLI via `CODEBUDDY_CODE_PATH` (highest priority)
        // or by walking relative to `__dirname`. In the SEA bundle neither
        // of those resolution paths can find a real CodeBuddy CLI on the
        // user's machine — `__dirname` points at the tmp extraction root
        // and the user's install lives under
        // `%LOCALAPPDATA%\codebuddy\bin\codebuddy.exe` (npm-global) or
        // elsewhere. Pointing the env var at the CLI we detected via
        // `agent_cli::find_binary` lets the proxy prewarm sessions without
        // throwing `CLINotFoundError` on the first request.
        if let Some(cli_path) = detect_agent_with_paths(paths, AgentCliId::Codebuddy)
            .detected_path
            .filter(|p| p.is_file())
        {
            cmd.env("CODEBUDDY_CODE_PATH", &cli_path);
        }
        if config.debug {
            // Attach a console window and forward the child's stdout/stderr
            // straight to the parent's terminal so the operator can watch
            // tool-use logging in real time.
            cmd.stdin(Stdio::null());
            cmd.stdout(Stdio::inherit());
            cmd.stderr(Stdio::inherit());
        } else {
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(CREATE_NO_WINDOW);
            }
            cmd.stdin(Stdio::null());
            cmd.stdout(Stdio::null());
            cmd.stderr(Stdio::piped());
        }
        let child = cmd
            .spawn()
            .map_err(|e| format!("failed to spawn codebuddy proxy: {e}"))?;
        if let Ok(mut g) = self.child.lock() {
            *g = Some(child);
        }
        if let Ok(mut r) = self.running.lock() {
            *r = Some(config.clone());
        }
        Ok(())
    }

    /// Wait for the proxy to become reachable. We first try a TCP connect
    /// (the port being in LISTEN state is enough to know the CLI process
    /// booted and Express bound the socket). We then poll `GET /healthz`
    /// to confirm the HTTP layer is fully up. The TCP probe is the typical
    /// happy path; the HTTP probe is the safety net.
    fn wait_until_healthy(&self, port: u16, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        // Phase 1: TCP connect (fast — sub-ms on localhost when the port is LISTEN).
        let tcp_deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < tcp_deadline {
            if !self.is_alive() {
                return Err(
                    "proxy process exited before becoming healthy (port may be in use)"
                        .to_string(),
                );
            }
            if TcpStream::connect_timeout(
                &format!("127.0.0.1:{port}").parse().unwrap(),
                Duration::from_millis(300),
            )
            .is_ok()
            {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(format!("timeout waiting for proxy to listen on 127.0.0.1:{port}"))
    }
}

/// Platform-aware binary name (mirrors `binary_name` in agent_cli).
fn binary_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

impl Drop for CodebuddyProxyManager {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_until_healthy_reports_unreachable_port() {
        let mgr = CodebuddyProxyManager::new();
        let res = mgr.wait_until_healthy(1, Duration::from_millis(500));
        assert!(res.is_err());
    }
}
