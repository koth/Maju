//! The one honest CDP conversation that produces a [`WireDoc`].
//!
//! Everything here is I/O and error mapping; the interesting logic lives in
//! [`crate::ax::wire`] (parsing), [`crate::ax::render`] (shape) and
//! [`crate::ax::diff`] (cheapness). That split is deliberate: the capture path
//! cannot be tested without a browser, so it is kept thin enough to read.
//!
//! Two choices are worth knowing about:
//!
//! - **Attach per capture, detach after.** A long-lived session would avoid a
//!   round trip, but it also keeps a target id pinned across navigations, and a
//!   pinned stale target is how "the browser stopped answering" bugs start.
//!   Attaching is two messages; correctness is worth more than two messages.
//! - **Settle before reading.** The accessibility tree and the DOM tree are
//!   fetched after the cheap probe reports quiet, not before. Capturing first
//!   and waiting later would describe a page state that is already gone.

use serde_json::{Value, json};

use crate::ax::action::{self, Action, ActionError, Target};
use crate::ax::settle::{SettleOptions, SettleOutcome, settle};
use crate::ax::wire::WireDoc;
use crate::cdp::{CdpClient, CdpError};

/// Errors from a capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AxError {
    /// The CDP conversation failed.
    Cdp(CdpError),
    /// The browser has no page target to capture.
    NoPage,
    /// The page answered, but not with the shape this module needs.
    Shape(String),
}

impl std::fmt::Display for AxError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AxError::Cdp(error) => write!(formatter, "accessibility capture failed: {error}"),
            AxError::NoPage => write!(formatter, "the browser has no page to capture"),
            AxError::Shape(detail) => {
                write!(formatter, "accessibility capture got an unexpected answer: {detail}")
            }
        }
    }
}

impl std::error::Error for AxError {}

impl From<CdpError> for AxError {
    fn from(error: CdpError) -> Self {
        AxError::Cdp(error)
    }
}

/// A page target, as the browser lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageTarget {
    pub target_id: String,
    pub url: String,
    pub title: String,
}

/// A capture: the document plus how the wait ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    pub doc: WireDoc,
    pub settle: SettleOutcome,
    /// The page this capture came from. Acting on a line number later has to
    /// attach to *this* target, or the number would address another tab.
    pub target_id: String,
}

