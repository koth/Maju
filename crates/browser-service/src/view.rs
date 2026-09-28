//! The right-hand browser view: a live screencast of one browser page plus
//! the input path back into it.
//!
//! A view is 1:1 with the session's browser, not with a page. The panel
//! attaches once, tracks every `page` target the browser reports — the
//! agent's tool-driven navigation opens and closes pages too — and streams
//! exactly one target at a time, which keeps the frame channel cheap and
//! matches what the panel can show. Switching targets stops the old cast
//! before starting the new one.
//!
//! Everything the frontend needs arrives through [`BrowserViewSink`], which
//! mirrors the tauri event contract (`browser_view:status` / `:targets` /
//! `:meta` / `:frame`) so the desktop command layer stays a thin
//! translation. Input is forwarded with unchanged meaning: the panel's
//! logical pixels are CDP pixels because the viewport override is 1:1.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, broadcast};

use crate::cdp::{CdpClient, CdpError, CdpEvent};

/// Screencast dimensions before the panel reports its size.
///
/// The browser scales frames down to fit, so a generous default simply means
/// the first frames arrive slightly larger than needed.
const DEFAULT_MAX_WIDTH: u32 = 1280;
const DEFAULT_MAX_HEIGHT: u32 = 800;

/// Where the view publishes its state. The desktop layer implements this as
/// tauri events; tests record it.
pub trait BrowserViewSink: Send + Sync + 'static {
    /// Connection lifecycle changed.
    fn status(&self, status: ViewStatus, detail: Option<String>);
    /// The full list of page targets replaced (created/destroyed).
    fn targets(&self, targets: Vec<PageTarget>);
    /// One target's URL or title moved; an incremental update.
    fn meta(&self, target_id: &str, url: Option<String>, title: Option<String>);
    /// One screencast frame, base64 JPEG.
    fn frame(&self, target_id: &str, jpeg_base64: &str, seq: u64);
}

/// Connection status of a view, mirroring `browser_view:status`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ViewStatus {
    /// Connecting (or re-attaching) to the CDP endpoint.
    Connecting,
    /// Attached; targets and frames are flowing.
    Live,
    /// Detached on request; the browser itself keeps running.
    Closed,
    /// The connection could not be established, or was lost.
    Failed,
}

/// One browser page, mirroring an entry of `browser_view:targets`.
#[derive(Clone, Debug, Serialize)]
pub struct PageTarget {
    pub target_id: String,
    pub url: String,
    pub title: String,
    pub r#type: String,
}

/// A user interaction with the live page, tagged for the command contract.
///
/// The frontend sends this as `{"kind": "mouse" | "key" | "text", ...}`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewInputEvent {
    Mouse {
        r#type: String,
        x: f64,
        y: f64,
        button: String,
        click_count: u32,
        delta_x: f64,
        delta_y: f64,
        modifiers: u32,
    },
    Key {
        r#type: String,
        key: String,
        code: String,
        text: String,
        modifiers: u32,
    },
    Text {
        text: String,
    },
}

struct Focused {
    target_id: String,
    session_id: String,
}

struct Inner {
    sink: Arc<dyn BrowserViewSink>,
    /// Taken (not just closed) by `detach`, so every later operation fails
    /// fast with `Disconnected` instead of asking a dead socket.
    client: Mutex<Option<Arc<CdpClient>>>,
    /// Page targets by id: a `BTreeMap` so the full-list push has a stable
    /// order the tab strip can rely on.
    targets: Mutex<BTreeMap<String, PageTarget>>,
    focus: Mutex<Option<Focused>>,
    /// Explicitly requested viewport sizes; a target without an entry keeps
    /// the browser's own viewport until the panel asks.
    sizes: Mutex<HashMap<String, (u32, u32)>>,
    seq: AtomicU64,
    /// Set before any deliberate shutdown so the event pump can tell a
    /// requested `detach` from a lost connection.
    detached: AtomicBool,
}

/// The right-side browser view for one session's browser.
///
/// Built by [`BrowserView::attach`]; dropping it closes the CDP connection,
/// [`BrowserView::detach`] does the same explicitly and idempotently.
pub struct BrowserView {
    inner: Arc<Inner>,
}

