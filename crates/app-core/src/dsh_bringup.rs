//! Kodex-managed `dsh web` bring-up.
//!
//! When a session selects the DeepSeek Harness agent, Kodex owns the whole
//! process lifecycle: writes `~/.kodex/dsh/settings.yaml` from the BYOK
//! provider catalog, spawns `dsh web --port 0`, discovers the bound loopback
//! endpoint from the readiness line, and registers the endpoint for the
//! session's `SessionConfig.harness_endpoint`.
//!
//! One `DshBringup` (process-wide, initialized by the desktop shell) shares a
//! single [`HarnessHostRegistry`] with the ACP harness backend, so a second
//! session targeting the same spawned endpoint reuses the same `dsh web`
//! process (one process, one SSE pair). The spawned child is attached to the
//! registry's `HarnessHost`; when the last sharing session exits, the host
//! teardown kills the child. A crash or force-quit runs no destructors, so
//! orphaned children from previous runs are reclaimed by
//! [`DshBringup::reap_orphaned_hosts`] (startup hook + spawn branch).

use crate::AppPaths;
use crate::settings::{build_dsh_settings_config, dsh_provider_keys, ensure_dsh_proxy_routing};
use dsh_bridge::{
    DshChild, HarnessHost, HarnessHostRegistry, HarnessMcpServer, HarnessPatchConfig,
    SpawnDshWebConfig, reap_orphaned_dsh_web, spawn_dsh_web, write_harness_patch, write_settings,
};
use std::sync::{Arc, Mutex, OnceLock};

/// Process-wide bring-up singleton. Initialized once by the desktop shell so
/// the harness backend and `Application` share the same host registry.
static BRINGUP: OnceLock<Arc<DshBringup>> = OnceLock::new();

/// Kodex's local MCP servers as mounted on a `dsh web` process.
///
/// The rows are what `dsh-mcp-client` reads from the `--patch` overlay; the
/// handles are what keeps each server alive. They are owned by the managed host
/// so the servers live exactly as long as the `dsh web` process that talks to
/// them (`dsh` connects at boot, so the servers must already be listening
/// before the spawn).
#[derive(Default)]
pub struct HarnessExposedMcp {
    pub rows: Vec<HarnessMcpServer>,
    /// Held only for its `Drop`: dropping the lease unregisters this harness
    /// process's sessions from the shared servers.
    pub web_tools: Option<crate::web_tools_mcp::WebToolsLease>,
    pub image: Option<crate::image_mcp::ImageMcpLease>,
}

/// Initialize the process-wide bring-up singleton. The registry is shared with
/// the ACP harness backend (`acp_core::set_harness_backend`), so a spawned
/// `dsh web` host is reused across sessions and workspaces.
pub fn init_dsh_bringup(registry: Arc<HarnessHostRegistry>) {
    let _ = BRINGUP.set(Arc::new(DshBringup::new(registry)));
}

/// Access the process-wide bring-up singleton, if initialized. Falls back to a
/// standalone instance when unset (e.g. tests) so harness sessions do not
/// panic in unit tests that never call `init_dsh_bringup`.
pub fn dsh_bringup() -> Arc<DshBringup> {
    BRINGUP
        .get()
        .cloned()
        .unwrap_or_else(|| Arc::new(DshBringup::standalone()))
}

/// Kodex-managed `dsh web` lifecycle: settings write, spawn, endpoint
/// discovery, and shared-host registration.
pub struct DshBringup {
    registry: Arc<HarnessHostRegistry>,
    /// A single-thread tokio runtime used to drive the async spawn + readiness
    /// wait. Created once so the first spawn's worker threads are not churned
    /// per session.
    runtime: tokio::runtime::Runtime,
    /// The currently-managed `dsh web` host, if any. Only one managed host is
    /// supported (one spawned process); sessions that share its endpoint reuse
    /// it via the registry.
    managed: Mutex<Option<ManagedHost>>,
}

struct ManagedHost {
    endpoint: String,
    host: Arc<HarnessHost>,
    /// Kodex-local MCP servers mounted on this host. Field is never read: it
    /// exists so the servers stop when the `dsh web` process they serve is
    /// torn down.
    _exposed_mcp: HarnessExposedMcp,
}

