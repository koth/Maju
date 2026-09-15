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

use crate::image_mcp::{ImageMcpConfig, ImageMcpHandle, ImageMcpLease};
use crate::web_tools::WebToolsConfig;
use crate::web_tools_mcp::{WebToolsLease, WebToolsMcpHandle};
use anyhow::anyhow;
use std::sync::{Arc, Mutex, OnceLock};
use workspace_model::ImageCapabilities;

static SHARED: OnceLock<Arc<SharedMcpServers>> = OnceLock::new();

/// The process-wide local MCP servers.
pub fn shared_mcp() -> Arc<SharedMcpServers> {
    SHARED
        .get_or_init(|| Arc::new(SharedMcpServers::default()))
        .clone()
}

#[derive(Default)]
pub struct SharedMcpServers {
    web_tools: Mutex<Option<Arc<WebToolsMcpHandle>>>,
    image: Mutex<Option<Arc<ImageMcpHandle>>>,
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
        let mut slot = self.image.lock().map_err(|_| anyhow!("image MCP lock poisoned"))?;
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
}
