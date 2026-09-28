//! Access to the CDP endpoint of a session's browser, for the right-panel
//! browser view (see `docs/browser-view-subsystem.md`).
//!
//! The view is a second client of the same browser the agent's MCP tools
//! drive, so it must resolve the endpoint the provider connects to — the
//! managed browser's websocket URL in launch/persistent mode, the user's
//! endpoint in attach mode — and it must arrive at a *live* browser even when
//! the agent has not called a tool yet. Going through the session registry's
//! `run` gives both: it acquires the provider (and thus the browser) on first
//! use and hands back the resource the endpoint is recorded on.

use std::future::ready;

pub use browser_service::view::{
    BrowserView, BrowserViewSink, PageTarget, ViewInputEvent, ViewStatus,
};

use crate::browser_server::BrowserServerService;

/// Ensure the session's browser exists and return its CDP endpoint.
///
/// Errors when the capability is not wired up (no adapter), when the session
/// is closing, or when the browser was configured without a reachable
/// endpoint.
pub async fn cdp_endpoint_for(
    service: &BrowserServerService,
    session_id: &str,
) -> Result<String, String> {
    let adapter = service
        .adapter_handle()
        .ok_or_else(|| "browser tools are not available".to_string())?;
    let browser_service = adapter.service().clone();
    let registry = browser_service.registry();
    let cancel = session_resource::CancelToken::new();
    registry
        .run(session_id, &cancel, |resource| {
            ready(match resource.cdp_endpoint() {
                Some(endpoint) => Ok(endpoint.to_string()),
                None => Err(browser_service::BrowserError::Launch(
                    "the browser has no CDP endpoint".to_string(),
                )),
            })
        })
        .await
        .map_err(|error| error.to_string())
}
