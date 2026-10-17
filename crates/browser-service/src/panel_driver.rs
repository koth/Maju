//! The panel's browser, driven natively by the agent's tools.
//!
//! On Windows the panel's webview speaks CDP, so the tools drive it through the
//! published DevTools endpoint ([`crate::panel`]). On macOS the webview is a
//! WKWebView, which has no CDP endpoint at all — so there the desktop shell
//! publishes a [`PanelDriver`] instead, and the app serves the agent's browser
//! tools by driving the panel's webview directly: navigating it, and running
//! JavaScript in the very page the user is looking at.
//!
//! The shape mirrors [`crate::panel`]: a process-wide slot, because the panel
//! is process-wide — one set of tabs per app run. The desktop shell is the
//! only publisher: it owns the webviews, so it is the only layer that can
//! drive them. A platform that never publishes (Windows, today) simply keeps
//! the provider path: `get` returns `None` and nothing here runs.

use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

/// One panel tab, as the driver reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelTabInfo {
    pub tab_id: String,
    pub url: String,
    pub title: String,
}

/// How the app drives the panel's own browser for the agent's tools.
///
/// Every method is `async` because everything here crosses into the GUI layer:
/// evaluating JavaScript dispatches to the webview and waits for the page to
/// answer, and opening a tab creates a native webview on the main thread.
#[async_trait::async_trait]
pub trait PanelDriver: Send + Sync {
    /// Run JavaScript in a tab's page and return the JSON value the page sent
    /// back, waiting at most `timeout` for the answer. The script reports its
    /// own outcome (the app wraps it so a thrown error arrives as an `Err`
    /// here rather than as silence). An `Err` is one of two kinds, and the
    /// caller has to be able to tell them apart:
    ///
    /// - **the page's own answer** — the script ran and reported a failure.
    ///   Authoritative: retrying repeats the failure.
    /// - **a transport failure** (prefixed with [`TRANSPORT_ERROR_PREFIX`]) —
    ///   the tab is mid-navigation and dropped the script, the page is too
    ///   busy to answer in time, or the answer was lost with the document that
    ///   produced it. The outcome is *unknown*: the action may have happened.
    ///   Read-only calls should settle and retry; actions should degrade to a
    ///   fresh snapshot instead of pretending certainty.
    async fn eval(&self, tab_id: &str, script: &str, timeout: Duration) -> Result<serde_json::Value, String>;

    /// Send a tab to a URL, returning once the page has finished loading — or
    /// once it has had long enough that the caller should read it as it is.
    async fn navigate(&self, tab_id: &str, url: &str) -> Result<(), String>;

    /// Wait until the tab's page has finished whatever load it is in, if any.
    ///
    /// Actions like a click can start a navigation after the call that caused
    /// them returns; callers that answer with a fresh snapshot use this so the
    /// snapshot describes the page the action landed on, not the one it left.
    async fn wait_settled(&self, tab_id: &str) -> Result<(), String>;

    /// Open a URL as a new tab and make it the current one. Returns the tab id.
    async fn open_tab(&self, url: &str) -> Result<String, String>;

    /// Close a tab.
    async fn close_tab(&self, tab_id: &str) -> Result<(), String>;

    /// Make a tab the current one: the panel shows it, and the tools act on it.
    async fn activate_tab(&self, tab_id: &str) -> Result<(), String>;

    /// Every tab, in strip order.
    async fn tabs(&self) -> Vec<PanelTabInfo>;

    /// The tab the tools act on, when one exists.
    async fn active_tab(&self) -> Option<String>;
}

/// The process-wide slot the desktop shell publishes into.
fn slot() -> &'static RwLock<Option<Arc<dyn PanelDriver>>> {
    static SLOT: OnceLock<RwLock<Option<Arc<dyn PanelDriver>>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// The prefix marking an `eval` failure raised by the driving machinery
/// itself — the page never answered, the answer was lost to a navigation, or
/// the script could not be injected — as opposed to a failure the *page*
/// reported, which is authoritative and stays unprefixed.
///
/// The distinction is what makes a failure actionable: a transport failure
/// means "the outcome is unknown", so read-only calls may settle and retry
/// and actions may degrade to a fresh snapshot; a page-reported failure means
/// "the action failed", and retrying just repeats it.
pub const TRANSPORT_ERROR_PREFIX: &str = "[transport] ";

/// Build a transport error message for [`PanelDriver`] implementations.
pub fn transport_error(detail: impl AsRef<str>) -> String {
    format!("{TRANSPORT_ERROR_PREFIX}{}", detail.as_ref())
}

/// Whether an `eval` failure came from the driving machinery rather than the
/// page.
pub fn is_transport_error(message: &str) -> bool {
    message.starts_with(TRANSPORT_ERROR_PREFIX)
}

/// Publish the panel driver for the whole process.
///
/// The desktop shell calls this once at startup. A driver that exists means
/// the agent's browser tools drive the panel natively — the provider path
/// (spawning `@playwright/mcp` and a browser for it) is bypassed entirely.
pub fn publish(driver: Arc<dyn PanelDriver>) {
    if let Ok(mut guard) = slot().write() {
        *guard = Some(driver);
    }
}

/// The published driver, when there is one.
pub fn get() -> Option<Arc<dyn PanelDriver>> {
    slot().read().ok().and_then(|guard| guard.clone())
}

/// Forget the driver: the panel is gone, the tools are back on the provider.
pub fn clear() {
    if let Ok(mut guard) = slot().write() {
        *guard = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoopDriver;

    #[async_trait::async_trait]
    impl PanelDriver for NoopDriver {
        async fn eval(
            &self,
            _tab_id: &str,
            _script: &str,
            _timeout: Duration,
        ) -> Result<serde_json::Value, String> {
            Ok(serde_json::Value::Null)
        }
        async fn navigate(&self, _tab_id: &str, _url: &str) -> Result<(), String> {
            Ok(())
        }
        async fn wait_settled(&self, _tab_id: &str) -> Result<(), String> {
            Ok(())
        }
        async fn open_tab(&self, _url: &str) -> Result<String, String> {
            Ok("tab-1".to_string())
        }
        async fn close_tab(&self, _tab_id: &str) -> Result<(), String> {
            Ok(())
        }
        async fn activate_tab(&self, _tab_id: &str) -> Result<(), String> {
            Ok(())
        }
        async fn tabs(&self) -> Vec<PanelTabInfo> {
            Vec::new()
        }
        async fn active_tab(&self) -> Option<String> {
            None
        }
    }

    #[test]
    fn the_slot_holds_exactly_one_driver() {
        clear();
        assert!(get().is_none());
        publish(Arc::new(NoopDriver));
        assert!(get().is_some());
        // Publishing again replaces, it does not stack.
        publish(Arc::new(NoopDriver));
        clear();
        assert!(get().is_none());
    }

    #[test]
    fn transport_errors_are_marked_and_page_errors_are_not() {
        let error = transport_error("页面没有响应");
        assert!(is_transport_error(&error));
        assert!(error.contains("页面没有响应"));
        // A page-reported failure — the script ran and said so — is
        // authoritative and must not look retryable.
        assert!(!is_transport_error("元素 e3 不存在或已过期"));
    }
}