/// List the browser's page targets, in the order the browser reports them.
pub async fn page_targets(client: &CdpClient) -> Result<Vec<PageTarget>, CdpError> {
    let answer = client.call("Target.getTargets", json!({}), None).await?;
    let targets = answer
        .get("targetInfos")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(targets
        .into_iter()
        .filter(|target| target.get("type").and_then(Value::as_str) == Some("page"))
        .filter_map(|target| {
            Some(PageTarget {
                target_id: target.get("targetId").and_then(Value::as_str)?.to_string(),
                url: target
                    .get("url")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                title: target
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect())
}

/// Capture one page. `target` defaults to the page worth looking at.
pub async fn capture(
    client: &CdpClient,
    target: Option<&str>,
    options: SettleOptions,
) -> Result<Capture, AxError> {
    let target_id = match target {
        Some(target) => target.to_string(),
        None => pick(&page_targets(client).await?)
            .ok_or(AxError::NoPage)?
            .target_id
            .clone(),
    };

    let session = attach(client, &target_id).await?;
    let mut result = capture_attached(client, &session, options).await;
    if let Ok(capture) = &mut result {
        capture.target_id = target_id.clone();
    }
    // Detach even on failure: a leaked session holds a target the next capture
    // may want to attach to.
    let _ = client
        .call("Target.detachFromTarget", json!({ "sessionId": session }), None)
        .await;
    result
}

/// Perform `action` on one element of a previously captured page.
///
/// The target is named rather than discovered: a line number belongs to the
/// capture that produced it, and resolving it against "whatever page is first"
/// would let a background tab answer for the one the model read.
pub async fn act(
    client: &CdpClient,
    target_id: &str,
    target: Target,
    action: &Action,
) -> Result<(), ActionError> {
    let session = attach(client, target_id)
        .await
        .map_err(|error| ActionError::Session(error.to_string()))?;
    prepare_session(client, &session).await;
    let result = action::perform(client, &session, target, action).await;
    let _ = client
        .call("Target.detachFromTarget", json!({ "sessionId": session }), None)
        .await;
    result
}

/// Override the page's viewport, or clear the override.
///
/// `None` clears it. An override lasts for the life of the document, so leaving
/// one in place silently changes what every later action and screenshot sees —
/// the caller is expected to reset it once the responsive check is done.
pub async fn viewport(
    client: &CdpClient,
    target: Option<&str>,
    size: Option<(u32, u32)>,
) -> Result<(), AxError> {
    let target_id = match target {
        Some(target) if !target.is_empty() => target.to_string(),
        _ => pick(&page_targets(client).await?)
            .ok_or(AxError::NoPage)?
            .target_id
            .clone(),
    };
    let session = attach(client, &target_id).await?;
    prepare_session(client, &session).await;
    let result = match size {
        Some((width, height)) if width > 0 && height > 0 => client
            .call(
                "Emulation.setDeviceMetricsOverride",
                json!({
                    "width": width,
                    "height": height,
                    "deviceScaleFactor": 1.0,
                    "mobile": false,
                }),
                Some(&session),
            )
            .await
            .map(|_| ())
            .map_err(AxError::from),
        Some(_) => Err(AxError::Shape(
            "a viewport needs a width and a height above zero".to_string(),
        )),
        None => client
            .call(
                "Emulation.clearDeviceMetricsOverride",
                json!({}),
                Some(&session),
            )
            .await
            .map(|_| ())
            .map_err(AxError::from),
    };
    let _ = client
        .call("Target.detachFromTarget", json!({ "sessionId": session }), None)
        .await;
    result
}

/// Which page a caller means when it did not say.
///
/// The panel keeps a blank warm page alive so the browser (and its DevTools
/// endpoint) outlives the last tab. That page is never what the agent wants to
/// read, so a page with a real URL wins; only when every target is blank does
/// the first one serve.
fn pick(pages: &[PageTarget]) -> Option<&PageTarget> {
    pages
        .iter()
        .find(|page| !is_blank(&page.url))
        .or_else(|| pages.first())
}

fn is_blank(url: &str) -> bool {
    url.is_empty() || url == "about:blank" || url.starts_with("about:blank#")
}

async fn attach(client: &CdpClient, target_id: &str) -> Result<String, AxError> {
    let answer = client
        .call(
            "Target.attachToTarget",
            json!({ "targetId": target_id, "flatten": true }),
            None,
        )
        .await?;
    answer
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| AxError::Shape("Target.attachToTarget returned no sessionId".to_string()))
}

async fn capture_attached(
    client: &CdpClient,
    session: &str,
    options: SettleOptions,
) -> Result<Capture, AxError> {
    prepare_session(client, session).await;

    let (outcome, _) = settle(options, || probe_signature(client, session)).await;

    let page = evaluate(client, session, PAGE_INFO).await?;
    let ax = client
        .call("Accessibility.getFullAXTree", json!({}), Some(session))
        .await?;
    let dom = client
        .call(
            "DOM.getDocument",
            json!({ "depth": -1, "pierce": true }),
            Some(session),
        )
        .await?;
    let key = document_key(client, session, &page).await;

    Ok(Capture {
        doc: WireDoc::from_cdp(key, &ax, &dom, &page),
        settle: outcome,
        target_id: String::new(),
    })
}

/// Evaluate an expression on a page and return its value.
///
/// The expression runs in the page's main world with `returnByValue`, so it can
/// only return JSON-serialisable data. This is a debugging and verification
/// door: reading a field's value, checking `document.activeElement`, asserting a
/// URL. Product tools should prefer observation; this exists because "did that
/// click do anything?" is not always answerable from the accessibility tree.
pub async fn evaluate_on_page(
    client: &CdpClient,
    target_id: &str,
    expression: &str,
) -> Result<Value, AxError> {
    let session = attach(client, target_id).await?;
    let value = self::evaluate(client, &session, expression).await;
    let _ = client
        .call("Target.detachFromTarget", json!({ "sessionId": session }), None)
        .await;
    value
}

/// Turn on the domains a capture and an action need.
///
/// All three are best effort: a WebView2 build that refuses one of them should
/// still be readable, and a page that cannot take focus should still be
/// capturable.
///
/// `Emulation.setFocusEmulationEnabled` is the one that is easy to miss and
/// impossible to work around. A background or headless page is not focused at
/// the window level, so the browser routes `Input.*` events nowhere and
/// `insertText` silently does nothing — the field looks focused, the text never
/// arrives, and nothing errors. Focus emulation makes the page behave as if it
/// were in front.
async fn prepare_session(client: &CdpClient, session: &str) {
    let _ = client
        .call("Accessibility.enable", json!({}), Some(session))
        .await;
    let _ = client
        .call("Page.enable", json!({}), Some(session))
        .await;
    let _ = client
        .call(
            "Emulation.setFocusEmulationEnabled",
            json!({ "enabled": true }),
            Some(session),
        )
        .await;
}
const SIGNATURE: &str = r#"(() => {
  const body = document.body;
  return [
    document.readyState,
    body ? body.querySelectorAll('*').length : 0,
    body && body.innerText ? body.innerText.length : 0
  ].join(':');
})()"#;

/// Title, url and readiness in one round trip.
const PAGE_INFO: &str = r#"(() => ({
  title: document.title || '',
  url: location.href || '',
  readyState: document.readyState
}))()"#;