impl BrowserView {
    /// Connect to the browser, enable target discovery, and push the current
    /// page list before returning.
    ///
    /// `endpoint` accepts the same shapes as [`CdpClient::connect`].
    pub async fn attach(
        endpoint: &str,
        sink: Arc<dyn BrowserViewSink>,
    ) -> Result<Arc<BrowserView>, CdpError> {
        sink.status(ViewStatus::Connecting, None);
        let client = match CdpClient::connect(endpoint).await {
            Ok(client) => Arc::new(client),
            Err(error) => {
                sink.status(ViewStatus::Failed, Some(error.to_string()));
                return Err(error);
            }
        };

        let inner = Arc::new(Inner {
            sink,
            client: Mutex::new(Some(Arc::clone(&client))),
            targets: Mutex::new(BTreeMap::new()),
            focus: Mutex::new(None),
            sizes: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(0),
            detached: AtomicBool::new(false),
        });
        // Subscribe before the first command: the browser answers discovery
        // with `targetCreated` events, and those must not race the pump.
        let events = client.events();
        let pump_inner = Arc::clone(&inner);
        let pump_client = Arc::clone(&client);
        tokio::spawn(async move {
            Pump {
                inner: pump_inner,
                client: pump_client,
            }
            .run(events)
            .await;
        });

        if let Err(error) = client
            .call("Target.setDiscoverTargets", json!({ "discover": true }), None)
            .await
        {
            inner.detached.store(true, Ordering::SeqCst);
            inner
                .sink
                .status(ViewStatus::Failed, Some(error.to_string()));
            client.close().await;
            return Err(error);
        }

        let targets = inner
            .targets
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        inner.sink.targets(targets);
        inner.sink.status(ViewStatus::Live, None);
        Ok(Arc::new(BrowserView { inner }))
    }

    /// Stop screencast and disconnect the view. The browser and the agent's
    /// tools are untouched. Idempotent; the view goes inert afterwards.
    pub async fn detach(&self) {
        let client = self.inner.client.lock().await.take();
        let Some(client) = client else {
            return;
        };
        // Before closing: the pump must not read this shutdown as a failure.
        self.inner.detached.store(true, Ordering::SeqCst);
        if let Some(previous) = self.inner.focus.lock().await.take() {
            let _ = client
                .call("Page.stopScreencast", json!({}), Some(&previous.session_id))
                .await;
            let _ = client
                .call(
                    "Target.detachFromTarget",
                    json!({ "sessionId": previous.session_id }),
                    None,
                )
                .await;
        }
        client.close().await;
        self.inner.sink.status(ViewStatus::Closed, None);
    }

