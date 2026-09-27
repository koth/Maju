//! Process-wide local MCP servers shared by every session.
//!
//! Before this module each session started its own pair of localhost MCP
//! servers — one for the configured web-tools provider, one for the image
//! capability server — and the DeepSeek Harness started a third pair for its
//! own process. Every one of them cost a thread plus a tokio runtime, and the
//! image server's view cache was per instance, so the same picture was
//! described once per session.
//!
//! Now there is exactly one `kodex-web-tools` and one `kodex-image` server per
//! process. What stays per session is the *registration* behind its token:
//!
//! * web tools — that session's provider client (so a settings change applies
//!   to the sessions that start after it, and no session can address another's
//!   credentials);
//! * image — that session's [`ImageCapabilities`] (so `tools/list` stays
//!   trimmed per model) and its [`crate::image_mcp::ImageMcpConfig`] (so
//!   generated images still land in that session's workspace).
//!
//! The servers start lazily on first use and live for the process; the
//! per-session leases unregister when the session ends.

use crate::AppPaths;
use crate::image_mcp::{ImageMcpConfig, ImageMcpHandle, ImageMcpLease};
use crate::web_tools::WebToolsConfig;
use crate::web_tools_mcp::{WebToolsLease, WebToolsMcpHandle};
use anyhow::anyhow;
use std::sync::{Arc, Mutex, OnceLock};
use workspace_model::{BrowserSettings, ImageCapabilities};

static SHARED: OnceLock<Arc<SharedMcpServers>> = OnceLock::new();

/// The process-wide local MCP servers.
/// A `SharedMcpServers` preloaded with a browser adapter, for tests that need
/// a browser server without touching the real settings or the real host.
#[cfg(test)]
pub fn shared_mcp_for_test(
    adapter: Arc<crate::browser_mcp::BrowserMcpService>,
) -> Arc<SharedMcpServers> {
    let service = Arc::new(crate::browser_server::BrowserServerService::with_adapter(
        adapter.clone(),
    ));
    let pipeline = adapter.pipeline().clone();
    let handle = Arc::new(
        crate::browser_server::start_browser_mcp_server(adapter.clone())
            .expect("test browser server starts"),
    );
    Arc::new(SharedMcpServers {
        browser: Mutex::new(Some(SharedBrowserServer {
            handle,
            adapter,
            service,
            pipeline,
        })),
        ..Default::default()
    })
}

/// Run a future to completion on a shared current-thread runtime.
///
/// The managed MCP servers own their own runtime threads, so a Tauri command
/// that has to await one of them needs a runtime too, and building a fresh one
/// per call would be wasteful.
pub fn block_on<F: std::future::Future<Output = T>, T>(future: F) -> Result<T, String> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    let runtime = RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)?;
    Ok(runtime.block_on(future))
}

/// Like [`block_on`], for a future that already produces a result.
///
/// [`block_on`] wraps whatever the future returns, so awaiting a future that
/// yields `Result<T, E>` through it gives `Result<Result<T, E>, E>`. A single
/// `?` at the call site then propagates the *outer* error and silently drops
/// the inner one — which is the error the caller actually wanted, so the
/// command reports success for a browser call that failed. Flattening here
/// makes the call sites read the way they mean.
pub fn block_on_result<F, T>(future: F) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, String>>,
{
    block_on(future)?
}

pub fn shared_mcp() -> Arc<SharedMcpServers> {
    SHARED
        .get_or_init(|| Arc::new(SharedMcpServers::default()))
        .clone()
}

#[derive(Default)]
pub struct SharedMcpServers {
    web_tools: Mutex<Option<Arc<WebToolsMcpHandle>>>,
    image: Mutex<Option<Arc<ImageMcpHandle>>>,
    browser: Mutex<Option<SharedBrowserServer>>,
}

impl SharedMcpServers {
    /// The shared web-tools server, started on first use.
    pub fn web_tools(&self) -> anyhow::Result<Arc<WebToolsMcpHandle>> {
        let mut slot = self
            .web_tools
            .lock()
            .map_err(|_| anyhow!("web tools MCP lock poisoned"))?;
        if let Some(handle) = slot.as_ref() {
            return Ok(handle.clone());
        }
        let handle = Arc::new(crate::web_tools_mcp::start_web_tools_mcp_server()?);
        *slot = Some(handle.clone());
        Ok(handle)
    }

    /// The shared image server, started on first use.
    pub fn image(&self) -> anyhow::Result<Arc<ImageMcpHandle>> {
        let mut slot = self
            .image
            .lock()
            .map_err(|_| anyhow!("image MCP lock poisoned"))?;
        if let Some(handle) = slot.as_ref() {
            return Ok(handle.clone());
        }
        let handle = Arc::new(crate::image_mcp::start_image_mcp_server()?);
        *slot = Some(handle.clone());
        Ok(handle)
    }

    /// Register one session's web-tools provider config on the shared server.
    pub fn web_tools_lease(&self, config: WebToolsConfig) -> anyhow::Result<WebToolsLease> {
        WebToolsLease::register(self.web_tools()?, config)
    }

    /// Register one session's image capabilities and config on the shared
    /// server.
    pub fn image_lease(
        &self,
        caps: ImageCapabilities,
        config: ImageMcpConfig,
    ) -> anyhow::Result<ImageMcpLease> {
        Ok(ImageMcpLease::register(self.image()?, caps, config))
    }

