//! Panel-driven browser control.
//!
//! The agent and the user share one browser per session, so a manual
//! navigation from the panel must go through the same registry and the same
//! per-session operation lock as an agent tool call. Routing it any other way
//! would let a user click land halfway through an agent navigation.
//!
//! Every call here is addressed by session id, not by token: the panel holds no
//! token, and the reverse index on the server maps the id to the live one.

use serde_json::json;
use std::sync::Arc;

use workspace_model::BrowserSessionState;

/// Which provider tool implements each panel action.
///
/// The names are the ones Playwright MCP advertises, and they are resolved
/// against the discovered catalog at call time — a provider that renames a
/// tool produces a clear "not available" rather than a silent no-op.
const NAVIGATE_TOOL: &str = "mcp__playwright-mcp__browser_navigate";
const REFRESH_TOOL: &str = "mcp__playwright-mcp__browser_navigate";
const SCREENSHOT_TOOL: &str = "mcp__playwright-mcp__browser_take_screenshot";

/// Result of a panel action, enough to decide whether to push a state event.
pub struct PanelOutcome {
    pub changed: bool,
    pub detail: Option<String>,
}

/// A handle to the shared browser server for panel actions.
pub struct PanelBrowser {
    service: Arc<crate::browser_server::BrowserServerService>,
}

impl PanelBrowser {
    pub fn new(service: Arc<crate::browser_server::BrowserServerService>) -> Self {
        Self { service }
    }

    /// Navigate the session's browser.
    pub async fn navigate(&self, session_id: &str, url: &str) -> Result<PanelOutcome, String> {
        let target = normalize_url(url)?;
        self.call(session_id, NAVIGATE_TOOL, json!({ "url": target }))
            .await
    }

    /// Re-capture the current page so the panel has something to show.
    pub async fn refresh(&self, session_id: &str) -> Result<PanelOutcome, String> {
        self.call(session_id, SCREENSHOT_TOOL, json!({})).await
    }

    /// Dispose the session's browser. A later agent tool call starts a fresh
    /// one, so closing from the panel is a reset rather than a disable.
    pub async fn close(&self, session_id: &str) -> Result<PanelOutcome, String> {
        if self.service.token_for_session(session_id).is_none() {
            return Err("This session has no browser registered.".to_string());
        }
        self.service.drop_session(session_id);
        Ok(PanelOutcome {
            changed: true,
            detail: None,
        })
    }

    async fn call(
        &self,
        session_id: &str,
        tool: &str,
        arguments: serde_json::Value,
    ) -> Result<PanelOutcome, String> {
        let result = self
            .service
            .call_for_session(session_id, tool, arguments)
            .await
            .map_err(|error| error.to_string())?;

        Ok(PanelOutcome {
            changed: result.is_error,
            detail: (!result.text.is_empty()).then_some(result.text),
        })
    }

    /// Build the state the panel should render for a session.
    pub fn state_for(&self, session_id: &str) -> Option<BrowserSessionState> {
        let adapter = self.service.adapter_handle()?;
        if self.service.token_for_session(session_id).is_none() {
            return None;
        }
        let status = adapter.service().registry().status(session_id);
        Some(BrowserSessionState {
            session_id: session_id.to_string(),
            status: match status {
                session_resource::ResourceStatus::Idle => {
                    workspace_model::CapabilityResourceStatus::Idle
                }
                session_resource::ResourceStatus::Active => {
                    workspace_model::CapabilityResourceStatus::Active
                }
                session_resource::ResourceStatus::Closing => {
                    workspace_model::CapabilityResourceStatus::Closing
                }
                session_resource::ResourceStatus::Closed => {
                    workspace_model::CapabilityResourceStatus::Closed
                }
                session_resource::ResourceStatus::Failed => {
                    workspace_model::CapabilityResourceStatus::Failed
                }
            },
            ..BrowserSessionState::default()
        })
    }
}

/// Accept what a person would type and turn it into something a browser can
/// open.
///
/// A bare `example.com` is what people actually type, and sending it to a
/// browser as-is would be treated as a relative path rather than a site.
pub fn normalize_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Enter a URL to open.".to_string());
    }

    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };

    let url = url::Url::parse(&with_scheme)
        .map_err(|error| format!("That does not look like a URL: {error}"))?;
    match url.scheme() {
        "http" | "https" => Ok(url.to_string()),
        other => Err(format!(
            "Only http and https URLs can be opened, not {other}."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_host_gets_https() {
        assert_eq!(
            normalize_url("example.com").unwrap(),
            "https://example.com/"
        );
    }

    #[test]
    fn an_explicit_scheme_is_preserved() {
        assert_eq!(
            normalize_url("http://127.0.0.1:3000").unwrap(),
            "http://127.0.0.1:3000/"
        );
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        assert_eq!(
            normalize_url("  example.com/docs  ").unwrap(),
            "https://example.com/docs"
        );
    }

    #[test]
    fn a_deep_path_survives() {
        assert_eq!(
            normalize_url("example.com/a/b?c=d").unwrap(),
            "https://example.com/a/b?c=d"
        );
    }

    #[test]
    fn an_empty_url_is_rejected_with_a_message() {
        let error = normalize_url("   ").unwrap_err();
        assert!(error.contains("Enter a URL"), "got {error}");
    }

    #[test]
    fn a_non_http_scheme_is_refused() {
        // The panel drives a browser; handing it `file://` or `javascript:`
        // would read local state the agent is not otherwise scoped to.
        let error = normalize_url("file:///etc/passwd").unwrap_err();
        assert!(error.contains("Only http and https"), "got {error}");

        let error = normalize_url("javascript://alert(1)").unwrap_err();
        assert!(error.contains("Only http and https"), "got {error}");
    }

    #[test]
    fn a_malformed_host_is_rejected() {
        assert!(normalize_url("http://").is_err());
    }
}