    /// Stream `target_id` instead of the current one.
    ///
    /// Only one target is cast at a time, so the previous session is stopped
    /// and detached first. Focusing the already-focused target is a no-op.
    pub async fn focus(&self, target_id: &str) -> Result<(), CdpError> {
        let client = self.client().await?;
        let mut focus = self.inner.focus.lock().await;
        if focus
            .as_ref()
            .map(|focused| focused.target_id == target_id)
            .unwrap_or(false)
        {
            return Ok(());
        }
        if let Some(previous) = focus.take() {
            let _ = client
                .call("Page.stopScreencast", json!({}), Some(&previous.session_id))
                .await;
            let _ = client
                .call(
                    "Target.detachFromTarget",
                    json!({ "sessionId": previous.session_id }),
                    None,
                )
                .await;
        }

        let session_id = client
            .call(
                "Target.attachToTarget",
                json!({ "targetId": target_id, "flatten": true }),
                None,
            )
            .await?
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| CdpError::Protocol {
                code: -32000,
                message: "Target.attachToTarget returned no sessionId".to_string(),
            })?;
        client
            .call("Page.enable", json!({}), Some(&session_id))
            .await?;
        if let Some((width, height)) = self.inner.sizes.lock().await.get(target_id).copied() {
            // A size the panel already asked for applies to whichever target
            // it is now showing.
            client
                .call(
                    "Emulation.setDeviceMetricsOverride",
                    device_metrics(width, height),
                    Some(&session_id),
                )
                .await?;
        }
        let (max_width, max_height) = self.size_for(target_id).await;
        client
            .call(
                "Page.startScreencast",
                screencast_params(max_width, max_height),
                Some(&session_id),
            )
            .await?;
        *focus = Some(Focused {
            target_id: target_id.to_string(),
            session_id,
        });
        Ok(())
    }

    /// Open `url` as a new page and return its target id. The new target
    /// reaches the sink through the normal target events.
    pub async fn open_page(&self, url: &str) -> Result<String, CdpError> {
        let client = self.client().await?;
        let result = client
            .call("Target.createTarget", json!({ "url": url }), None)
            .await?;
        result
            .get("targetId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| CdpError::Protocol {
                code: -32000,
                message: "Target.createTarget returned no targetId".to_string(),
            })
    }

    /// Close a page. If it was the streamed one, the view stops streaming.
    pub async fn close_page(&self, target_id: &str) -> Result<(), CdpError> {
        let client = self.client().await?;
        client
            .call("Target.closeTarget", json!({ "targetId": target_id }), None)
            .await?;
        let mut focus = self.inner.focus.lock().await;
        if focus
            .as_ref()
            .map(|focused| focused.target_id == target_id)
            .unwrap_or(false)
        {
            // The page is gone; its screencast ended with it.
            *focus = None;
        }
        Ok(())
    }

    /// Set the page's layout viewport in logical pixels, rounded to whole
    /// ones, and restart the screencast at the new resolution.
    ///
    /// In attach mode the override affects only this page's layout viewport —
    /// the user's browser window keeps its own size. A size for a target that
    /// is not currently streamed is remembered and applied on focus.
    pub async fn set_size(
        &self,
        target_id: &str,
        width: f64,
        height: f64,
    ) -> Result<(), CdpError> {
        let client = self.client().await?;
        let size = (round_pixels(width), round_pixels(height));
        self.inner
            .sizes
            .lock()
            .await
            .insert(target_id.to_string(), size);
        let session_id = {
            let focus = self.inner.focus.lock().await;
            match focus.as_ref() {
                Some(focused) if focused.target_id == target_id => focused.session_id.clone(),
                _ => return Ok(()),
            }
        };

        let (width, height) = size;
        client
            .call(
                "Emulation.setDeviceMetricsOverride",
                device_metrics(width, height),
                Some(&session_id),
            )
            .await?;
        // Frame resolution is fixed when the cast starts, so the restart is
        // what actually changes what the panel receives.
        client
            .call("Page.stopScreencast", json!({}), Some(&session_id))
            .await?;
        client
            .call(
                "Page.startScreencast",
                screencast_params(width, height),
                Some(&session_id),
            )
            .await?;
        Ok(())
    }

    /// Forward one user interaction to the streamed page.
    ///
    /// Coordinates are logical pixels and map 1:1 to CDP's x/y after the
    /// viewport override. The target must be the focused one: input to a page
    /// that is not being shown would silently type into the wrong tab.
    pub async fn input(
        &self,
        target_id: &str,
        event: &ViewInputEvent,
    ) -> Result<(), CdpError> {
        let client = self.client().await?;
        let session_id = {
            let focus = self.inner.focus.lock().await;
            match focus.as_ref() {
                Some(focused) if focused.target_id == target_id => focused.session_id.clone(),
                _ => {
                    return Err(CdpError::Protocol {
                        code: -32000,
                        message: format!("{target_id} is not the focused target"),
                    });
                }
            }
        };
        let (method, params) = match event {
            ViewInputEvent::Mouse {
                r#type,
                x,
                y,
                button,
                click_count,
                delta_x,
                delta_y,
                modifiers,
            } => (
                "Input.dispatchMouseEvent",
                json!({
                    "type": r#type,
                    "x": x,
                    "y": y,
                    "button": button,
                    "clickCount": click_count,
                    "deltaX": delta_x,
                    "deltaY": delta_y,
                    "modifiers": modifiers,
                }),
            ),
            ViewInputEvent::Key {
                r#type,
                key,
                code,
                text,
                modifiers,
            } => (
                "Input.dispatchKeyEvent",
                json!({
                    "type": r#type,
                    "key": key,
                    "code": code,
                    "text": text,
                    "modifiers": modifiers,
                }),
            ),
            ViewInputEvent::Text { text } => {
                ("Input.insertText", json!({ "text": text }))
            }
        };
        client.call(method, params, Some(&session_id)).await?;
        Ok(())
    }

    async fn client(&self) -> Result<Arc<CdpClient>, CdpError> {
        self.inner
            .client
            .lock()
            .await
            .clone()
            .ok_or(CdpError::Disconnected)
    }

    async fn size_for(&self, target_id: &str) -> (u32, u32) {
        self.inner
            .sizes
            .lock()
            .await
            .get(target_id)
            .copied()
            .unwrap_or((DEFAULT_MAX_WIDTH, DEFAULT_MAX_HEIGHT))
    }
}

