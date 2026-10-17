//! The panel driver: how the agent's browser tools drive this app's panel.
//!
//! `browser-service` owns the contract (`PanelDriver`); this is the only
//! implementation, because this crate is the only layer that owns the panel's
//! webviews. The app publishes it at startup (macOS: the WKWebView has no CDP
//! endpoint, so the tools cannot reach the panel any other way).
//!
//! Two mechanics live here and nowhere else:
//!
//! - **Getting an answer out of a page.** Tauri's `eval` is fire-and-forget,
//!   so the script is wrapped: it runs the expression, then reports the result
//!   back through the Tauri IPC bridge that every webview carries
//!   (`browser_panel_eval_result`). A oneshot channel keyed by a random call
//!   id pairs the two halves; a page that navigates away mid-call simply never
//!   answers, and the caller times out instead of hanging.
//! - **Knowing when a page has settled.** The webview's page-load callback
//!   already knows the two halves of every load; the host mirrors them into a
//!   per-tab loading set and a broadcast channel, so `navigate` can return
//!   when the page has finished and `wait_settled` can wait out whatever load
//!   an action started.

use std::time::Duration;

use serde_json::Value;
use tauri::{AppHandle, Manager};
use tokio::sync::broadcast;

use browser_service::panel_driver::{PanelDriver, PanelTabInfo, transport_error};

use super::{BrowserPanelHost, PageEvent};

/// How long a navigation may take to finish loading.
const NAVIGATE_TIMEOUT: Duration = Duration::from_secs(15);
/// The quiet period `wait_settled` accepts as "no load is coming". Short on
/// purpose: an action that starts a navigation does so immediately.
const SETTLE_QUIET: Duration = Duration::from_millis(600);

/// The app's panel, exposed to the agent's tools as their browser.
pub struct TauriPanelDriver {
    app: AppHandle,
}

impl TauriPanelDriver {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }

    fn host(&self) -> tauri::State<'_, BrowserPanelHost> {
        self.app.state::<BrowserPanelHost>()
    }

    /// Wait until `tab_id`'s page reports `finished`, or the timeout does.
    ///
    /// The receiver is the caller's own, subscribed before whatever started
    /// the load: subscribing after the fact is how "the page never settles"
    /// bugs are made.
    async fn await_load(
        &self,
        tab_id: &str,
        events: &mut broadcast::Receiver<PageEvent>,
        timeout: Duration,
    ) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                // A page that never finishes is still a page: the caller reads
                // whatever is there rather than getting a hard failure for a
                // slow site.
                return Ok(());
            }
            match tokio::time::timeout(remaining, events.recv()).await {
                Ok(Ok(event)) if event.tab_id == tab_id && event.event == "finished" => {
                    return Ok(());
                }
                Ok(Ok(_)) | Ok(Err(_)) => continue,
                Err(_) => return Ok(()),
            }
        }
    }
}

/// Resolve the moment `tab_id`'s page starts a new load. Used as a `select!`
/// arm against an eval's answer: a navigation destroys the document the script
/// was running in, so the answer can never arrive — and the caller should hear
/// that immediately rather than after the full timeout.
async fn navigation_started(events: &mut broadcast::Receiver<PageEvent>, tab_id: &str) {
    loop {
        match events.recv().await {
            Ok(event) if event.tab_id == tab_id && event.event == "started" => return,
            // Other tabs' loads and a lagged receiver are not this tab
            // navigating; the sender lives in the host for the app's
            // lifetime, so a closed channel means "never" here.
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => std::future::pending().await,
        }
    }
}

