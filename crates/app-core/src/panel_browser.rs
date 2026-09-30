//! The panel browser, as the app publishes it to the agent's browser tools.
//!
//! Maju's right panel is a real browser (`apps/desktop/src-tauri/src/
//! browser_panel.rs`), not a picture of one, and it is the browser the tools
//! drive: the page the user reads and the page the agent acts on are then the
//! same page. The panel publishes its DevTools endpoint here as soon as its
//! browser process exists, which is what makes that true — see
//! [`browser_service::panel`] for the slot and
//! [`browser_service::BrowserFactory`] for the decision, where a running panel
//! browser wins over every choice in settings.
//!
//! The desktop shell is the only publisher: it owns the webviews, so it is the
//! only layer that knows whether the panel's browser is up.

pub use browser_service::panel::{clear, endpoint, publish};

use serde_json::Value;
use workspace_model::BrowserSettings;

/// Whether the panel's browser is worth starting before anyone needs it.
///
/// The browser tools are what attach to it, so a user who never turned
/// browser-use on never needs the process: starting one anyway would spend a
/// browser's worth of memory and startup time on every launch for nothing.
/// When it is on, the browser is started warm so the agent's first tool call
/// has somewhere to attach and the user's first link click has nothing to wait
/// for.
pub fn warm_required(settings: &BrowserSettings) -> bool {
    settings.enabled
}

/// What a DevTools endpoint says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelProbe {
    /// The browser's product string, e.g. `Edg/131.0.2903.86`. WebView2 is Edge
    /// underneath, so this is also what tells the panel's browser apart from any
    /// other Chromium on the machine.
    pub product: String,
    /// How many pages the endpoint lists. The agent's tools drive the page they
    /// find, so an endpoint with none on it is not yet a browser anyone can use.
    pub pages: usize,
}

/// Ask a DevTools endpoint to describe itself.
///
/// This is what decides whether the endpoint is worth publishing: the browser
/// tools attach to a published panel endpoint *instead of* falling back to
/// settings, so publishing an address nothing answers on would take the tools
/// down with it rather than leave them working against a browser of their own.
pub async fn probe(endpoint: &str) -> Result<PanelProbe, String> {
    let client = browser_service::cdp::CdpClient::connect(endpoint)
        .await
        .map_err(|error| error.to_string())?;
    let described = async {
        let version = client
            .call("Browser.getVersion", Value::Null, None)
            .await
            .map_err(|error| error.to_string())?;
        let targets = client
            .call("Target.getTargets", Value::Null, None)
            .await
            .map_err(|error| error.to_string())?;
        Ok::<PanelProbe, String>(PanelProbe {
            product: text_of(&version, "product"),
            pages: page_count(&targets),
        })
    }
    .await;
    client.close().await;
    described
}

/// A string field of a CDP result, or the empty string when it is absent.
fn text_of(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// How many `page` targets a `Target.getTargets` result carries.
///
/// Only pages count: the same result lists the browser itself, service workers
/// and any out-of-process iframe, none of which a `browser_navigate` can drive.
fn page_count(targets: &Value) -> usize {
    targets
        .get("targetInfos")
        .and_then(Value::as_array)
        .map(|infos| {
            infos
                .iter()
                .filter(|info| info.get("type").and_then(Value::as_str) == Some("page"))
                .count()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_warm_browser_is_only_for_a_user_with_browser_tools_on() {
        assert!(warm_required(&BrowserSettings {
            enabled: true,
            ..BrowserSettings::default()
        }));
        // The default configuration has browser-use off, so a default install
        // starts no browser at all.
        assert!(!warm_required(&BrowserSettings::default()));
    }

    #[test]
    fn only_page_targets_are_counted_as_pages() {
        let targets = json!({
            "targetInfos": [
                { "type": "page", "url": "about:blank" },
                { "type": "browser", "url": "" },
                { "type": "page", "url": "https://example.com/" },
                { "type": "iframe", "url": "https://example.com/embed" },
            ]
        });
        assert_eq!(page_count(&targets), 2);
    }

    #[test]
    fn a_target_list_that_never_arrived_has_no_pages() {
        assert_eq!(page_count(&json!({})), 0);
        assert_eq!(page_count(&Value::Null), 0);
        assert_eq!(page_count(&json!({ "targetInfos": "nonsense" })), 0);
    }

    #[test]
    fn a_version_without_a_product_is_read_as_empty() {
        assert_eq!(text_of(&json!({ "product": "Edg/131.0" }), "product"), "Edg/131.0");
        assert_eq!(text_of(&json!({}), "product"), "");
        assert_eq!(text_of(&json!({ "product": 7 }), "product"), "");
    }

    /// A port this test has just let go of: nothing is listening, so a probe
    /// has to come back with an error rather than a browser.
    #[tokio::test]
    async fn probing_a_port_nobody_listens_on_fails() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);

        let error = probe(&format!("http://127.0.0.1:{port}"))
            .await
            .expect_err("nothing is listening on that port");
        assert!(!error.is_empty(), "the failure has to say something");
    }
}
