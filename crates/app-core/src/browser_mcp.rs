//! Browser-use as a managed MCP capability.
//!
//! One MCP server per process exposes the provider's tool catalog; the browser
//! each call reaches is the *calling session's* browser. The split matters: a
//! process-wide server is what lets the tool names be stable across sessions,
//! while the browser has to be per session, because two sessions must never
//! share cookies, tabs, or a serialized operation queue.
//!
//! A session registers, receives a token, and presents it on every call. The
//! token is the only thing a tool call carries, so one session cannot address
//! another's browser even if it guesses an id.
//!
//! Image results never travel back through the tool result. They go to the
//! screenshot pipeline, and the model receives a handle — inlined only when the
//! route reports vision support.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use browser_service::BrowserService;
use browser_service::catalog::{ExposedTool, ProviderCatalog};
use browser_service::mcp::ToolCallResult;
use session_resource::CancelToken;
use workspace_model::ScreenshotHandle;

use crate::screenshot_pipeline::{Capture, Delivery, ScreenshotPipeline};

/// Why a tool call could not be served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserCallError {
    /// The presented token is not registered, or its session has ended.
    UnknownSession,
    /// The tool name is not part of the exposed catalog.
    UnknownTool { name: String },
    /// The provider reported a failure.
    Provider { tool: String, detail: String },
    /// The call exceeded its deadline.
    Timeout { tool: String },
    /// The session was closed mid-call.
    Closed { tool: String },
    /// A screenshot could not be persisted.
    Capture(String),
}

impl std::fmt::Display for BrowserCallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BrowserCallError::UnknownSession => {
                write!(formatter, "browser session is not registered")
            }
            BrowserCallError::UnknownTool { name } => {
                write!(formatter, "browser tool \"{name}\" is not available")
            }
            BrowserCallError::Provider { tool, detail } => {
                write!(formatter, "browser tool {tool} failed: {detail}")
            }
            BrowserCallError::Timeout { tool } => {
                write!(formatter, "browser tool {tool} timed out")
            }
            BrowserCallError::Closed { tool } => {
                write!(
                    formatter,
                    "browser tool {tool} was interrupted by session close"
                )
            }
            BrowserCallError::Capture(detail) => {
                write!(formatter, "screenshot could not be stored: {detail}")
            }
        }
    }
}

impl std::error::Error for BrowserCallError {}

/// A tool result, already projected for the model.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BrowserToolResult {
    /// Text the model reads.
    pub text: String,
    /// Captures persisted during this call. The model gets a handle rather
    /// than bytes, so a full-resolution page image never enters the log.
    pub screenshots: Vec<ScreenshotHandle>,
    /// How the captures should reach the model.
    pub delivery: Option<Delivery>,
    /// The provider reported the call as failed.
    pub is_error: bool,
}

impl BrowserToolResult {
    /// The text a model should see, with capture references appended when the
    /// model cannot see images itself.
    pub fn model_text(&self) -> String {
        if self.screenshots.is_empty() {
            return self.text.clone();
        }
        let mut text = self.text.clone();
        for (index, handle) in self.screenshots.iter().enumerate() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&format!(
                "screenshot {} saved to {} ({}x{}, {} bytes)",
                index + 1,
                handle.path,
                handle.width,
                handle.height,
                handle.byte_size,
            ));
        }
        text
    }
}

/// Per-session registration.
struct Lease {
    session_id: String,
    cancel: CancelToken,
}

/// The browser capability, shared by every session in the process.
pub struct BrowserMcpService {
    service: Arc<BrowserService>,
    pipeline: Arc<ScreenshotPipeline>,
    leases: Mutex<HashMap<String, Lease>>,
    next_token: AtomicU64,
    /// Whether the active model route understands image input. Updated when a
    /// session registers, since it is a property of the route.
    model_vision: Mutex<HashMap<String, bool>>,
    catalog: Mutex<Option<Vec<ExposedTool>>>,
    /// Serializes catalog priming. Two agents listing tools at the same moment
    /// must not both spawn a provider; the loser of the race re-checks after
    /// acquiring this and finds the catalog already installed.
    prime: tokio::sync::Mutex<()>,
}

