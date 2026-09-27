//! Browser-use: one real Chromium per live session activation, exposed to
//! managed agents as a managed MCP server.
//!
//! The lifecycle is [`session_resource::SessionResourceRegistry`], which owns
//! lazy acquisition, per-session serialization, and deterministic disposal.
//! This crate supplies the browser-specific parts: how the provider process
//! is launched, how its catalog is discovered and exposed to the model, and
//! how a tool result is projected.

pub mod catalog;
pub mod installer;
pub mod mcp;
pub mod orphan;
pub mod provider;
pub mod provision;
pub mod proxy;

use std::sync::Arc;
use std::time::Duration;

use catalog::{ExposedTool, ProviderCatalog, ProviderTool};
use provider::ProviderLaunch;
use session_resource::{CancelToken, RegistryError, ResourceFactory, SessionResourceRegistry};
use workspace_model::BrowserSettings;

/// A tool call result, projected from whatever shape the provider returned.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolResult {
    /// Text the model reads.
    pub text: String,
    /// Image bytes, when the tool captured something. These are handed to the
    /// screenshot pipeline rather than inlined, so a full-resolution capture
    /// never reaches the session log.
    pub images: Vec<Vec<u8>>,
}

impl ToolResult {
    pub fn text_only(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            images: Vec::new(),
        }
    }

    pub fn has_images(&self) -> bool {
        !self.images.is_empty()
    }
}

/// Errors from a browser operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserError {
    /// The provider process could not be started, or exited during startup.
    Launch(String),
    /// The provider is running but the call failed.
    Provider { tool: String, detail: String },
    /// The call exceeded the configured timeout.
    Timeout { tool: String, after_ms: u64 },
    /// The call was cancelled.
    Cancelled { tool: String },
}

impl std::fmt::Display for BrowserError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BrowserError::Launch(detail) => {
                write!(formatter, "browser provider failed to start: {detail}")
            }
            BrowserError::Provider { tool, detail } => {
                write!(formatter, "browser tool {tool} failed: {detail}")
            }
            BrowserError::Timeout { tool, after_ms } => {
                write!(
                    formatter,
                    "browser tool {tool} timed out after {after_ms}ms"
                )
            }
            BrowserError::Cancelled { tool } => write!(formatter, "browser tool {tool} cancelled"),
        }
    }
}

impl std::error::Error for BrowserError {}

/// What the registry treats as the per-session browser resource.
///
/// The provider process is opaque here: the registry only needs something it
/// can hold and release. The concrete driver that talks to it over stdio is
/// attached by the service that builds the registry, which keeps this crate
/// testable without a browser.
pub struct BrowserResource {
    pub launch: ProviderLaunch,
    pub server_name: String,
    /// Populated after the first successful handshake, so a session that never
    /// calls a tool never pays for a catalog fetch.
    pub catalog: Option<ProviderCatalog>,
    /// The live provider process, when one was actually started. `None` for a
    /// session that only resolved its launch description.
    process: Option<tokio::process::Child>,
    client: Option<Arc<mcp::McpClient>>,
}

impl BrowserResource {
    pub fn new(launch: ProviderLaunch, server_name: impl Into<String>) -> Self {
        Self {
            launch,
            server_name: server_name.into(),
            catalog: None,
            process: None,
            client: None,
        }
    }

    /// Attach a started process and its client.
    pub fn with_process(
        mut self,
        process: tokio::process::Child,
        client: Arc<mcp::McpClient>,
    ) -> Self {
        self.client = Some(client);
        self.process = Some(process);
        self
    }

    pub fn client(&self) -> Option<&Arc<mcp::McpClient>> {
        self.client.as_ref()
    }

    pub fn process_mut(&mut self) -> Option<&mut tokio::process::Child> {
        self.process.as_mut()
    }
}

/// Launches one provider process per session.
pub struct BrowserFactory {
    settings: BrowserSettings,
    node_executable: std::path::PathBuf,
    package_root: std::path::PathBuf,
    /// Kodex data root, used to resolve a persistent profile directory. Never
    /// the user's browser profile: persistent mode writes only under here.
    data_root: std::path::PathBuf,
    server_name: String,
    /// When false, acquisition resolves the launch description without
    /// starting a process. Used to exercise lifecycle behaviour without a
    /// browser on the machine.
    spawn_process: bool,
}

impl BrowserFactory {
    pub fn new(
        settings: BrowserSettings,
        node_executable: std::path::PathBuf,
        package_root: std::path::PathBuf,
        data_root: std::path::PathBuf,
    ) -> Self {
        Self {
            settings,
            node_executable,
            package_root,
            data_root,
            server_name: "playwright-mcp".to_string(),
            spawn_process: true,
        }
    }

