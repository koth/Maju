use app_core::AppPaths;
use serde::Serialize;
use std::net::TcpStream;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::oneshot;
use tauri::async_runtime::JoinHandle;
#[derive(Clone, Debug, PartialEq, Eq)]
struct RunningConfig {
    port: u16,
    api_key: String,
    default_model: String,
    internet_environment: String,
    debug: bool,
}
pub struct CodebuddyProxyManager {
    task: Mutex<Option<JoinHandle<()>>>,
    shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
    running: Mutex<Option<RunningConfig>>,
    alive: AtomicBool,
}
#[derive(Debug, Clone, Serialize)]
pub struct CodebuddyProxyStatus {
    pub running: bool,
    pub port: Option<u16>,
    pub internet_environment: String,
    pub debug: bool,
}
impl CodebuddyProxyManager {
    pub fn new() -> Self {
        Self { task: Mutex::new(None), shutdown_tx: Mutex::new(None), running: Mutex::new(None), alive: AtomicBool::new(false) }
    }
    pub fn status(&self, internet_environment: &str, debug: bool) -> CodebuddyProxyStatus {
        let running = self.running.lock().ok().and_then(|g| g.clone());
        CodebuddyProxyStatus {
            running: self.is_alive(),
            port: running.map(|c| c.port),
            internet_environment: internet_environment.to_string(),
            debug,
        }
    }
    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
    pub fn ensure_running(&self, paths: &AppPaths, port: u16, api_key: &str, default_model: &str, internet_environment: &str, debug: bool) -> Result<(), String> {
        let desired = RunningConfig { port, api_key: api_key.to_string(), default_model: default_model.to_string(), internet_environment: internet_environment.to_string(), debug };
        if self.is_alive() {
            let current = self.running.lock().ok().and_then(|g| g.clone());
            if current.as_ref() == Some(&desired) { return Ok(()); }
            self.stop();
        }
        self.spawn_inline(paths, &desired)?;
        self.wait_until_healthy(port, Duration::from_secs(15)).map_err(|e| format!("codebuddy proxy failed to become healthy: {e}"))?;
        Ok(())
    }
    pub fn restart_if_changed(&self, paths: &AppPaths, port: u16, api_key: &str, default_model: &str, internet_environment: &str, debug: bool) -> Result<(), String> {
        let desired = RunningConfig { port, api_key: api_key.to_string(), default_model: default_model.to_string(), internet_environment: internet_environment.to_string(), debug };
        if !self.is_alive() { return Ok(()); }
        let current = self.running.lock().ok().and_then(|g| g.clone());
        if current.as_ref() == Some(&desired) { return Ok(()); }
        self.stop();
        self.spawn_inline(paths, &desired)?;
        self.wait_until_healthy(port, Duration::from_secs(15)).map_err(|e| format!("codebuddy proxy failed to become healthy: {e}"))?;
        Ok(())
    }
    pub fn stop(&self) {
        let tx = self.shutdown_tx.lock().ok().and_then(|mut g| g.take());
        if let Some(tx) = tx { let _ = tx.send(()); }
        let handle = self.task.lock().ok().and_then(|mut g| g.take());
        if let Some(handle) = handle { handle.abort(); }
        if let Ok(mut r) = self.running.lock() { *r = None; }
        self.alive.store(false, Ordering::SeqCst);
    }
    fn spawn_inline(&self, paths: &AppPaths, config: &RunningConfig) -> Result<(), String> {
        let port = config.port;
        let default_model = config.default_model.clone();
        let cwd = std::env::current_dir().ok();
        let max_turns: Option<u32> = std::env::var("CODEBUDDY_MAX_TURNS").ok().and_then(|s| s.parse().ok());
        let max_sessions: usize = std::env::var("CODEBUDDY_MAX_SESSIONS").ok().and_then(|s| s.parse().ok()).unwrap_or(8);
        let idle_timeout = Duration::from_secs(std::env::var("CODEBUDDY_IDLE_TIMEOUT_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(600));
        let api_key = if config.api_key.is_empty() { None } else { Some(config.api_key.clone()) };
        // Resolve the CodeBuddy CLI binary the same way the ACP agent launcher
        // does: `detect_agent_with_paths(Codebuddy)` → `find_binary("codebuddy")`
        // scans PATH plus the npm-global / `%LOCALAPPDATA%\codebuddy\bin` roots
        // that a GUI-launched app does not inherit. The SDK's own
        // `search_dirs()` only looks next to the running exe (and
        // `CARGO_MANIFEST_DIR`), which never contains the user-installed
        // `codebuddy` CLI inside the desktop app — so without this the proxy
        // fails with "CodeBuddy CLI binary not found". The resolved path is
        // plumbed through `ProxyConfig` → `SessionOptions::codebuddy_code_path`,
        // which the SDK spawns directly, bypassing its narrow search.
        let cli_path = app_core::settings::detect_agent_with_paths(
            paths,
            workspace_model::AgentCliId::Codebuddy,
        )
        .detected_path;
        // Build the env forwarded to every spawned CodeBuddy CLI subprocess.
        // The legacy TS/desktop launcher set these on the CLI child; the
        // in-process Rust proxy must do the same via SessionOptions::env, or
        // the headless CLI cannot authenticate and `initialize` hangs until
        // the 60s control timeout.
        let mut cli_env: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
        // `CODEBUDDY_API_KEY` — backend auth. Without it the CLI has no
        // credentials in headless mode and blocks on `initialize`.
        if !config.api_key.is_empty() {
            cli_env.insert("CODEBUDDY_API_KEY".to_string(), config.api_key.clone());
        }
        // `CODEBUDDY_INTERNET_ENVIRONMENT` — selects the backend endpoint
        // (`internal` | `ioa`).
        cli_env.insert(
            "CODEBUDDY_INTERNET_ENVIRONMENT".to_string(),
            config.internet_environment.clone(),
        );
        // `PATH` — GUI-launched apps do not inherit the user's interactive
        // PATH; augment it with `search_paths()` (current PATH + npm-global
        // / `%LOCALAPPDATA%\codebuddy\bin` roots) so a node-shim `codebuddy`
        // CLI resolves `node`, exactly like the ACP agent launcher.
        if let Ok(joined) = std::env::join_paths(app_core::settings::search_paths()) {
            cli_env.insert("PATH".to_string(), joined.to_string_lossy().into_owned());
        }
        let proxy_cfg = codebuddy_proxy::server::ProxyConfig { port, default_model, cwd, max_turns, max_sessions, idle_timeout, api_key, cli_path, cli_env, debug: config.debug };
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let handle = tauri::async_runtime::spawn(async move {
            if let Err(e) = codebuddy_proxy::server::run(proxy_cfg, shutdown_rx).await {
                eprintln!("[codebuddy-proxy] server error: {e}");
            }
        });
        if let Ok(mut g) = self.task.lock() { *g = Some(handle); }
        if let Ok(mut g) = self.shutdown_tx.lock() { *g = Some(shutdown_tx); }
        if let Ok(mut r) = self.running.lock() { *r = Some(config.clone()); }
        self.alive.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn wait_until_healthy(&self, port: u16, _timeout: Duration) -> Result<(), String> {
        let addr_str = format!("127.0.0.1:{port}");
        let addr: std::net::SocketAddr = addr_str.parse().unwrap();
        let tcp_deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < tcp_deadline {
            if !self.is_alive() { return Err("proxy task exited before becoming healthy".to_string()); }
            if TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok() {
                std::thread::sleep(Duration::from_millis(150));
                if !self.is_alive() { return Err("proxy task exited after TCP probe".to_string()); }
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(format!("timeout waiting for proxy to listen on {addr_str}"))
    }
}
impl Drop for CodebuddyProxyManager {
    fn drop(&mut self) { self.stop(); }
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