async fn probe_signature(client: &CdpClient, session: &str) -> String {
    match evaluate(client, session, SIGNATURE).await {
        Ok(value) => value
            .get("readyState")
            .and_then(Value::as_str)
            .map(|state| state.to_string())
            .unwrap_or_else(|| value.to_string()),
        // A probe that fails must not look like a stable page: the changing
        // error text keeps the settle loop awake until the deadline.
        Err(error) => error.to_string(),
    }
}

/// `Runtime.evaluate` with `returnByValue`, unwrapped to the JS value.
async fn evaluate(
    client: &CdpClient,
    session: &str,
    expression: &str,
) -> Result<Value, AxError> {
    let answer = client
        .call(
            "Runtime.evaluate",
            json!({
                "expression": expression,
                "returnByValue": true,
                "awaitPromise": true,
            }),
            Some(session),
        )
        .await?;
    if let Some(details) = answer.get("exceptionDetails") {
        return Err(AxError::Shape(format!(
            "page script threw: {}",
            details
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
        )));
    }
    answer
        .get("result")
        .and_then(|result| result.get("value"))
        .cloned()
        .ok_or_else(|| AxError::Shape("Runtime.evaluate returned no value".to_string()))
}

/// A key that changes exactly when the document is replaced.///
/// `frameId` alone is stable across same-document navigations, and `loaderId`
/// alone is empty on `about:blank`; the pair covers both.
async fn document_key(client: &CdpClient, session: &str, page: &Value) -> String {
    let frame = client
        .call("Page.getFrameTree", json!({}), Some(session))
        .await
        .ok()
        .and_then(|answer| answer.get("frameTree").cloned())
        .and_then(|tree| tree.get("frame").cloned());
    let frame_id = frame
        .as_ref()
        .and_then(|frame| frame.get("id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let loader_id = frame
        .as_ref()
        .and_then(|frame| frame.get("loaderId"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if frame_id.is_empty() && loader_id.is_empty() {
        // No frame tree: fall back to the URL, which is still a better
        // invalidation key than a constant.
        return page
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
    }
    format!("{frame_id}:{loader_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(url: &str) -> PageTarget {
        PageTarget {
            target_id: url.to_string(),
            url: url.to_string(),
            title: String::new(),
        }
    }

    #[test]
    fn a_blank_warm_page_is_not_what_the_agent_means() {
        let pages = vec![page("about:blank"), page("https://example.com/")];
        assert_eq!(pick(&pages).unwrap().url, "https://example.com/");
    }

    #[test]
    fn a_lone_blank_page_is_still_something_to_read() {
        let pages = vec![page("about:blank")];
        assert_eq!(pick(&pages).unwrap().url, "about:blank");
    }

    #[test]
    fn no_pages_means_no_capture() {
        assert!(pick(&[]).is_none());
    }

    #[test]
    fn blank_url_spellings_all_count() {
        for url in ["", "about:blank", "about:blank#", "about:blank#fragment"] {
            assert!(is_blank(url), "{url:?} should be blank");
        }
        assert!(!is_blank("data:text/html,hello"));
        assert!(!is_blank("https://example.com/"));
    }
}