    pub fn with_server_name(mut self, server_name: impl Into<String>) -> Self {
        self.server_name = server_name.into();
        self
    }

    /// Resolve the launch description without starting a provider process.
    pub fn without_spawn(mut self) -> Self {
        self.spawn_process = false;
        self
    }
}

impl ResourceFactory for BrowserFactory {
    type Resource = BrowserResource;
    type Error = BrowserError;

    fn label(&self) -> &'static str {
        "browser"
    }

    async fn acquire(&self, _session_id: &str) -> Result<BrowserResource, BrowserError> {
        // The launch description is resolved here rather than in the caller so
        // that a settings change between session start and first use cannot
        // produce a process that does not match the advertised configuration.
        let launch = provider::build_launch(
            &self.settings,
            self.node_executable.clone(),
            self.package_root.clone(),
            crate::provision::profile_dir(&self.data_root, &self.settings.profile_name),
        );

        // The process is started here, once, on the session's first tool call.
        if !self.spawn_process {
            return Ok(BrowserResource::new(launch, self.server_name.clone()));
        }

        let mut command = tokio::process::Command::new(&launch.executable);
        command
            .args(&launch.args)
            // The MCP conversation runs over the child's own stdio, so these
            // pipes are the transport — not a diagnostic convenience. Without
            // them the provider inherits Kodex's stdio, `StdioTransport::
            // from_child` finds nothing to take, and acquisition fails with
            // "produced no stdout to talk over" on a provider that started
            // perfectly well.
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // A provider that cannot be spoken to is a startup failure, not a
            // provider that hangs forever waiting for a handshake.
            .kill_on_drop(true);
        let parent: std::collections::HashMap<String, String> = std::env::vars().collect();
        command.env_clear();
        // The owner marker makes this process identifiable to the orphan reaper
        // if Kodex is killed before the child is reaped normally.
        let mut env = orphan::provider_env(&parent);
        // The provider is started as `<node> <cli.js>`, so the interpreter is
        // found by absolute path — but the helpers it launches by name are
        // not, and this process's PATH is whatever the app was launched with.
        crate::provider::ProviderLaunch::prepend_executable_dir(&mut env, &self.node_executable);
        command.envs(&env);

        // Chromium refuses to start against a missing `--user-data-dir`
        // parent, and creating it here keeps the failure at the point the
        // setting was made rather than at the first tool call.
        if let Some(parent) = launch
            .args
            .iter()
            .position(|arg| arg == "--user-data-dir")
            .and_then(|index| launch.args.get(index + 1))
        {
            let _ = std::fs::create_dir_all(parent);
        }

        let mut child = command.spawn().map_err(|error| {
            BrowserError::Launch(format!(
                "{} {}: {error}",
                launch.executable.display(),
                launch.args.join(" ")
            ))
        })?;

        let transport = mcp::StdioTransport::from_child(&mut child)
            .map_err(|error| BrowserError::Launch(error.to_string()))?;
        let client = Arc::new(
            mcp::McpClient::new(Arc::new(transport))
                .with_timeout(Duration::from_millis(self.settings.tool_call_timeout_ms)),
        );

        // Handshake during acquisition so a provider that cannot start fails
        // the first tool call with a clear reason rather than every later one.
        client
            .initialize()
            .await
            .map_err(|error| BrowserError::Launch(error.to_string()))?;

        Ok(BrowserResource::new(launch, self.server_name.clone()).with_process(child, client))
    }

    async fn release(
        &self,
        _session_id: &str,
        mut resource: BrowserResource,
    ) -> Result<(), BrowserError> {
        // `kill_on_drop` already reaps the process; this makes the intent
        // explicit and covers a provider that outlived its client.
        if let Some(child) = resource.process_mut() {
            let _ = child.start_kill();
        }
        Ok(())
    }
}

/// The browser capability: a registry plus the settings it was built from.
pub struct BrowserService {
    registry: Arc<SessionResourceRegistry<BrowserFactory>>,
    settings: BrowserSettings,
}

impl BrowserService {
    pub fn new(
        settings: BrowserSettings,
        node_executable: std::path::PathBuf,
        package_root: std::path::PathBuf,
        data_root: std::path::PathBuf,
    ) -> Self {
        Self::from_factory(BrowserFactory::new(
            settings.clone(),
            node_executable,
            package_root,
            data_root,
        ))
    }

    /// Build a service around a caller-supplied factory.
    pub fn from_factory(factory: BrowserFactory) -> Self {
        let settings = factory.settings.clone();
        // Two modes cannot be shared between sessions. Attach holds the user's
        // own browser, and a persistent profile is a single Chromium profile
        // directory that only one process may open. Launch gives each session
        // its own browser and needs no such limit.
        let exclusive = matches!(
            settings.mode,
            workspace_model::BrowserMode::Attach | workspace_model::BrowserMode::Persistent
        );
        let registry = if exclusive {
            SessionResourceRegistry::new(factory).exclusive()
        } else {
            SessionResourceRegistry::new(factory)
        };
        Self {
            registry: Arc::new(registry),
            settings,
        }
    }