impl Drop for BrowserView {
    /// A view dropped without `detach` must not leak its connection or its
    /// tasks; closing the client ends both.
    fn drop(&mut self) {
        self.inner.detached.store(true, Ordering::SeqCst);
        let inner = Arc::clone(&self.inner);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Some(client) = inner.client.lock().await.take() {
                    client.close().await;
                }
            });
        }
    }
}

/// Turns CDP traffic into sink callbacks for one view.
struct Pump {
    inner: Arc<Inner>,
    client: Arc<CdpClient>,
}

impl Pump {
    async fn run(self, mut events: broadcast::Receiver<CdpEvent>) {
        loop {
            match events.recv().await {
                Ok(event) => self.handle(event).await,
                // The view cannot render every frame if the sink is slow;
                // dropping late ones keeps the live picture moving.
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
        if !self.inner.detached.swap(true, Ordering::SeqCst) {
            self.inner.client.lock().await.take();
            self.inner
                .sink
                .status(ViewStatus::Failed, Some("cdp connection lost".to_string()));
        }
    }

    async fn handle(&self, event: CdpEvent) {
        match event.method.as_str() {
            "Target.targetCreated" | "Target.targetInfoChanged" => {
                let Some(info) = event.params.get("targetInfo") else {
                    return;
                };
                let Some((id, kind)) = info
                    .get("targetId")
                    .and_then(Value::as_str)
                    .zip(info.get("type").and_then(Value::as_str))
                else {
                    return;
                };
                if kind != "page" {
                    return;
                }
                let url = info
                    .get("url")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let title = info
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();

                let mut targets = self.inner.targets.lock().await;
                match targets.get(id) {
                    // Same target, same page: nothing moved.
                    Some(known) if known.url == url && known.title == title => return,
                    // A known target whose URL/title moved is an incremental
                    // update; the tab strip already has the row.
                    Some(_) => {
                        targets.insert(
                            id.to_string(),
                            PageTarget {
                                target_id: id.to_string(),
                                url: url.clone(),
                                title: title.clone(),
                                r#type: "page".to_string(),
                            },
                        );
                        drop(targets);
                        self.inner.sink.meta(id, Some(url), Some(title));
                    }
                    // A target the set has not seen yet changes the set.
                    None => {
                        targets.insert(
                            id.to_string(),
                            PageTarget {
                                target_id: id.to_string(),
                                url: url.clone(),
                                title: title.clone(),
                                r#type: "page".to_string(),
                            },
                        );
                        drop(targets);
                        self.push_targets().await;
                    }
                }
            }
            "Target.targetDestroyed" => {
                let Some(id) = event.params.get("targetId").and_then(Value::as_str) else {
                    return;
                };
                let removed = self.inner.targets.lock().await.remove(id).is_some();
                if removed {
                    self.push_targets().await;
                }
            }
            "Page.screencastFrame" => {
                let (target_id, session_id) = {
                    let focus = self.inner.focus.lock().await;
                    match focus.as_ref() {
                        Some(focused)
                            if event.session_id.as_deref() == Some(focused.session_id.as_str()) =>
                        {
                            (focused.target_id.clone(), focused.session_id.clone())
                        }
                        // A late frame for a target that stopped streaming:
                        // its session may already be detached, so no ack.
                        _ => return,
                    }
                };
                let seq = self.inner.seq.fetch_add(1, Ordering::Relaxed);
                let data = event
                    .params
                    .get("data")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                self.inner.sink.frame(&target_id, data, seq);
                // The browser stalls screencast until every frame is acked.
                let ack = json!({
                    "sessionId": event.params.get("sessionId").cloned().unwrap_or(Value::Null),
                });
                let _ = self
                    .client
                    .call("Page.screencastFrameAck", ack, Some(&session_id))
                    .await;
            }
            _ => {}
        }
    }

    async fn push_targets(&self) {
        let targets = self
            .inner
            .targets
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        self.inner.sink.targets(targets);
    }
}

fn device_metrics(width: u32, height: u32) -> Value {
    json!({
        "width": width,
        "height": height,
        "deviceScaleFactor": 1,
        "mobile": false,
    })
}

fn screencast_params(max_width: u32, max_height: u32) -> Value {
    json!({
        "format": "jpeg",
        "quality": 60,
        "maxWidth": max_width,
        "maxHeight": max_height,
    })
}

/// Logical pixels become whole CSS pixels; a degenerate size is one pixel.
fn round_pixels(value: f64) -> u32 {
    value.round().max(1.0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use futures::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    type Log = Arc<StdMutex<Vec<Value>>>;

    struct FakeCdp {
        port: u16,
        log: Log,
        push: broadcast::Sender<Value>,
    }

    impl FakeCdp {
        fn endpoint(&self) -> String {
            format!(
                "ws://127.0.0.1:{}/devtools/browser/test-browser",
                self.port
            )
        }

        /// Every frame the client sent, in order.
        fn record(&self) -> Vec<Value> {
            self.log.lock().unwrap().clone()
        }

        /// Send a raw frame to the live connection, like a browser event.
        fn push(&self, frame: Value) {
            let _ = self.push.send(frame);
        }
    }

    /// A scripted CDP peer that answers every command with `responder`'s body
    /// (merged next to the request id).
    fn serve<F>(responder: F) -> Arc<FakeCdp>
    where
        F: Fn(&Value, &broadcast::Sender<Value>) -> Value + Send + Sync + 'static,
    {
        let log: Log = Arc::new(StdMutex::new(Vec::new()));
        let (push, _) = broadcast::channel(64);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let listener = TcpListener::from_std(listener).unwrap();
        let responder = Arc::new(responder);
        let accept_log = Arc::clone(&log);
        let accept_push = push.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let log = Arc::clone(&accept_log);
                let mut push_rx = accept_push.subscribe();
                let push_tx = accept_push.clone();
                let responder = Arc::clone(&responder);
                tokio::spawn(async move {
                    let Ok(websocket) = tokio_tungstenite::accept_async(stream).await else {
                        return;
                    };
                    let mut websocket = websocket;
                    loop {
                        tokio::select! {
                            incoming = websocket.next() => {
                                let Some(Ok(message)) = incoming else { break };
                                let Message::Text(text) = message else { continue };
                                let Ok(frame) = serde_json::from_str::<Value>(&text) else { continue };
                                log.lock().unwrap().push(frame.clone());
                                if frame.get("id").is_some() {
                                    let mut reply = json!({ "id": frame["id"] });
                                    if let Some(body) = responder(&frame, &push_tx).as_object() {
                                        for (key, value) in body {
                                            reply[key] = value.clone();
                                        }
                                    }
                                    let _ = websocket.send(Message::Text(reply.to_string().into())).await;
                                }
                            }
                            pushed = push_rx.recv() => {
                                match pushed {
                                    Ok(frame) => {
                                        let _ = websocket.send(Message::Text(frame.to_string().into())).await;
                                    }
                                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                                    Err(broadcast::error::RecvError::Closed) => break,
                                }
                            }
                        }
                    }
                });
            }
        });
        Arc::new(FakeCdp { port, log, push })
    }