impl BrowserMcpService {
    pub fn new(service: Arc<BrowserService>, pipeline: Arc<ScreenshotPipeline>) -> Self {
        Self {
            service,
            pipeline,
            leases: Mutex::new(HashMap::new()),
            next_token: AtomicU64::new(1),
            model_vision: Mutex::new(HashMap::new()),
            catalog: Mutex::new(None),
            prime: tokio::sync::Mutex::new(()),
        }
    }

    pub fn service(&self) -> &Arc<BrowserService> {
        &self.service
    }

    pub fn pipeline(&self) -> &Arc<ScreenshotPipeline> {
        &self.pipeline
    }

    /// Register a session and return the token its tool calls must present.
    ///
    /// Registering twice for the same session replaces the previous lease, so a
    /// reconnect does not accumulate tokens.
    pub fn register_session(&self, session_id: &str, model_vision: bool) -> String {
        let token = format!("browser-{}", self.next_token.fetch_add(1, Ordering::SeqCst));

        if let Ok(mut leases) = self.leases.lock() {
            leases.retain(|_, lease| lease.session_id != session_id);
            leases.insert(
                token.clone(),
                Lease {
                    session_id: session_id.to_string(),
                    cancel: CancelToken::new(),
                },
            );
        }
        if let Ok(mut vision) = self.model_vision.lock() {
            vision.insert(session_id.to_string(), model_vision);
        }
        token
    }

    /// Drop a session's lease and dispose its browser.
    pub async fn unregister_session(&self, token: &str) {
        let lease = self.leases.lock().ok().and_then(|mut leases| {
            let lease = leases.remove(token);
            lease
        });

        if let Some(lease) = lease {
            // The cancel token stops any wait the registry is holding, then the
            // close releases the process.
            lease.cancel.cancel();
            let _ = self.service.close_session(&lease.session_id).await;
            if let Ok(mut vision) = self.model_vision.lock() {
                vision.remove(&lease.session_id);
            }
        }
    }

    /// Number of live leases, for diagnostics and leak checks.
    pub fn lease_count(&self) -> usize {
        self.leases.lock().map(|leases| leases.len()).unwrap_or(0)
    }

    /// Install the catalog discovered from the provider.
    ///
    /// A catalog is resolved per activation, so a provider upgrade changes the
    /// exposed surface without a Kodex release. Installing an empty or invalid
    /// catalog is rejected rather than clearing the surface, so a transient
    /// provider hiccup cannot strip the tools from a running session.
    pub fn install_catalog(
        &self,
        catalog: &ProviderCatalog,
    ) -> Result<usize, browser_service::catalog::CatalogError> {
        let exposed = self.service.exposed_tools(catalog)?;
        let count = exposed.len();
        if let Ok(mut slot) = self.catalog.lock() {
            *slot = Some(exposed);
        }
        Ok(count)
    }

    /// The currently exposed tools, in the shape an MCP `tools/list` returns.
    pub fn exposed_tools(&self) -> Vec<ExposedTool> {
        self.catalog
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
            .unwrap_or_default()
    }

    /// Whether any tools are currently exposed.
    pub fn has_tools(&self) -> bool {
        !self.exposed_tools().is_empty()
    }

    /// The session a token addresses, for priming before a lease is read.
    pub fn session_for_token(&self, token: &str) -> Option<String> {
        self.leases
            .lock()
            .ok()
            .and_then(|leases| leases.get(token).map(|lease| lease.session_id.clone()))
    }

