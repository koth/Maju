use std::time::Duration;
fn main() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let port: u16 = std::env::var("CODEBUDDY_PROXY_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(17856);
        let default_model = std::env::var("CODEBUDDY_DEFAULT_MODEL")
            .unwrap_or_else(|_| "claude-sonnet-5".to_string());
        let max_turns: Option<u32> = std::env::var("CODEBUDDY_MAX_TURNS")
            .ok()
            .and_then(|s| s.parse().ok());
        let max_sessions: usize = std::env::var("CODEBUDDY_MAX_SESSIONS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8);
        let idle_timeout_secs: u64 = std::env::var("CODEBUDDY_IDLE_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(600);
        let debug: bool = std::env::var("CODEBUDDY_DEBUG")
            .ok()
            .map(|s| matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);
        let cwd = std::env::current_dir().ok();
        // Standalone binary: forward the upstream CodeBuddy API key +
        // internet environment from this process's env to each CLI
        // subprocess, mirroring the legacy TS proxy's `env` forwarding.
        // Without `CODEBUDDY_API_KEY` the headless CLI cannot authenticate
        // and `initialize` hangs until the 60s control timeout.
        let mut cli_env: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
        if let Ok(v) = std::env::var("CODEBUDDY_API_KEY") {
            if !v.trim().is_empty() {
                cli_env.insert("CODEBUDDY_API_KEY".to_string(), v);
            }
        }
        if let Ok(v) = std::env::var("CODEBUDDY_INTERNET_ENVIRONMENT") {
            if !v.trim().is_empty() {
                cli_env.insert("CODEBUDDY_INTERNET_ENVIRONMENT".to_string(), v);
            }
        }
        let cfg = codebuddy_proxy::server::ProxyConfig {
            port,
            default_model,
            cwd,
            max_turns,
            max_sessions,
            idle_timeout: Duration::from_secs(idle_timeout_secs),
            api_key: std::env::var("CODEBUDDY_API_KEY").ok(),
            // Standalone binary: let the SDK resolve the CLI itself (env
            // CODEBUDDY_CODE_PATH or exe-relative search).
            cli_path: None,
            cli_env,
            debug,
        };
        let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
        codebuddy_proxy::server::run(cfg, rx).await
    })
}
