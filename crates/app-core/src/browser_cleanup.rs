//! Releasing browser resources when sessions end.
//!
//! A `BrowserServerLease`'s `Drop` only removes the token from the server's
//! table. The provider process it stands for lives in the adapter's registry,
//! and releasing that is asynchronous. Relying on `Drop` alone would leave a
//! Chromium running for every session that ever touched browser tools, so
//! teardown has to be explicit.
//!
//! Disposal is idempotent and best-effort: a browser that is already gone, or
//! a session that never had one, is not an error, because this runs on paths
//! that are already unwinding.

use std::sync::Arc;

use crate::shared_mcp::SharedMcpServers;

/// Dispose one session's browser and drop its registration.
pub async fn dispose_session(servers: &SharedMcpServers, session_id: &str) {
    let browser = match servers.browser_existing() {
        Some(browser) => browser,
        // No browser server was ever started, so nothing can be holding a
        // browser.
        None => return,
    };
    browser.dispose_session(session_id).await;
}

/// Dispose every browser the adapter knows about.
///
/// Used on application shutdown, where no session id is available but every
/// session's browser must go.
pub async fn dispose_all(servers: &SharedMcpServers) {
    let Some(browser) = servers.browser_existing() else {
        return;
    };
    // Enumerate from the registry, not the server's token table: the registry
    // owns the browser lifetimes, and a session can hold a resource even if
    // whatever started it has already let go of its handle.
    for session_id in browser.adapter().service().registry().session_ids() {
        browser.dispose_session(&session_id).await;
    }
}

/// Synchronous wrapper for `Drop` and other non-async contexts.
pub fn dispose_all_blocking(servers: Arc<SharedMcpServers>) {
    let _ = crate::shared_mcp::block_on(dispose_all(&servers));
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_service::catalog::ProviderCatalog;
    use browser_service::{BrowserFactory, BrowserService};
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn adapter(dir: &TempDir) -> Arc<crate::browser_mcp::BrowserMcpService> {
        Arc::new(crate::browser_mcp::BrowserMcpService::new(
            Arc::new(BrowserService::from_factory(
                BrowserFactory::new(
                    workspace_model::BrowserSettings {
                        enabled: true,
                        ..workspace_model::BrowserSettings::default()
                    },
                    PathBuf::from("/usr/bin/node"),
                    PathBuf::from("/pkg/@playwright/mcp"),
                    dir.path().to_path_buf(),
                )
                .without_spawn(),
            )),
            Arc::new(crate::screenshot_pipeline::ScreenshotPipeline::new(
                dir.path().join("shots"),
            )),
        ))
    }

    #[tokio::test]
    async fn disposing_an_unknown_session_is_a_no_op() {
        let dir = TempDir::new().unwrap();
        let servers = crate::shared_mcp::shared_mcp_for_test(adapter(&dir));

        // Nothing was ever registered, so this must not panic or error.
        dispose_session(&servers, "never-existed").await;
    }

    #[tokio::test]
    async fn dispose_all_closes_every_browser_that_was_acquired() {
        let dir = TempDir::new().unwrap();
        let servers = crate::shared_mcp::shared_mcp_for_test(adapter(&dir));
        let browser = servers.browser_existing().expect("browser server");

        // A catalog so the tool name resolves and the call gets as far as the
        // provider.
        browser
            .adapter()
            .install_catalog(&ProviderCatalog {
                tools: vec![browser_service::catalog::ProviderTool {
                    name: "browser_click".to_string(),
                    description: String::new(),
                    input_schema: serde_json::json!({}),
                }],
            })
            .expect("catalog installs");

        // Register, then drive a real tool call so the registry actually
        // acquires a browser. The call itself fails (no provider is running),
        // but acquisition happens first — and that acquired resource is
        // exactly the state shutdown has to clean up.
        for id in ["session-1", "session-2"] {
            let token = browser.adapter().register_session(id, true);
            let _ = browser
                .adapter()
                .call(
                    &token,
                    "mcp__playwright-mcp__browser_click",
                    serde_json::json!({}),
                )
                .await;
            assert_eq!(
                browser.adapter().service().registry().status(id),
                session_resource::ResourceStatus::Active,
                "{id} should hold a browser before shutdown",
            );
        }

        dispose_all(&servers).await;

        for id in ["session-1", "session-2"] {
            assert_eq!(
                browser.adapter().service().registry().status(id),
                session_resource::ResourceStatus::Closed,
                "{id} kept its browser after shutdown",
            );
        }
    }

    #[tokio::test]
    async fn disposing_twice_is_harmless() {
        let dir = TempDir::new().unwrap();
        let servers = crate::shared_mcp::shared_mcp_for_test(adapter(&dir));
        let browser = servers.browser_existing().expect("browser server");
        let _ = browser.adapter().register_session("session-1", true);

        dispose_session(&servers, "session-1").await;
        // Teardown can run on both the switch path and the drop path.
        dispose_session(&servers, "session-1").await;
    }
}