    pub fn settings(&self) -> &BrowserSettings {
        &self.settings
    }

    pub fn registry(&self) -> &Arc<SessionResourceRegistry<BrowserFactory>> {
        &self.registry
    }

    /// Expose the provider's catalog to the model.
    pub fn exposed_tools(
        &self,
        catalog: &ProviderCatalog,
    ) -> Result<Vec<ExposedTool>, catalog::CatalogError> {
        catalog::expose("playwright-mcp", catalog)
    }

    /// Run one browser tool call under the session's operation lock.
    pub async fn call<T, Op, Fut>(
        &self,
        session_id: &str,
        cancel: &CancelToken,
        operation: Op,
    ) -> Result<T, RegistryError>
    where
        Op: FnOnce(Arc<BrowserResource>) -> Fut,
        Fut: std::future::Future<Output = Result<T, BrowserError>>,
    {
        self.registry.run(session_id, cancel, operation).await
    }

    /// Dispose one session's browser.
    pub async fn close_session(&self, session_id: &str) -> Result<(), RegistryError> {
        self.registry.close(session_id).await
    }

    /// Allow a resumed activation to acquire a fresh browser.
    pub fn reopen(&self, session_id: &str) {
        self.registry.forget(session_id);
    }

    /// Dispose every session's browser.
    pub async fn shutdown(&self) -> Result<(), RegistryError> {
        self.registry.close_all().await
    }
}