    /// Poll `check` until true, with a bounded total wait.
    async fn wait_until(what: &str, check: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if check() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
    }

    #[derive(Default)]
    struct TestSink {
        statuses: StdMutex<Vec<(ViewStatus, Option<String>)>>,
        targets: StdMutex<Vec<Vec<PageTarget>>>,
        metas: StdMutex<Vec<(String, Option<String>, Option<String>)>>,
        frames: StdMutex<Vec<(String, String, u64)>>,
    }

    impl TestSink {
        fn frames(&self) -> Vec<(String, String, u64)> {
            self.frames.lock().unwrap().clone()
        }

        fn targets_seen(&self, target_ids: &[&str]) -> bool {
            self.targets.lock().unwrap().iter().any(|pushed| {
                pushed.len() == target_ids.len()
                    && pushed.iter().zip(target_ids).all(|(target, id)| target.target_id == *id)
            })
        }
    }

    impl BrowserViewSink for TestSink {
        fn status(&self, status: ViewStatus, detail: Option<String>) {
            self.statuses.lock().unwrap().push((status, detail));
        }

        fn targets(&self, targets: Vec<PageTarget>) {
            self.targets.lock().unwrap().push(targets);
        }

        fn meta(&self, target_id: &str, url: Option<String>, title: Option<String>) {
            self.metas
                .lock()
                .unwrap()
                .push((target_id.to_string(), url, title));
        }