    /// Make sure the provider has advertised its tools, starting it if needed.
    ///
    /// This is the step that turns an installed provider into a usable tool
    /// list, and it is deliberately lazy. Starting the provider during app
    /// bring-up would put a Node process and an MCP handshake on the startup
    /// path for a capability most sessions never touch; doing it on the first
    /// `tools/list` means the cost is paid by the session that wants it and
    /// nobody else.
    ///
    /// It has to be lazy rather than eager for a second reason: the catalog is
    /// the only source of the exposed tool names, so any gate that asks for a
    /// non-empty catalog *before* a session is wired up deadlocks — nothing can
    /// call `tools/list` until the server is mounted, and the server reports no
    /// tools until it is asked. The injection decision therefore gates on
    /// preflight alone, which is the question that can be answered without
    /// starting anything.
    pub async fn ensure_catalog(&self, session_id: &str) -> Result<usize, BrowserCallError> {
        if self.has_tools() {
            return Ok(self.exposed_tools().len());
        }
        let _guard = self.prime.lock().await;
        // Whoever held the lock may have primed while we waited.
        if self.has_tools() {
            return Ok(self.exposed_tools().len());
        }

        let service = self.service.clone();
        let cancel = CancelToken::new();
        let catalog: ProviderCatalog = service
            .call(session_id, &cancel, |resource| async move {
                let client = resource.client().ok_or_else(|| {
                    browser_service::BrowserError::Launch(
                        "browser provider is not running for this session".to_string(),
                    )
                })?;
                client
                    .list_tools()
                    .await
                    .map_err(|error| browser_service::BrowserError::Provider {
                        tool: "tools/list".to_string(),
                        detail: error.to_string(),
                    })
            })
            .await
            .map_err(|error| classify("tools/list", &error.to_string()))?;

        self.install_catalog(&catalog)
            .map_err(|error| BrowserCallError::Provider {
                tool: "tools/list".to_string(),
                detail: error.to_string(),
            })
    }

    /// Route one tool call to the session's browser.
    pub async fn call(
        &self,
        token: &str,
        exposed_name: &str,
        arguments: serde_json::Value,
    ) -> Result<BrowserToolResult, BrowserCallError> {
        let (session_id, cancel) = {
            let leases = self
                .leases
                .lock()
                .map_err(|_| BrowserCallError::UnknownSession)?;
            let lease = leases.get(token).ok_or(BrowserCallError::UnknownSession)?;
            (lease.session_id.clone(), lease.cancel.clone())
        };

        let catalog = self.exposed_tools();
        let tool = catalog
            .iter()
            .find(|tool| tool.exposed_name == exposed_name)
            .ok_or_else(|| BrowserCallError::UnknownTool {
                name: exposed_name.to_string(),
            })?
            .clone();

        let service = self.service.clone();
        let provider_name = tool.provider_name.clone();
        let call_name = exposed_name.to_string();
        let error_name = call_name.clone();

        let raw: ToolCallResult = service
            .call(&session_id, &cancel, move |resource| async move {
                let client = resource.client().ok_or_else(|| {
                    browser_service::BrowserError::Launch(
                        "browser provider is not running for this session".to_string(),
                    )
                })?;
                client
                    .call_tool(&provider_name, arguments)
                    .await
                    .map_err(|error| match error {
                        browser_service::mcp::McpProtocolError::Timeout => {
                            browser_service::BrowserError::Timeout {
                                tool: error_name.clone(),
                                after_ms: 0,
                            }
                        }
                        other => browser_service::BrowserError::Provider {
                            tool: error_name.clone(),
                            detail: other.to_string(),
                        },
                    })
            })
            .await
            .map_err(|error| classify(&call_name, &error.to_string()))?;

        self.project(&session_id, raw)
    }

    /// Persist captures and decide how the model receives them.
    fn project(
        &self,
        session_id: &str,
        raw: ToolCallResult,
    ) -> Result<BrowserToolResult, BrowserCallError> {
        let mut screenshots = Vec::new();
        for bytes in &raw.images {
            let capture: Capture = self
                .pipeline
                .capture(session_id, bytes)
                .map_err(|error| BrowserCallError::Capture(error.to_string()))?;
            screenshots.push(capture.handle);
        }

        let delivery = screenshots.first().map(|handle| {
            let vision = self
                .model_vision
                .lock()
                .ok()
                .and_then(|map| map.get(session_id).copied())
                .unwrap_or(false);
            self.pipeline.deliver(session_id, handle, vision)
        });

        Ok(BrowserToolResult {
            text: raw.text,
            screenshots,
            delivery,
            is_error: raw.is_error,
        })
    }