impl DshBringup {
    pub fn new(registry: Arc<HarnessHostRegistry>) -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build dsh bringup tokio runtime");
        Self {
            registry,
            runtime,
            managed: Mutex::new(None),
        }
    }

    pub fn standalone() -> Self {
        Self::new(Arc::new(HarnessHostRegistry::new()))
    }

    pub fn registry(&self) -> &Arc<HarnessHostRegistry> {
        &self.registry
    }

    /// Return the managed harness host, reusing the same authenticated
    /// client/mux as sessions. Creates it only when bring-up succeeds.
    pub fn ensure_harness_host(&self, paths: &AppPaths) -> Result<Arc<HarnessHost>, String> {
        let endpoint = self.ensure_harness_endpoint(paths)?;
        self.registry
            .acquire(endpoint)
            .map_err(|e| format!("failed to connect dsh harness host: {e}"))
    }

    /// Ensure a `dsh web` process is running and return its loopback endpoint.
    ///
    /// On first call: start Kodex's local MCP servers, write `settings.yaml` and
    /// the patch overlay that mounts them, spawn `dsh web --port 0`, wait for
    /// the readiness line, attach the child to the shared host, and cache the
    /// endpoint. Subsequent calls reuse the cached endpoint as long as the host
    /// is still alive (and keep the MCP servers it was brought up with).
    pub fn ensure_harness_endpoint(&self, paths: &AppPaths) -> Result<String, String> {
        if let Some(managed) = self
            .managed
            .lock()
            .map_err(|_| "dsh bringup lock poisoned")?
            .as_ref()
            && self.registry.host_alive(&managed.endpoint)
        {
            return Ok(managed.endpoint.clone());
        }

        // 0. Start Kodex's local MCP servers and collect the rows that mount
        //    them on the harness. They must be listening before the spawn —
        //    `dsh-mcp-client` connects during boot — and they live exactly as
        //    long as this `dsh web` process. A server that fails to start
        //    degrades to "fewer tools", never to a failed bring-up.
        let exposed_mcp = crate::application::sessions::harness_exposed_mcp(paths);

        // 1. Write ~/.kodex/dsh/settings.yaml from the BYOK provider catalog.
        //    First ensure the local codex_api_proxy is running and knows every
        //    configured source provider, so the harness's chat/completions
        //    traffic (routed through the proxy) can reach each upstream.
        ensure_dsh_proxy_routing(paths);
        let config = build_dsh_settings_config(paths).map_err(|e| e.to_string())?;
        write_settings(&paths.dsh_settings_path(), &config)
            .map_err(|e| format!("failed to write dsh settings.yaml: {e}"))?;

        // 1.05 Write the Kodex-owned cordis patch overlay. `settings.yaml` only
        //      carries the two sections Kodex owns; bundle-row plugin config
        //      (the session-title token budget, the optional pinned route, and
        //      the local MCP servers) has to travel as a `--patch` overlay
        //      applied after the profile layer.
        let title_route = crate::settings::session_title_route(paths);
        let patch_config = HarnessPatchConfig::with_title_route(title_route)
            .with_mcp_servers(exposed_mcp.rows.clone());
        write_harness_patch(&paths.dsh_patch_path(), &patch_config)
            .map_err(|e| format!("failed to write dsh patch overlay: {e}"))?;

        // 1.1 Reclaim `dsh web` processes orphaned by a previous crashed run
        //     BEFORE spawning the replacement: a crash/force-quit runs no
        //     destructors, so the old child (parent dead, DSH_HOME = Kodex's
        //     home) is a pure leak. The fresh child is spawned after this and
        //     can never be a candidate.
        let home = paths.dsh_dir().display().to_string();
        reap_orphaned_dsh_web(&home);

        // 1.5 Seed ~/.kodex/dsh/AGENTS.md (the dsh user-global instruction
        //     file) when missing, so file edits go through the dedicated file
        //     tools and stay observable as structured diffs.
        ensure_dsh_agents_md(paths)?;

        // 2. Spawn `dsh web --port 0` and wait for the readiness line.
        let provider_keys = dsh_provider_keys(paths);
        let spawn_config = SpawnDshWebConfig {
            dsh_home: home,
            provider_keys,
            extra_env: vec![
                ("DSH_TELEMETRY_DISABLED".to_string(), "1".to_string()),
                ("DSH_PERMISSION_MODE".to_string(), "danger-full-access".to_string()),
            ],
            patch_overlay: Some(paths.dsh_patch_path().display().to_string()),
        };
        let (endpoint, child) = self
            .runtime
            .block_on(spawn_dsh_web(spawn_config))
            .map_err(|e| e.to_string())?;

        // 3. Acquire (or reuse) the shared host for this endpoint and attach the
        //    child so the last session exit tears the process down.
        let host = self
            .registry
            .acquire(endpoint.clone())
            .map_err(|e| e.to_string())?;
        host.attach_child(child);

        // 4. Cache the managed host so subsequent sessions reuse it. The
        //    exposed MCP servers ride along: dropping this entry (shutdown, or
        //    a dead host replaced by the next bring-up) stops them.
        *self
            .managed
            .lock()
            .map_err(|_| "dsh bringup lock poisoned")? = Some(ManagedHost {
            endpoint: endpoint.clone(),
            host,
            _exposed_mcp: exposed_mcp,
        });
        Ok(endpoint)
    }

    /// Shut down the managed `dsh web` process, if any. Called on app exit;
    /// the harness host teardown kills the spawned child.
    pub fn shutdown(&self) {
        if let Ok(mut managed) = self.managed.lock() {
            if let Some(host) = managed.take() {
                host.host.teardown();
            }
        }
    }

    /// Reclaim `dsh web` processes orphaned by a previous crashed run. Safe
    /// to call at any time: hosts of live Kodex instances keep their live
    /// parent, and user-launched servers carry another `DSH_HOME`, so both
    /// are left alone (see `reap_orphaned_dsh_web`). Called from the desktop
    /// shell's startup hook so leftovers are freed even before any session
    /// needs a harness host, and again from `ensure_harness_endpoint`'s
    /// spawn branch.
    pub fn reap_orphaned_hosts(&self) {
        let paths = match AppPaths::resolve() {
            Ok(paths) => paths,
            Err(err) => {
                tracing::warn!(
                    target: "dsh_bringup",
                    error = %err,
                    "orphan reap skipped: AppPaths resolve failed"
                );
                return;
            }
        };
        let home = paths.dsh_dir().display().to_string();
        let reaped = reap_orphaned_dsh_web(&home);
        if !reaped.is_empty() {
            // `eprintln` is invisible for a GUI-launched app (stderr goes
            // nowhere); tracing lands in ~/.kodex/logs/app.log where the
            // per-pid reap lines from `reap_orphaned_dsh_web` also live.
            tracing::info!(
                target: "dsh_bringup",
                pids = ?reaped,
                "reaped orphaned dsh web process(es) from previous runs"
            );
        }
    }
}