/// Find the tool a model-visible name refers to.
pub fn resolve<'a>(catalog: &'a ProviderCatalog, exposed: &str) -> Option<&'a ProviderTool> {
    let leaf = catalog::leaf_name(exposed)?;
    catalog.find(leaf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A service whose factory resolves launch descriptions without starting a
    /// provider process, so lifecycle behaviour can be tested without a
    /// browser. Process behaviour itself is covered by the transport tests.
    fn service() -> BrowserService {
        BrowserService::from_factory(
            BrowserFactory::new(
                BrowserSettings {
                    enabled: true,
                    ..BrowserSettings::default()
                },
                std::path::PathBuf::from("/usr/bin/node"),
                std::path::PathBuf::from("/pkg/@playwright/mcp"),
                std::path::PathBuf::from("/data"),
            )
            .without_spawn(),
        )
    }

    fn catalog() -> ProviderCatalog {
        ProviderCatalog {
            tools: vec![
                ProviderTool {
                    name: "browser_click".to_string(),
                    description: "Click".to_string(),
                    input_schema: json!({ "type": "object" }),
                },
                ProviderTool {
                    name: "browser_take_screenshot".to_string(),
                    description: "Shot".to_string(),
                    input_schema: json!({ "type": "object" }),
                },
            ],
        }
    }

    #[tokio::test]
    async fn no_browser_is_started_until_a_tool_runs() {
        let service = service();
        assert_eq!(
            service.registry().status("session-1"),
            session_resource::ResourceStatus::Idle
        );
    }

    #[tokio::test]
    async fn a_tool_call_acquires_one_browser_for_the_session() {
        let service = service();
        let cancel = CancelToken::new();

        for _ in 0..3 {
            service
                .call("session-1", &cancel, |resource| async move {
                    Ok(resource.server_name.clone())
                })
                .await
                .unwrap();
        }

        assert_eq!(
            service.registry().status("session-1"),
            session_resource::ResourceStatus::Active
        );
    }

    #[tokio::test]
    async fn closing_refuses_later_calls_until_reopened() {
        let service = service();
        let cancel = CancelToken::new();

        service
            .call("session-1", &cancel, |_| async { Ok(()) })
            .await
            .unwrap();
        service.close_session("session-1").await.unwrap();

        assert!(
            service
                .call("session-1", &cancel, |_| async { Ok(()) })
                .await
                .is_err()
        );

        service.reopen("session-1");
        service
            .call("session-1", &cancel, |_| async { Ok(()) })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn attach_mode_refuses_a_second_session() {
        let service = BrowserService::from_factory(
            BrowserFactory::new(
                BrowserSettings {
                    enabled: true,
                    mode: workspace_model::BrowserMode::Attach,
                    endpoint: "http://127.0.0.1:9222".to_string(),
                    allow_attach: true,
                    ..BrowserSettings::default()
                },
                std::path::PathBuf::from("/usr/bin/node"),
                std::path::PathBuf::from("/pkg/@playwright/mcp"),
                std::path::PathBuf::from("/data"),
            )
            .without_spawn(),
        );
        let cancel = CancelToken::new();

        service
            .call("session-1", &cancel, |_| async { Ok(()) })
            .await
            .unwrap();
        assert!(
            service
                .call("session-2", &cancel, |_| async { Ok(()) })
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn launch_mode_allows_concurrent_sessions() {
        let service = service();
        let cancel = CancelToken::new();

        service
            .call("session-1", &cancel, |_| async { Ok(()) })
            .await
            .unwrap();
        service
            .call("session-2", &cancel, |_| async { Ok(()) })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn provider_errors_surface_without_killing_the_session() {
        let service = service();
        let cancel = CancelToken::new();

        let failed: Result<(), _> = service
            .call("session-1", &cancel, |_| async {
                Err(BrowserError::Provider {
                    tool: "browser_click".to_string(),
                    detail: "selector not found".to_string(),
                })
            })
            .await;
        assert!(failed.is_err());

        // The operation lock must have been released.
        service
            .call("session-1", &cancel, |_| async { Ok(()) })
            .await
            .unwrap();
    }

    #[test]
    fn exposed_surface_comes_from_the_discovered_catalog() {
        let service = service();
        let exposed = service.exposed_tools(&catalog()).unwrap();

        assert_eq!(exposed.len(), 2);
        assert_eq!(
            exposed[0].exposed_name,
            "mcp__playwright-mcp__browser_click"
        );
        assert!(!exposed[0].effect.is_read());
        assert!(exposed[1].effect.is_read());
    }

    #[test]
    fn a_model_name_resolves_back_to_the_provider_tool() {
        let catalog = catalog();
        let resolved = resolve(&catalog, "mcp__playwright-mcp__browser_click").unwrap();
        assert_eq!(resolved.name, "browser_click");
        assert!(resolve(&catalog, "mcp__playwright-mcp__nope").is_none());
    }

    #[test]
    fn tool_result_separates_text_from_images() {
        let result = ToolResult {
            text: "captured".to_string(),
            images: vec![vec![0x89, b'P', b'N', b'G']],
        };
        assert!(result.has_images());
        assert!(!ToolResult::text_only("done").has_images());
    }

    #[tokio::test]
    async fn shutdown_disposes_every_session() {
        let service = service();
        let cancel = CancelToken::new();

        for id in ["session-1", "session-2"] {
            service
                .call(id, &cancel, |_| async { Ok(()) })
                .await
                .unwrap();
        }
        service.shutdown().await.unwrap();

        assert_eq!(
            service.registry().status("session-1"),
            session_resource::ResourceStatus::Closed
        );
    }

    #[tokio::test]
    async fn a_missing_provider_executable_fails_the_first_call_with_a_reason() {
        // Real spawn path, pointed at an executable that does not exist. The
        // failure must name the command so the user can see what was attempted.
        let service = BrowserService::from_factory(BrowserFactory::new(
            BrowserSettings {
                enabled: true,
                ..BrowserSettings::default()
            },
            std::path::PathBuf::from("/nonexistent/node-for-tests"),
            std::path::PathBuf::from("/pkg/@playwright/mcp"),
            std::path::PathBuf::from("/data"),
        ));
        let cancel = CancelToken::new();

        let failed: Result<(), _> = service
            .call("session-1", &cancel, |_| async { Ok(()) })
            .await;
        let message = failed
            .expect_err("spawning a missing executable must fail")
            .to_string();
        assert!(
            message.contains("browser provider failed to start"),
            "got {message}"
        );
        assert!(
            message.contains("nonexistent/node-for-tests"),
            "the failing command should be named, got {message}",
        );
    }

    #[tokio::test]
    async fn a_provider_that_never_answers_the_handshake_fails_rather_than_hanging() {
        // `/bin/cat` starts, holds stdin open, and never writes a handshake.
        // Acquisition must give up instead of blocking the session forever.
        let service = BrowserService::from_factory(BrowserFactory::new(
            BrowserSettings {
                enabled: true,
                tool_call_timeout_ms: 300,
                ..BrowserSettings::default()
            },
            std::path::PathBuf::from("/bin/cat"),
            std::path::PathBuf::from("/pkg/@playwright/mcp"),
            std::path::PathBuf::from("/data"),
        ));
        let cancel = CancelToken::new();

        let started = std::time::Instant::now();
        let failed: Result<(), _> = service
            .call("session-1", &cancel, |_| async { Ok(()) })
            .await;
        let elapsed = started.elapsed();

        assert!(
            failed.is_err(),
            "a silent provider must not be treated as ready"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "handshake should time out promptly, took {elapsed:?}",
        );
    }
}