    /// Clear a session's turn budget when its turn ends.
    pub fn end_turn(&self, token: &str) {
        if let Some(session_id) = self.session_for(token) {
            self.pipeline.end_turn(&session_id);
        }
    }

    fn session_for(&self, token: &str) -> Option<String> {
        self.leases
            .lock()
            .ok()
            .and_then(|leases| leases.get(token).map(|lease| lease.session_id.clone()))
    }
}

/// Turn a registry error back into the vocabulary the adapter reports.
fn classify(tool: &str, detail: &str) -> BrowserCallError {
    if detail.contains("timed out") {
        BrowserCallError::Timeout {
            tool: tool.to_string(),
        }
    } else if detail.contains("closing") || detail.contains("disposing") {
        BrowserCallError::Closed {
            tool: tool.to_string(),
        }
    } else {
        BrowserCallError::Provider {
            tool: tool.to_string(),
            detail: detail.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_service::catalog::ProviderTool;
    use browser_service::{BrowserFactory, BrowserService};
    use serde_json::json;
    use std::path::Path;
    use tempfile::TempDir;

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

    /// A service whose provider is not started, so routing can be tested
    /// without a browser.
    fn service() -> Arc<BrowserService> {
        Arc::new(BrowserService::from_factory(
            BrowserFactory::new(
                workspace_model::BrowserSettings {
                    enabled: true,
                    ..workspace_model::BrowserSettings::default()
                },
                std::path::PathBuf::from("/usr/bin/node"),
                std::path::PathBuf::from("/pkg/@playwright/mcp"),
                std::path::PathBuf::from("/data"),
            )
            .without_spawn(),
        ))
    }

    fn adapter(dir: &TempDir) -> BrowserMcpService {
        BrowserMcpService::new(
            service(),
            Arc::new(ScreenshotPipeline::new(dir.path().join("shots"))),
        )
    }

    #[test]
    fn registering_yields_a_token_and_a_lease() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);

        let token = adapter.register_session("session-1", true);
        assert!(token.starts_with("browser-"));
        assert_eq!(adapter.lease_count(), 1);
    }

    #[test]
    fn re_registering_a_session_replaces_its_previous_lease() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);

        adapter.register_session("session-1", true);
        adapter.register_session("session-1", true);
        assert_eq!(
            adapter.lease_count(),
            1,
            "a reconnect must not accumulate tokens",
        );
    }

    #[tokio::test]
    async fn unregistering_drops_the_lease_and_the_browser() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let token = adapter.register_session("session-1", true);

        adapter.unregister_session(&token).await;
        assert_eq!(adapter.lease_count(), 0);
        assert_eq!(
            adapter.service().registry().status("session-1"),
            session_resource::ResourceStatus::Closed
        );
    }

    #[test]
    fn the_exposed_surface_comes_from_the_installed_catalog() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        assert!(!adapter.has_tools());

        assert_eq!(adapter.install_catalog(&catalog()).unwrap(), 2);
        let tools = adapter.exposed_tools();
        assert_eq!(tools[0].exposed_name, "mcp__playwright-mcp__browser_click");
        assert!(tools[1].effect.is_read());
    }

    #[test]
    fn a_provider_upgrade_replaces_the_surface() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        adapter.install_catalog(&catalog()).unwrap();

        // A renamed tool is a new catalog, not a new Kodex release.
        let renamed = ProviderCatalog {
            tools: vec![ProviderTool {
                name: "browser_screenshot".to_string(),
                description: "Shot".to_string(),
                input_schema: json!({}),
            }],
        };
        adapter.install_catalog(&renamed).unwrap();

        let tools = adapter.exposed_tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(
            tools[0].exposed_name,
            "mcp__playwright-mcp__browser_screenshot"
        );
    }

    #[test]
    fn an_empty_catalog_is_rejected_and_leaves_the_surface_intact() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        adapter.install_catalog(&catalog()).unwrap();

        // A provider that failed to start advertises nothing. Stripping the
        // tools here would leave a running session unable to do anything.
        assert!(
            adapter
                .install_catalog(&ProviderCatalog { tools: vec![] })
                .is_err()
        );
        assert_eq!(adapter.exposed_tools().len(), 2);
    }

    #[tokio::test]
    async fn an_unknown_token_is_refused() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        adapter.install_catalog(&catalog()).unwrap();

        let result = adapter
            .call(
                "browser-999",
                "mcp__playwright-mcp__browser_click",
                json!({}),
            )
            .await;
        assert_eq!(result, Err(BrowserCallError::UnknownSession));
    }

    #[tokio::test]
    async fn a_tool_outside_the_catalog_is_refused() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        adapter.install_catalog(&catalog()).unwrap();
        let token = adapter.register_session("session-1", true);

        assert_eq!(
            adapter
                .call(&token, "mcp__playwright-mcp__browser_navigate", json!({}))
                .await,
            Err(BrowserCallError::UnknownTool {
                name: "mcp__playwright-mcp__browser_navigate".to_string()
            })
        );
    }

    #[tokio::test]
    async fn a_call_with_no_provider_running_reports_rather_than_hanging() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        adapter.install_catalog(&catalog()).unwrap();
        let token = adapter.register_session("session-1", true);

        // The factory is in no-spawn mode, so the call fails on the missing
        // provider rather than blocking.
        let result = adapter
            .call(&token, "mcp__playwright-mcp__browser_click", json!({}))
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn captures_are_persisted_and_referenced_not_inlined() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let token = adapter.register_session("session-1", false);

        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1280u32.to_be_bytes());
        png.extend_from_slice(&720u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        png.extend_from_slice(&[0xAA; 32]);

        let projected = adapter
            .project(
                "session-1",
                ToolCallResult {
                    text: "captured".to_string(),
                    images: vec![png],
                    is_error: false,
                },
            )
            .unwrap();

        assert_eq!(projected.screenshots.len(), 1);
        assert!(std::path::Path::new(&projected.screenshots[0].path).is_file());
        // The model-facing text names the file instead of carrying bytes.
        assert!(!projected.model_text().contains("base64"));
        assert!(projected.model_text().contains("1280x720"));
    }

    #[test]
    fn a_vision_capable_model_is_marked_for_inlining() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        adapter.register_session("session-1", true);
        adapter.register_session("session-2", false);

        let handle = ScreenshotHandle {
            path: "/tmp/x.png".to_string(),
            width: 8,
            height: 8,
            byte_size: 64,
            media_type: "image/png".to_string(),
        };

        let vision = adapter
            .project(
                "session-1",
                ToolCallResult {
                    text: String::new(),
                    images: vec![vec![0x89, b'P']],
                    is_error: false,
                },
            )
            .unwrap();
        assert!(vision.delivery.unwrap().inlineable());

        let no_vision = adapter
            .project(
                "session-2",
                ToolCallResult {
                    text: String::new(),
                    images: vec![vec![0x89, b'P']],
                    is_error: false,
                },
            )
            .unwrap();
        assert!(!no_vision.delivery.unwrap().inlineable());
        let _ = handle;
    }

    #[test]
    fn multiple_captures_share_one_delivery_decision() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        adapter.register_session("session-1", true);

        let png = {
            let mut bytes = vec![0x89, b'P', b'N', b'G'];
            bytes.extend_from_slice(&[0xAA; 16]);
            bytes
        };

        let result = adapter
            .project(
                "session-1",
                ToolCallResult {
                    text: String::new(),
                    images: vec![png.clone(), png],
                    is_error: false,
                },
            )
            .unwrap();

        assert_eq!(result.screenshots.len(), 2);
        // One delivery decision covers the call, not one per capture.
        assert!(result.delivery.is_some());
    }

    #[test]
    fn ending_a_turn_refills_the_screenshot_budget() {
        let dir = TempDir::new().unwrap();
        let pipeline =
            Arc::new(ScreenshotPipeline::new(dir.path().join("shots")).with_budget(4096, 1));
        let adapter = BrowserMcpService::new(service(), pipeline.clone());
        let token = adapter.register_session("session-1", true);

        let png = {
            let mut bytes = vec![0x89, b'P', b'N', b'G'];
            bytes.extend_from_slice(&[0xBB; 32]);
            bytes
        };

        let first = adapter
            .project(
                "session-1",
                ToolCallResult {
                    text: String::new(),
                    images: vec![png.clone()],
                    is_error: false,
                },
            )
            .unwrap();
        assert!(first.delivery.unwrap().inlineable());

        let second = adapter
            .project(
                "session-1",
                ToolCallResult {
                    text: String::new(),
                    images: vec![png],
                    is_error: false,
                },
            )
            .unwrap();
        assert!(!second.delivery.unwrap().inlineable(), "budget should bind");

        adapter.end_turn(&token);
        let third = adapter
            .project(
                "session-1",
                ToolCallResult {
                    text: String::new(),
                    images: vec![vec![0x89, b'P', b'N', b'G', 0xCC]],
                    is_error: false,
                },
            )
            .unwrap();
        assert!(third.delivery.unwrap().inlineable());
    }

    #[test]
    fn tool_results_without_images_carry_no_delivery_decision() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);

        let result = adapter
            .project(
                "session-1",
                ToolCallResult {
                    text: "clicked".to_string(),
                    images: vec![],
                    is_error: false,
                },
            )
            .unwrap();
        assert!(result.delivery.is_none());
        assert_eq!(result.model_text(), "clicked");
    }

    #[test]
    fn a_provider_error_flag_is_surfaced() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);

        let result = adapter
            .project(
                "session-1",
                ToolCallResult {
                    text: "selector not found".to_string(),
                    images: vec![],
                    is_error: true,
                },
            )
            .unwrap();
        assert!(result.is_error);
        assert_eq!(result.text, "selector not found");
    }

    #[test]
    fn registry_errors_are_classified_for_the_tool_card() {
        assert!(matches!(
            classify(
                "browser_click",
                "operation failed: browser tool browser_click timed out after 30000ms"
            ),
            BrowserCallError::Timeout { .. }
        ));
        assert!(matches!(
            classify("browser_click", "session resource is closing"),
            BrowserCallError::Closed { .. }
        ));
        assert!(matches!(
            classify("browser_click", "selector not found"),
            BrowserCallError::Provider { .. }
        ));
    }

    /// The real provider, the real handshake, the real catalog.
    ///
    /// Every other test in this file builds a `BrowserMcpService` with
    /// `.without_spawn()` and installs a catalog by hand, which is why the fact
    /// that *nothing in production ever installed a catalog* went unnoticed for
    /// so long: the one step that actually talks to the provider had no test.
    ///
    /// Run it under a PATH a packaged app would inherit:
    ///
    /// ```text
    /// env PATH=/usr/bin:/bin:/usr/sbin:/sbin:$HOME/.cargo/bin \
    ///     cargo test -p app-core --lib -- --ignored --nocapture real_provider_catalog
    /// ```
    #[test]
    #[ignore = "starts the real provider process and reads a real tools/list"]
    fn real_provider_catalog() {
        // Not `#[tokio::test]`: the spawn path expects a runtime the shared one
        // provides, and this runs outside the app.
        crate::shared_mcp::block_on(async {
        let paths = crate::AppPaths::resolve().expect("real data root");
        let settings = crate::settings::load_app_settings(&paths);
        let node = dsh_bridge::find_binary("node").expect("node");
        let package_root =
            crate::browser_preflight::provider_package_root(&paths, &settings.browser.provider_version);
        assert!(
            package_root.join("cli.js").is_file(),
            "the provider is not installed at {package_root:?}; run the install from Settings first"
        );

        let service = Arc::new(BrowserService::from_factory(BrowserFactory::new(
            settings.browser.clone(),
            node,
            package_root,
            paths.root().to_path_buf(),
        )));
        let adapter = Arc::new(BrowserMcpService::new(
            service,
            Arc::new(crate::screenshot_pipeline::ScreenshotPipeline::new(
                paths.attachments_dir().join("screenshots"),
            )),
        ));

        let count = adapter
            .ensure_catalog("real-catalog-probe")
            .await
            .expect("prime the catalog from the real provider");
        let names: Vec<String> = adapter
            .exposed_tools()
            .into_iter()
            .map(|tool| tool.exposed_name)
            .collect();
        eprintln!("provider advertised {count} tools: {names:?}");

        assert!(count > 0, "the real provider advertised nothing");
        assert!(
            names
                .iter()
                .all(|name| name.starts_with("mcp__playwright-mcp__")),
            "tools must be namespaced: {names:?}"
        );

        // A second prime must not spawn a second provider.
        let again = adapter
            .ensure_catalog("real-catalog-probe-2")
            .await
            .expect("re-prime is a no-op");
        assert_eq!(again, count);

        adapter
            .service()
            .close_session("real-catalog-probe")
            .await
            .ok();
        })
        .expect("the real provider yields a catalog");
    }

    /// Navigate a real browser and take a real screenshot.
    ///
    /// The claim this whole feature rests on, executed: a provider process is
    /// started, a tool is dispatched by its exposed name, Chromium launches, and
    /// the capture comes back as a handle. Everything above it — the catalog,
    /// the injection decision, the panel, the tool card — is only correct if
    /// this line works, and no amount of unit testing substitutes for it.
    #[test]
    #[ignore = "launches a real Chromium and writes a real screenshot"]
    fn real_browser_navigates_and_screenshots() {
        crate::shared_mcp::block_on(async {
            let paths = crate::AppPaths::resolve().expect("real data root");
            let settings = crate::settings::load_app_settings(&paths);
            let node = dsh_bridge::find_binary("node").expect("node");
            let package_root = crate::browser_preflight::provider_package_root(
                &paths,
                &settings.browser.provider_version,
            );
            let service = Arc::new(BrowserService::from_factory(BrowserFactory::new(
                settings.browser.clone(),
                node,
                package_root,
                paths.root().to_path_buf(),
            )));
            let adapter = Arc::new(BrowserMcpService::new(
                service,
                Arc::new(crate::screenshot_pipeline::ScreenshotPipeline::new(
                    paths.attachments_dir().join("screenshots"),
                )),
            ));

            let token = adapter.register_session("real-drive", true);
            adapter
                .ensure_catalog("real-drive")
                .await
                .expect("prime the catalog");

            let navigate = adapter
                .call(
                    &token,
                    "mcp__playwright-mcp__browser_navigate",
                    json!({ "url": "https://example.com/" }),
                )
                .await
                .expect("navigate a real browser");
            eprintln!(
                "navigate: is_error={} text={}",
                navigate.is_error,
                navigate.model_text()
            );
            assert!(
                !navigate.is_error,
                "navigate failed: {}",
                navigate.model_text()
            );

            let shot = adapter
                .call(
                    &token,
                    "mcp__playwright-mcp__browser_take_screenshot",
                    json!({}),
                )
                .await
                .expect("screenshot a real browser");
            eprintln!(
                "screenshot: is_error={} captures={}",
                shot.is_error,
                shot.screenshots.len()
            );
            assert!(!shot.is_error, "screenshot failed");
            let capture = shot.screenshots.first().expect("a capture");
            assert!(capture.width > 0 && capture.height > 0, "{capture:?}");
            assert!(
                Path::new(&capture.path).is_file(),
                "no image at {}",
                capture.path
            );
            eprintln!(
                "image on disk: {} ({} bytes)",
                capture.path, capture.byte_size
            );

            adapter.unregister_session(&token).await;
        })
        .expect("a real browser answers a real tool call");
    }
}