#[async_trait::async_trait]
impl PanelDriver for TauriPanelDriver {
    async fn eval(&self, tab_id: &str, script: &str, timeout: Duration) -> Result<Value, String> {
        let host = self.host();
        let webview = host.webview_for(tab_id)?;
        let call_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        host.register_eval(&call_id, tx)?;
        // Subscribed before the script is injected, so a navigation the script
        // itself starts cannot slip past between the two.
        let mut events = host.page_events.subscribe();
        let wrapped = wrap_eval(&call_id, script);
        if let Err(error) = webview.eval(&wrapped) {
            host.cancel_eval(&call_id);
            return Err(transport_error(format!("无法注入页面脚本:{error}")));
        }
        // Three ways out: the page answers, the caller's budget runs out, or
        // the page starts a navigation — which destroys the document the
        // script ran in, so its answer can never arrive and waiting for it is
        // how "eval 无响应" 20-second stalls were made.
        let payload = tokio::select! {
            payload = rx => match payload {
                Ok(payload) => payload,
                Err(_) => {
                    return Err(transport_error("页面应答通道中断"));
                }
            },
            _ = tokio::time::sleep(timeout) => {
                host.cancel_eval(&call_id);
                return Err(transport_error(
                    "页面没有响应(求值超时):页面可能正忙于长任务,或脚本在求值期间被导航丢弃",
                ));
            }
            _ = navigation_started(&mut events, tab_id) => {
                host.cancel_eval(&call_id);
                return Err(transport_error("页面在求值期间开始跳转,脚本应答已随旧文档销毁"));
            }
        };
        if payload.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(payload.get("value").cloned().unwrap_or(Value::Null))
        } else {
            // The page itself answered: this failure is authoritative, so it
            // deliberately does not carry the transport prefix — a caller must
            // not retry it as if the outcome were unknown.
            Err(payload
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("页面脚本执行失败")
                .to_string())
        }
    }

    async fn navigate(&self, tab_id: &str, url: &str) -> Result<(), String> {
        let host = self.host();
        // Subscribed before the navigation is issued, so the load it starts
        // cannot slip past between the two calls.
        let mut events = host.page_events.subscribe();
        let target = super::normalize_url(url)?;
        let surface = host.tab(tab_id)?;
        // Anything already queued belongs to an earlier load; only the one
        // this navigation starts counts.
        while events.try_recv().is_ok() {}
        surface.navigate(&target).await?;
        self.await_load(tab_id, &mut events, NAVIGATE_TIMEOUT).await
    }

    async fn wait_settled(&self, tab_id: &str) -> Result<(), String> {
        let host = self.host();
        // Subscribed before the loading check, so a load that finishes right
        // between the two still has its `finished` queued for us.
        let mut events = host.page_events.subscribe();
        if host.is_loading(tab_id) {
            return self.await_load(tab_id, &mut events, NAVIGATE_TIMEOUT).await;
        }
        // Not loading right now: an action may still have started one a beat
        // ago. Give a started load a short window to show itself; silence
        // means there was nothing to wait for.
        match tokio::time::timeout(SETTLE_QUIET, events.recv()).await {
            Ok(Ok(event)) if event.tab_id == tab_id && event.event == "started" => {
                self.await_load(tab_id, &mut events, NAVIGATE_TIMEOUT).await
            }
            Ok(Ok(event)) if event.tab_id == tab_id && event.event == "finished" => Ok(()),
            _ => Ok(()),
        }
    }

    async fn open_tab(&self, url: &str) -> Result<String, String> {
        let tab = super::browser_panel_open(self.app.clone(), self.host(), url.to_string()).await?;
        super::browser_panel_activate(self.app.clone(), self.host(), tab.tab_id.clone()).await?;
        Ok(tab.tab_id)
    }

    async fn close_tab(&self, tab_id: &str) -> Result<(), String> {
        super::browser_panel_close(self.app.clone(), self.host(), tab_id.to_string()).await
    }

    async fn activate_tab(&self, tab_id: &str) -> Result<(), String> {
        super::browser_panel_activate(self.app.clone(), self.host(), tab_id.to_string()).await
    }

    async fn tabs(&self) -> Vec<PanelTabInfo> {
        self.host()
            .report()
            .map(|report| {
                report
                    .tabs
                    .into_iter()
                    .map(|tab| PanelTabInfo {
                        tab_id: tab.tab_id,
                        url: tab.url,
                        title: tab.title,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn active_tab(&self) -> Option<String> {
        self.host()
            .report()
            .ok()
            .and_then(|report| report.active)
    }
}

/// The script half of the eval protocol: run the expression, report the
/// outcome back over the webview's own Tauri IPC bridge.
///
/// `script` is an *expression* (it may evaluate to a Promise — it is awaited).
/// The result travels as `{ ok: true, value }` or `{ ok: false, error }`; a
/// page that never runs this (closed, navigated mid-flight) simply never
/// answers, which the caller's timeout covers.
///
/// The wrapper is a template file, not string formatting: it is parseable
/// JavaScript on its own and the UI tests execute it, so a syntax error fails
/// a test instead of silently timing out in the app. Placeholders: the call
/// id is substituted as a JSON string literal; the script is embedded as
/// source — not as a string passed to `eval` — because an inner `eval` would
/// be subject to the page's CSP, and strict sites forbid it.
fn wrap_eval(call_id: &str, script: &str) -> String {
    let id = serde_json::to_string(call_id).unwrap_or_else(|_| "\"\"".to_string());
    WRAP_TEMPLATE
        .replace("__KODEX_CALL_ID__", &id)
        .replace("__KODEX_SCRIPT__", script)
}

const WRAP_TEMPLATE: &str = include_str!("wrap_eval.js");

#[cfg(test)]
mod tests {
    use super::wrap_eval;

    #[test]
    fn the_wrapper_awaits_the_expression_and_reports_both_outcomes() {
        let wrapped = wrap_eval("call-1", "Promise.resolve(42)");
        assert!(wrapped.contains("await (Promise.resolve(42))"));
        assert!(wrapped.contains("callId: \"call-1\""));
        assert!(wrapped.contains("ok: true"));
        assert!(wrapped.contains("ok: false"));
        // The response goes through the Tauri IPC bridge, which every webview
        // carries — including the panel's remote pages.
        assert!(wrapped.contains("__TAURI_INTERNALS__.invoke"));
    }

    #[test]
    fn the_call_id_is_json_escaped() {
        let wrapped = wrap_eval("ca\"ll", "1");
        assert!(wrapped.contains(r#"callId: "ca\"ll""#), "got {wrapped}");
    }

    #[test]
    fn the_wrapped_script_is_balanced() {
        // A weak check next to the real one (the UI tests execute the wrapper
        // in jsdom): at minimum the braces and parens must come out even.
        let wrapped = wrap_eval("c", "window.__kodexBrowser.exec({\"cmd\":\"snapshot\"})");
        let balance = |script: &str, open: char, close: char| {
            script.matches(open).count() as i64 - script.matches(close).count() as i64
        };
        assert_eq!(balance(&wrapped, '{', '}'), 0, "got {wrapped}");
        assert_eq!(balance(&wrapped, '(', ')'), 0, "got {wrapped}");
    }
}