    /// The shared browser server if it was already started.
    ///
    /// Teardown uses this rather than `browser_server`, because disposal must
    /// not *create* a server just to close it.
    pub fn browser_existing(&self) -> Option<SharedBrowserServer> {
        let slot = self.browser.lock().ok()?;
        slot.as_ref().map(|existing| SharedBrowserServer {
            handle: existing.handle.clone(),
            adapter: existing.adapter.clone(),
            service: existing.service.clone(),
            pipeline: existing.pipeline.clone(),
        })
    }

    /// Session ids the browser adapter currently knows about.
    pub fn active_sessions(&self) -> Vec<String> {
        let Some(browser) = self.browser_existing() else {
            return Vec::new();
        };
        browser.service.active_sessions()
    }

    /// The shared browser server, started on first use.
    ///
    /// The adapter is built around a browser service resolved from the current
    /// settings. It is created once per process so tool names stay stable
    /// across sessions, while each session still gets its own browser behind
    /// its own token.
    ///
    /// Settings are passed in rather than resolved here because the shared
    /// servers are process-wide singletons with no view of `AppPaths`.
    pub fn browser_server(
        &self,
        app_paths: &AppPaths,
        settings: &BrowserSettings,
    ) -> anyhow::Result<SharedBrowserServer> {
        let mut slot = self
            .browser
            .lock()
            .map_err(|_| anyhow!("browser MCP lock poisoned"))?;
        if let Some(existing) = slot.as_ref() {
            return Ok(SharedBrowserServer {
                handle: existing.handle.clone(),
                adapter: existing.adapter.clone(),
                service: existing.service.clone(),
                pipeline: existing.pipeline.clone(),
            });
        }

        let preflight = crate::browser_preflight::check_browser(
            settings,
            &crate::browser_preflight::HostEnvironment,
        );
        let service = Arc::new(browser_service::BrowserService::new(
            settings.clone(),
            preflight
                .node_executable
                .clone()
                .unwrap_or_else(|| std::path::PathBuf::from("node")),
            browser_provider_package_root(app_paths, settings),
            app_paths.root().to_path_buf(),
        ));
        let pipeline =
            crate::screenshot_pipeline::shared(app_paths.attachments_dir().join("screenshots"));
        let adapter = Arc::new(crate::browser_mcp::BrowserMcpService::new(
            service,
            pipeline.clone(),
        ));
        let handle = Arc::new(crate::browser_server::start_browser_mcp_server(
            adapter.clone(),
        )?);

        let service = Arc::new(crate::browser_server::BrowserServerService::with_adapter(
            adapter.clone(),
        ));
        *slot = Some(SharedBrowserServer {
            handle: handle.clone(),
            adapter: adapter.clone(),
            service: service.clone(),
            pipeline: pipeline.clone(),
        });
        Ok(SharedBrowserServer {
            handle,
            adapter,
            service,
            pipeline,
        })
    }
}

/// The shared browser server and the adapter behind it.
pub struct SharedBrowserServer {
    handle: Arc<crate::browser_server::BrowserServerHandle>,
    adapter: Arc<crate::browser_mcp::BrowserMcpService>,
    service: Arc<crate::browser_server::BrowserServerService>,
    pipeline: Arc<crate::screenshot_pipeline::ScreenshotPipeline>,
}

impl SharedBrowserServer {
    pub fn handle(&self) -> Arc<crate::browser_server::BrowserServerHandle> {
        self.handle.clone()
    }

    pub fn adapter(&self) -> Arc<crate::browser_mcp::BrowserMcpService> {
        self.adapter.clone()
    }

    /// The per-token view, for panel actions addressed by session id.
    pub fn service(&self) -> Arc<crate::browser_server::BrowserServerService> {
        self.service.clone()
    }

    /// The pipeline production captures land in, for retention.
    pub fn pipeline(&self) -> Arc<crate::screenshot_pipeline::ScreenshotPipeline> {
        self.pipeline.clone()
    }

    /// Dispose one session's browser, addressed by session id.
    pub async fn dispose_session(&self, session_id: &str) {
        self.service.drop_session(session_id);
        let _ = self.adapter.service().close_session(session_id).await;
    }
}

/// Where the pinned provider package is installed for the current user.
///
/// The provider lives under Kodex's own data root rather than a global npm
/// prefix, so the version pinned in settings is the version that runs and a
/// user's global install cannot change it.
fn browser_provider_package_root(
    app_paths: &AppPaths,
    settings: &BrowserSettings,
) -> std::path::PathBuf {
    // Shared with preflight so the check and the spawn can never disagree
    // about where the provider lives.
    crate::browser_preflight::provider_package_root(app_paths, &settings.provider_version)
}

#[cfg(test)]
mod block_on_tests {
    use super::{block_on, block_on_result};

    /// The whole point of `block_on_result`: an error produced *inside* the
    /// future survives the call. Through `block_on` it would be the inner of
    /// two `Result`s, and a single `?` would propagate the outer one and drop
    /// it — a browser command reporting success for a call that failed.
    #[test]
    fn an_inner_error_is_propagated_rather_than_swallowed() {
        let failed: Result<(), String> =
            block_on_result(async { Err("no browser registered".to_string()) });
        assert_eq!(failed.unwrap_err(), "no browser registered");
    }

    #[test]
    fn a_successful_value_comes_back_intact() {
        let value: Result<u32, String> = block_on_result(async { Ok(7) });
        assert_eq!(value.unwrap(), 7);
    }

    #[test]
    fn the_plain_helper_still_returns_whatever_the_future_produced() {
        // `block_on` keeps its general shape; only the result-returning call
        // sites need the flattening one.
        let wrapped: Result<Result<u32, String>, String> = block_on(async { Ok(3) });
        assert_eq!(wrapped.unwrap().unwrap(), 3);
    }
}