        fn frame(&self, target_id: &str, jpeg_base64: &str, seq: u64) {
            self.frames
                .lock()
                .unwrap()
                .push((target_id.to_string(), jpeg_base64.to_string(), seq));
        }
    }

    fn last_with_method<'a>(log: &'a [Value], method: &str) -> Option<&'a Value> {
        log.iter()
            .rev()
            .find(|frame| frame["method"] == method)
    }

    fn all_with_method<'a>(log: &'a [Value], method: &str) -> Vec<&'a Value> {
        log.iter()
            .filter(|frame| frame["method"] == method)
            .collect()
    }

    #[tokio::test]
    async fn attach_focus_frames_input_and_page_lifecycle() {
        let fake = serve(|frame, push| {
            match frame["method"].as_str().unwrap_or("") {
                // The browser answers discovery with the pages it already has.
                "Target.setDiscoverTargets" => {
                    for info in [
                        json!({ "targetId": "t-1", "type": "page",
                                "url": "https://example.com/one", "title": "One" }),
                        json!({ "targetId": "t-2", "type": "page",
                                "url": "https://example.com/two", "title": "Two" }),
                        json!({ "targetId": "sw-1", "type": "service_worker",
                                "url": "https://example.com/sw", "title": "" }),
                    ] {
                        let _ = push.send(json!({
                            "method": "Target.targetCreated",
                            "params": { "targetInfo": info },
                        }));
                    }
                    json!({ "result": {} })
                }
                "Target.attachToTarget" => json!({ "result": { "sessionId": "s-1" } }),
                "Target.createTarget" => {
                    let _ = push.send(json!({
                        "method": "Target.targetCreated",
                        "params": { "targetInfo": {
                            "targetId": "t-3", "type": "page",
                            "url": frame["params"]["url"], "title": "Three",
                        } },
                    }));
                    json!({ "result": { "targetId": "t-3" } })
                }
                "Target.closeTarget" => {
                    let _ = push.send(json!({
                        "method": "Target.targetDestroyed",
                        "params": { "targetId": "t-3" },
                    }));
                    json!({ "result": { "success": true } })
                }
                _ => json!({ "result": { "success": true } }),
            }
        });
        let sink = Arc::new(TestSink::default());

        let view = BrowserView::attach(&fake.endpoint(), sink.clone())
            .await
            .unwrap();

        // attach: discovery on, full page list out, status live. The
        // service_worker target must not appear.
        wait_until("the page targets to arrive", || {
            sink.targets_seen(&["t-1", "t-2"])
        })
        .await;
        {
            let statuses = sink.statuses.lock().unwrap();
            assert_eq!(statuses[0].0, ViewStatus::Connecting);
            assert_eq!(statuses.last().unwrap().0, ViewStatus::Live);
        }
        {
            let log = fake.record();
            let discovery = last_with_method(&log, "Target.setDiscoverTargets").unwrap();
            assert_eq!(discovery["params"], json!({ "discover": true }));
            assert!(discovery.get("sessionId").is_none());
        }

        // A URL/title move is incremental meta, not a list replacement.
        fake.push(json!({
            "method": "Target.targetInfoChanged",
            "params": { "targetInfo": {
                "targetId": "t-1", "type": "page",
                "url": "https://example.com/one#moved", "title": "One moved",
            } },
        }));
        wait_until("the meta update", || {
            sink.metas.lock().unwrap().iter().any(|(id, url, title)| {
                id == "t-1"
                    && url.as_deref() == Some("https://example.com/one#moved")
                    && title.as_deref() == Some("One moved")
            })
        })
        .await;

        // focus: attach flattened, page enabled, cast started.
        view.focus("t-1").await.unwrap();
        {
            let log = fake.record();
            let attach = last_with_method(&log, "Target.attachToTarget").unwrap();
            assert_eq!(
                attach["params"],
                json!({ "targetId": "t-1", "flatten": true })
            );
            assert!(attach.get("sessionId").is_none());
            let enable = last_with_method(&log, "Page.enable").unwrap();
            assert_eq!(enable["sessionId"], "s-1");
            let cast = last_with_method(&log, "Page.startScreencast").unwrap();
            assert_eq!(cast["sessionId"], "s-1");
            assert_eq!(
                cast["params"],
                json!({ "format": "jpeg", "quality": 60,
                        "maxWidth": DEFAULT_MAX_WIDTH, "maxHeight": DEFAULT_MAX_HEIGHT })
            );
        }

        // A frame reaches the sink and is acked on the cast session.
        fake.push(json!({
            "method": "Page.screencastFrame",
            "sessionId": "s-1",
            "params": { "data": "RkFNRTE=", "sessionId": 7, "metadata": {} },
        }));
        wait_until("the screencast frame", || !sink.frames().is_empty()).await;
        assert_eq!(sink.frames(), vec![("t-1".to_string(), "RkFNRTE=".to_string(), 0)]);
        wait_until("the frame ack", || {
            fake.record()
                .iter()
                .any(|frame| frame["method"] == "Page.screencastFrameAck")
        })
        .await;
        {
            let log = fake.record();
            let ack = last_with_method(&log, "Page.screencastFrameAck").unwrap();
            assert_eq!(ack["sessionId"], "s-1");
            assert_eq!(ack["params"], json!({ "sessionId": 7 }));
        }

        // Input maps onto the dispatch commands of the focused session.
        view.input("t-1", &ViewInputEvent::Mouse {
            r#type: "mousePressed".to_string(),
            x: 10.5,
            y: 20.5,
            button: "left".to_string(),
            click_count: 1,
            delta_x: 0.0,
            delta_y: 0.0,
            modifiers: 2,
        })
        .await
        .unwrap();
        view.input("t-1", &ViewInputEvent::Key {
            r#type: "keyDown".to_string(),
            key: "a".to_string(),
            code: "KeyA".to_string(),
            text: "a".to_string(),
            modifiers: 0,
        })
        .await
        .unwrap();
        view.input("t-1", &ViewInputEvent::Text { text: "hello".to_string() })
            .await
            .unwrap();
        {
            let log = fake.record();
            let mouse = last_with_method(&log, "Input.dispatchMouseEvent").unwrap();
            assert_eq!(mouse["sessionId"], "s-1");
            assert_eq!(
                mouse["params"],
                json!({ "type": "mousePressed", "x": 10.5, "y": 20.5, "button": "left",
                        "clickCount": 1, "deltaX": 0.0, "deltaY": 0.0, "modifiers": 2 })
            );
            let key = last_with_method(&log, "Input.dispatchKeyEvent").unwrap();
            assert_eq!(key["sessionId"], "s-1");
            assert_eq!(
                key["params"],
                json!({ "type": "keyDown", "key": "a", "code": "KeyA",
                        "text": "a", "modifiers": 0 })
            );
            let insert = last_with_method(&log, "Input.insertText").unwrap();
            assert_eq!(insert["sessionId"], "s-1");
            assert_eq!(insert["params"], json!({ "text": "hello" }));
        }

        // set_size: rounded viewport override, then a restarted cast.
        view.set_size("t-1", 800.4, 600.6).await.unwrap();
        {
            let log = fake.record();
            let metrics = last_with_method(&log, "Emulation.setDeviceMetricsOverride").unwrap();
            assert_eq!(metrics["sessionId"], "s-1");
            assert_eq!(
                metrics["params"],
                json!({ "width": 800, "height": 601,
                        "deviceScaleFactor": 1, "mobile": false })
            );
            let casts = all_with_method(&log, "Page.startScreencast");
            assert!(
                casts.len() >= 2,
                "the cast must be restarted, got {casts:?}"
            );
            let restart = casts.last().unwrap();
            assert_eq!(restart["sessionId"], "s-1");
            assert_eq!(
                restart["params"],
                json!({ "format": "jpeg", "quality": 60,
                        "maxWidth": 800, "maxHeight": 601 })
            );
            let stops = all_with_method(&log, "Page.stopScreencast");
            assert!(
                stops.iter().any(|stop| stop["sessionId"] == "s-1"),
                "the old cast must be stopped, got {stops:?}"
            );
        }

        // open_page/close_page with the target list following along.
        let opened = view.open_page("https://example.com/new").await.unwrap();
        assert_eq!(opened, "t-3");
        wait_until("the opened page in the targets", || {
            sink.targets_seen(&["t-1", "t-2", "t-3"])
        })
        .await;
        view.close_page("t-3").await.unwrap();
        wait_until("the closed page to leave the targets", || {
            sink.targets_seen(&["t-1", "t-2"])
        })
        .await;
        {
            let log = fake.record();
            let create = last_with_method(&log, "Target.createTarget").unwrap();
            assert_eq!(create["params"], json!({ "url": "https://example.com/new" }));
            assert!(create.get("sessionId").is_none());
            let close = last_with_method(&log, "Target.closeTarget").unwrap();
            assert_eq!(close["params"], json!({ "targetId": "t-3" }));
        }

        // detach: the cast session is torn down and the status goes closed.
        view.detach().await;
        {
            let log = fake.record();
            let detach = last_with_method(&log, "Target.detachFromTarget").unwrap();
            assert_eq!(detach["params"], json!({ "sessionId": "s-1" }));
            assert_eq!(all_with_method(&log, "Page.stopScreencast").len(), 2);
        }
        {
            let statuses = sink.statuses.lock().unwrap();
            assert_eq!(statuses.last().unwrap().0, ViewStatus::Closed);
        }
        // Idempotent, and the view is inert afterwards.
        view.detach().await;
        assert_eq!(sink.statuses.lock().unwrap().len(), 3);
        assert!(matches!(
            view.input("t-1", &ViewInputEvent::Text { text: "x".to_string() }).await,
            Err(CdpError::Disconnected)
        ));
    }

    #[tokio::test]
    async fn input_to_an_unfocused_target_is_refused() {
        let fake = serve(|_, _| json!({ "result": { "success": true } }));
        let sink = Arc::new(TestSink::default());
        let view = BrowserView::attach(&fake.endpoint(), sink.clone())
            .await
            .unwrap();

        let error = view
            .input("t-2", &ViewInputEvent::Text { text: "typed".to_string() })
            .await
            .expect_err("input must not reach a target that is not shown");
        assert!(
            matches!(&error, CdpError::Protocol { message, .. } if message.contains("not the focused")),
            "got {error}"
        );

        view.detach().await;
    }

    #[tokio::test]
    async fn attach_reports_failure_when_the_endpoint_is_unreachable() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let sink = Arc::new(TestSink::default());
        let endpoint = format!("ws://127.0.0.1:{port}/devtools/browser/gone");
        let error = BrowserView::attach(&endpoint, sink.clone())
            .await
            .err()
            .expect("a dead endpoint must not attach");
        assert!(matches!(error, CdpError::Connect(_)));

        let statuses = sink.statuses.lock().unwrap();
        assert_eq!(statuses[0].0, ViewStatus::Connecting);
        assert_eq!(statuses.last().unwrap().0, ViewStatus::Failed);
    }

    #[test]
    fn input_events_serialize_with_their_kind_tag() {
        let event = ViewInputEvent::Mouse {
            r#type: "mouseMoved".to_string(),
            x: 1.0,
            y: 2.0,
            button: "none".to_string(),
            click_count: 0,
            delta_x: 0.0,
            delta_y: 3.0,
            modifiers: 1,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["kind"], "mouse");
        assert_eq!(value["type"], "mouseMoved");
        // The wire contract spells parameters snake_case; the CDP mapping is
        // where camelCase comes back.
        assert_eq!(value["delta_y"], 3.0);
        assert_eq!(value["click_count"], 0);

        let key = ViewInputEvent::Key {
            r#type: "char".to_string(),
            key: "a".to_string(),
            code: "KeyA".to_string(),
            text: "a".to_string(),
            modifiers: 0,
        };
        let value = serde_json::to_value(&key).unwrap();
        assert_eq!(value["kind"], "key");

        let round_trip: ViewInputEvent =
            serde_json::from_value(serde_json::json!({ "kind": "text", "text": "hi" })).unwrap();
        assert!(matches!(round_trip, ViewInputEvent::Text { text } if text == "hi"));
    }
}