/// Seed `~/.kodex/dsh/AGENTS.md` (the dsh `agent-instructions` plugin's
/// user-global instruction file) when it does not exist yet. dsh folds this
/// file into every session's context at startup as a durable
/// `<system-reminder>`, which is the supported way to steer the agent without
/// patching the shipped system prompt (this dsh build hardcodes the preset
/// root to its npm package, so deployment-side persona overrides are not
/// possible). Created ONLY when missing: afterwards the file belongs to the
/// user and Kodex never overwrites their edits.
fn ensure_dsh_agents_md(paths: &AppPaths) -> Result<(), String> {
    let path = paths.dsh_dir().join("AGENTS.md");
    if path.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(paths.dsh_dir())
        .map_err(|e| format!("failed to create dsh home: {e}"))?;
    std::fs::write(&path, KODEX_DSH_AGENTS_MD)
        .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    Ok(())
}

/// The seeded user-global instruction block. The point: file modifications
/// must go through the dedicated `edit`/`write` tools so Kodex's diff
/// pipeline observes them — shell-based edits (sed/python/tee/redirection)
/// produce no structured diff and are invisible to the review UI.
const KODEX_DSH_AGENTS_MD: &str = r#"<!-- Managed by Kodex: this file is created once when the app starts and
     is never overwritten afterwards. Edit it freely — it is applied to every
     dsh session on this machine. -->

# File editing policy

- Apply ALL file modifications with the dedicated file tools: `edit` for
  partial changes, `write` only for new files or full rewrites.
- NEVER modify files through shell commands — no `sed`, `awk`, `perl`,
  `python`, `node -e`, `tee`, output redirection (`>` / `>>`), heredocs, or
  `patch`. Shell-based edits bypass the structured diff pipeline and are
  invisible to code review. If a file tool call fails, fix the arguments and
  retry the tool instead of falling back to shell.
- Shell is for running programs, tests, and read-only inspection (`cat`,
  `grep`, `ls`, `git status` / `git diff` / `git log`). Mutating anything in
  the working tree belongs to the file tools.
- When a change touches several spots in one file, make consecutive `edit`
  calls to that file rather than scripting the edits in one shell command.
"#;

// Silence unused import when DshChild is only used via attach_child in host.rs.
#[allow(unused_imports)]
use dsh_bridge::DshChild as _DshChildRef;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_agents_md_when_missing_and_never_overwrites() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = AppPaths::from_root(tmp.path());

        // First bring-up seeds the file with the file-editing policy.
        ensure_dsh_agents_md(&paths).expect("seed");
        let path = paths.dsh_dir().join("AGENTS.md");
        let seeded = std::fs::read_to_string(&path).expect("read seeded file");
        assert!(seeded.contains("File editing policy"));
        assert!(seeded.contains("NEVER modify files through shell"));

        // A later bring-up must treat the file as user-owned: edits survive.
        std::fs::write(&path, "# my own rules\n").expect("simulate user edit");
        ensure_dsh_agents_md(&paths).expect("second run");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read after second run"),
            "# my own rules\n"
        );
    }
}
