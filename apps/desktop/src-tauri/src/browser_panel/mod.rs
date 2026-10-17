//! The right panel's browser.
//!
//! One surface on every desktop platform: a child webview of the app window, one
//! per tab, placed on the slot the frontend measures. WebView2 on Windows,
//! WKWebView on macOS — the same `browser_panel_*` commands and `browser_panel:*`
//! events behind it, and the same native rendering, scrolling, caret and input
//! method, which is what the panel is for (see
//! `docs/browser-view-subsystem.md`).
//!
//! On Windows the webview is started with a DevTools port, which makes the panel
//! the browser the agent's tools drive: the page the user reads and the page the
//! agent acts on are one page. WKWebView speaks no CDP, so on macOS the panel is
//! a viewer and the tools drive the browser their settings name. Nothing else
//! about a tab differs by platform.
//!
//! A browser exists exactly while a tab does: the first link click is what starts
//! one, and the last tab closed is what ends it. Launching the app never puts a
//! browser on screen. Windows additionally keeps one spare blank page — not a
//! tab, never shown — so the agent's tools always have a page to act on and an
//! endpoint to attach to; the endpoint is published to the app
//! (`app_core::panel_browser`) once it answers DevTools.

#[cfg(windows)]
use std::net::TcpListener;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Url};

pub(super) mod driver;
#[cfg(any(windows, target_os = "macos"))]
mod webview_surface;

pub use driver::TauriPanelDriver;

/// Where a webview waits before the panel says where its slot is.
///
/// A newly created child webview paints immediately, and creating it over the
/// app UI would flash a page across whatever the user was looking at. It is
/// created far outside the window and moved in when its tab is shown.
const OFFSCREEN: f64 = -20_000.0;

/// The panel slot, in logical pixels relative to the window's client area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Slot {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) width: f64,
    pub(super) height: f64,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            x: OFFSCREEN,
            y: OFFSCREEN,
            width: 1024.0,
            height: 768.0,
        }
    }
}

impl Slot {
    /// Whether the panel has room to show a page. The UI reports a collapsed
    /// slot (the panel is hidden or mid-layout) as a zero-sized rect, and a
    /// surface given one would reflow the page inside it to nothing.
    pub(super) fn has_area(&self) -> bool {
        self.width >= 1.0 && self.height >= 1.0
    }
}

/// One panel tab, as the UI sees it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PanelTab {
    pub tab_id: String,
    pub url: String,
    pub title: String,
}

/// What the panel needs to rebuild its tab strip after a remount.
#[derive(Clone, Debug, Serialize)]
pub struct PanelStateReport {
    pub tabs: Vec<PanelTab>,
    pub active: Option<String>,
    /// The DevTools endpoint the agent's tools are actually pointed at.
    ///
    /// Not simply the port the browser was started with: the endpoint is
    /// published only once the browser answers DevTools, and until then the
    /// tools are still on whatever settings named. `None` therefore means
    /// "the agent is not driving this panel" — which on macOS is the standing
    /// answer, because a WKWebView has no DevTools endpoint to publish.
    pub endpoint: Option<String>,
}

#[derive(Clone, Serialize)]
struct TabsReport {
    tabs: Vec<PanelTab>,
    active: Option<String>,
}

#[derive(Clone, Serialize)]
struct PageReport {
    tab_id: String,
    url: String,
    /// `started` or `finished`, the two halves of a page load.
    event: String,
}

/// One half of a page load, as the panel driver hears it.
///
/// The webview callback fires these on the main thread; the driver awaits
/// them on the async runtime, so they travel over a broadcast channel rather
/// than the host lock.
#[derive(Clone, Debug)]
pub(super) struct PageEvent {
    pub tab_id: String,
    /// `started` or `finished`.
    pub event: &'static str,
}

/// How old a "this tab is loading" record may become before it is read as
/// stale. Comfortably longer than the driver's navigation timeout: a real
/// load that takes this long is already being treated as finished by every
/// waiter, so the record has no one left to inform.
const LOAD_STALE: Duration = Duration::from_secs(30);

/// What shows a tab's page: a child webview of the app window. The host only
/// ever asks it to place, navigate and close itself.
pub(super) enum Surface {
    #[cfg(any(windows, target_os = "macos"))]
    Webview(tauri::Webview),
    /// Platforms where the panel browser is not supported yet: commands fail
    /// with an explanation instead of an unimplemented crash.
    #[cfg(not(any(windows, target_os = "macos")))]
    Inert,
}

impl Surface {
    /// Put the page at the slot (`Some`) or take it off screen (`None`).
    pub(super) fn place(&self, slot: Option<Slot>) {
        match self {
            #[cfg(any(windows, target_os = "macos"))]
            Surface::Webview(webview) => webview_surface::place(webview, slot),
            #[cfg(not(any(windows, target_os = "macos")))]
            Surface::Inert => {
                let _ = slot;
            }
        }
    }

    pub(super) async fn navigate(&self, url: &Url) -> Result<(), String> {
        match self {
            #[cfg(any(windows, target_os = "macos"))]
            Surface::Webview(webview) => webview
                .navigate(url.clone())
                .map_err(|error| error.to_string()),
            #[cfg(not(any(windows, target_os = "macos")))]
            Surface::Inert => {
                let _ = url;
                Err(UNSUPPORTED.to_string())
            }
        }
    }

    pub(super) async fn current_url(&self) -> Result<Url, String> {
        match self {
            #[cfg(any(windows, target_os = "macos"))]
            Surface::Webview(webview) => webview
                .url()
                .map_err(|error| format!("无法读取当前网址:{error}")),
            #[cfg(not(any(windows, target_os = "macos")))]
            Surface::Inert => Err(UNSUPPORTED.to_string()),
        }
    }

    pub(super) async fn eval(&self, js: &str) -> Result<(), String> {
        match self {
            #[cfg(any(windows, target_os = "macos"))]
            Surface::Webview(webview) => webview.eval(js).map_err(|error| error.to_string()),
            #[cfg(not(any(windows, target_os = "macos")))]
            Surface::Inert => {
                let _ = js;
                Err(UNSUPPORTED.to_string())
            }
        }
    }

    /// The page is gone for good: close the webview behind it.
    pub(super) async fn close(&self) -> Result<(), String> {
        match self {
            #[cfg(any(windows, target_os = "macos"))]
            Surface::Webview(webview) => {
                let _ = webview.close();
                Ok(())
            }
            #[cfg(not(any(windows, target_os = "macos")))]
            Surface::Inert => Ok(()),
        }
    }

    /// The raw webview, for the panel driver's page evaluations.
    #[cfg(any(windows, target_os = "macos"))]
    pub(super) fn webview(&self) -> Option<&tauri::Webview> {
        match self {
            Surface::Webview(webview) => Some(webview),
        }
    }
}

/// What a platform without a panel browser is told when it asks for one.
#[cfg(not(any(windows, target_os = "macos")))]
const UNSUPPORTED: &str = "此平台暂不支持面板浏览器";

pub(super) struct Tab {
    pub(super) info: PanelTab,
    pub(super) surface: Surface,
}

pub(super) struct Inner {
    /// The DevTools port the profile's browser process is started with. It is
    /// fixed for the life of the environment, so it is chosen once. Only a
    /// webview with a DevTools endpoint asks for one, so on macOS there is no
    /// port and nothing is ever published.
    #[cfg(windows)]
    pub(super) port: Option<u16>,
    pub(super) next_id: u64,
    pub(super) tabs: Vec<Tab>,
    pub(super) active: Option<String>,
    pub(super) slot: Slot,
    /// Whether the panel is currently showing a web tab at all.
    pub(super) shown: bool,
    /// Windows only: the panel's spare blank page, while one exists. It is not
    /// a tab — it becomes one the first time a real page lands in it.
    #[cfg(windows)]
    pub(super) warm: Option<Surface>,
    /// Whether a spare page is being created right now. Creating the surface
    /// runs outside this lock, so two callers racing for the first one need a
    /// flag rather than the lock to tell them apart.
    #[cfg(windows)]
    pub(super) warming: bool,
    /// Tabs whose page is mid-load, and when that load started. The timestamp
    /// is what keeps a load whose `finished` never arrived — a cancelled or
    /// failed navigation, which WKWebView does not always pair — from marking
    /// its tab as loading forever: [`BrowserPanelHost::is_loading`] expires
    /// entries older than [`LOAD_STALE`].
    pub(super) loading: HashMap<String, Instant>,
}

/// The panel's tabs and their pages, one browser per app.
pub struct BrowserPanelHost {
    /// Shared with the webview callbacks: they run on the main thread while the
    /// commands run on the async runtime, and both must see the same state.
    pub(super) inner: Arc<Mutex<Inner>>,
    /// Page-load halves, broadcast for the panel driver's settle waits.
    pub(super) page_events: tokio::sync::broadcast::Sender<PageEvent>,
    /// In-flight page evaluations: call id → the caller waiting for the
    /// page's answer (which arrives through `browser_panel_eval_result`).
    pub(super) eval_results: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>>>,
}

impl Default for BrowserPanelHost {
    fn default() -> Self {
        let (page_events, _) = tokio::sync::broadcast::channel(64);
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            page_events,
            eval_results: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            next_id: 0,
            tabs: Vec::new(),
            active: None,
            slot: Slot::default(),
            shown: false,
            #[cfg(windows)]
            port: None,
            #[cfg(windows)]
            warm: None,
            #[cfg(windows)]
            warming: false,
            loading: HashMap::new(),
        }
    }
}

impl Inner {
    /// The id the next tab gets. It is also the webview's label, so events can
    /// name their tab without a lookup table.
    pub(super) fn allocate_id(&mut self) -> String {
        self.next_id += 1;
        format!("tab-{}", self.next_id)
    }

    /// Whether `label` is the panel's unused blank page.
    #[cfg(windows)]
    pub(super) fn is_warm(&self, label: &str) -> bool {
        self.warm.as_ref().is_some_and(|warm| {
            matches!(warm, Surface::Webview(webview) if webview.label() == label)
        })
    }
}

/// The DevTools endpoint for a port, in the shape a CDP client connects to.
#[cfg(windows)]
fn endpoint_for(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// Turn what the user typed into a URL a browser can load.
///
/// `localhost:5173` parses as a URL whose scheme is `localhost`, which no
/// browser can fetch; only schemes that name a way to get the page are taken
/// as they are, and everything else is read as a host.
pub(super) fn normalize_url(raw: &str) -> Result<Url, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("请输入网址".to_string());
    }
    if let Ok(url) = Url::parse(trimmed) {
        if matches!(
            url.scheme(),
            "http" | "https" | "about" | "file" | "data" | "blob"
        ) {
            return Ok(url);
        }
    }
    Url::parse(&format!("https://{trimmed}"))
        .map_err(|error| format!("这不是一个网址:{trimmed}({error})"))
}

/// The panel tab id behind a webview label — labels are the tab ids.
pub(super) fn tab_id_of(label: &str) -> String {
    label.to_string()
}

/// The window the panel's pages belong to.
pub(super) fn main_window(app: &AppHandle) -> Result<tauri::Window, String> {
    app.get_window("main")
        .or_else(|| {
            app.get_webview_window("main")
                .map(|w| w.as_ref().window().clone())
        })
        .ok_or_else(|| "主窗口不存在".to_string())
}

impl BrowserPanelHost {
    /// Pick the DevTools port the first time the browser starts, and remember
    /// it.
    #[cfg(windows)]
    fn port(&self) -> Result<u16, String> {
        let mut inner = self.inner.lock().map_err(|_| poisoned())?;
        if let Some(port) = inner.port {
            return Ok(port);
        }
        // Bound to 0 and released: the port only has to be free at this
        // moment, and the browser takes it immediately after.
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|error| format!("无法为面板浏览器分配端口:{error}"))?;
        let port = listener
            .local_addr()
            .map_err(|error| format!("无法为面板浏览器分配端口:{error}"))?
            .port();
        drop(listener);
        inner.port = Some(port);
        Ok(port)
    }

    fn report(&self) -> Result<PanelStateReport, String> {
        let inner = self.inner.lock().map_err(|_| poisoned())?;
        Ok(PanelStateReport {
            tabs: inner.tabs.iter().map(|tab| tab.info.clone()).collect(),
            active: inner.active.clone(),
            // Read from the published slot rather than from `inner.port`: the
            // two differ exactly when the browser never answered DevTools, and
            // this field is what a reader trusts to know whether the agent is
            // on the panel.
            endpoint: app_core::panel_browser::endpoint(),
        })
    }

    fn tab(&self, tab_id: &str) -> Result<Surface, String> {
        let inner = self.inner.lock().map_err(|_| poisoned())?;
        inner
            .tabs
            .iter()
            .find(|tab| tab.info.tab_id == tab_id)
            .map(|tab| clone_surface(&tab.surface))
            .ok_or_else(|| format!("面板里没有这个标签页:{tab_id}"))
    }

    /// The raw webview behind a tab, for the panel driver's page evaluations.
    #[cfg(any(windows, target_os = "macos"))]
    pub(super) fn webview_for(&self, tab_id: &str) -> Result<tauri::Webview, String> {
        let inner = self.inner.lock().map_err(|_| poisoned())?;
        inner
            .tabs
            .iter()
            .find(|tab| tab.info.tab_id == tab_id)
            .and_then(|tab| tab.surface.webview().cloned())
            .ok_or_else(|| format!("面板里没有这个标签页:{tab_id}"))
    }

    /// Register a caller waiting for a page's answer to an evaluation.
    pub(super) fn register_eval(
        &self,
        call_id: &str,
        sender: tokio::sync::oneshot::Sender<serde_json::Value>,
    ) -> Result<(), String> {
        self.eval_results
            .lock()
            .map_err(|_| poisoned())?
            .insert(call_id.to_string(), sender);
        Ok(())
    }

    /// Drop a pending evaluation without an answer (timeout, or the webview
    /// refused the script).
    pub(super) fn cancel_eval(&self, call_id: &str) {
        if let Ok(mut pending) = self.eval_results.lock() {
            pending.remove(call_id);
        }
    }

    /// The page's answer to an evaluation, arrived over the webview's IPC
    /// bridge. An unknown call id is a page answering late (the caller already
    /// timed out) or a page talking to itself; either way it is dropped.
    pub(super) fn complete_eval(&self, call_id: &str, payload: serde_json::Value) {
        let sender = self
            .eval_results
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(call_id));
        if let Some(sender) = sender {
            let _ = sender.send(payload);
        }
    }

    /// Whether a tab's page is mid-load right now.
    ///
    /// The `finished` half of a load is not guaranteed: a navigation cancelled
    /// by a newer one, a failed load, a download — WKWebView does not pair
    /// every `started` with a `finished`. An entry that old is therefore
    /// expired rather than believed, so one lost callback cannot make every
    /// later `wait_settled` wait out the full navigation timeout.
    pub(super) fn is_loading(&self, tab_id: &str) -> bool {
        self.inner
            .lock()
            .map(|mut inner| {
                let now = Instant::now();
                inner
                    .loading
                    .retain(|_, started| now.duration_since(*started) < LOAD_STALE);
                inner.loading.contains_key(tab_id)
            })
            .unwrap_or(false)
    }

    /// Put every tab at the slot, and show the active one if the panel is on a
    /// web tab. Positions are applied to the whole set: the tabs share one
    /// slot, so they stay in step whichever one is shown next.
    pub(super) fn place(&self) -> Result<(), String> {
        let (tabs, active, slot, shown) = {
            let inner = self.inner.lock().map_err(|_| poisoned())?;
            (
                inner
                    .tabs
                    .iter()
                    .map(|tab| (tab.info.tab_id.clone(), clone_surface(&tab.surface)))
                    .collect::<Vec<_>>(),
                inner.active.clone(),
                inner.slot,
                inner.shown,
            )
        };
        for (tab_id, surface) in &tabs {
            let visible = shown && slot.has_area() && active.as_deref() == Some(tab_id.as_str());
            surface.place(visible.then_some(slot));
        }
        Ok(())
    }

    fn emit_tabs(&self, app: &AppHandle) {
        if let Ok(inner) = self.inner.lock() {
            report_tabs(app, &inner);
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn clone_surface(surface: &Surface) -> Surface {
    match surface {
        Surface::Webview(webview) => Surface::Webview(webview.clone()),
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn clone_surface(surface: &Surface) -> Surface {
    match surface {
        Surface::Inert => Surface::Inert,
    }
}

/// Tell the UI what the panel's tabs are now.
///
/// The tab list is the only source of tab truth in the app, so every change —
/// a tab opened, closed, promoted from the spare page, retitled — ends here.
pub(super) fn report_tabs(app: &AppHandle, inner: &Inner) {
    let _ = app.emit(
        "browser_panel:tabs",
        TabsReport {
            tabs: inner.tabs.iter().map(|tab| tab.info.clone()).collect(),
            active: inner.active.clone(),
        },
    );
}

pub(super) fn poisoned() -> String {
    "面板浏览器状态已损坏".to_string()
}

/// How long the panel's browser is given to start answering DevTools, and how
/// often it is asked while it does.
#[cfg(windows)]
const PUBLISH_DEADLINE: Duration = Duration::from_secs(5);
#[cfg(windows)]
const PUBLISH_RETRY: Duration = Duration::from_millis(200);

/// Whether a probe says the tools have something to drive here.
///
/// A browser with no page on it is a browser the tools cannot use: the
/// provider connects, finds no tab to act on, and its first call fails.
/// Published anyway, it would take every call down with it instead of leaving
/// the tools on the browser settings named.
#[cfg(windows)]
fn has_a_page_to_drive(pages: usize) -> bool {
    pages > 0
}

/// Publish the panel's DevTools endpoint, once something is listening on it
/// and there is a page on the other end to drive.
///
/// The agent's browser tools take a published panel endpoint over everything
/// settings say — see `app_core::panel_browser` — so what is published has to
/// be usable. An endpoint put up too early would not leave the tools to the
/// browser they were configured with, it would take them down with it: every
/// call would fail with `connection refused`, and the browser the user can
/// see would be the one browser nobody could drive.
///
/// Retried, because the browser process is a moment behind the surface that
/// asked for it. The outcome is logged either way, so "the panel works but
/// the agent drives something else" is a line in `app.log` rather than a
/// mystery.
///
/// Safe to call again: it returns immediately once the endpoint is published,
/// so the callers may ask on every page the panel opens rather than only at
/// startup. A webview with no DevTools endpoint never asked for a port, so
/// there it returns without probing anything.
#[cfg(windows)]
async fn publish_drivable(panel: Arc<Mutex<Inner>>) {
    let endpoint = {
        let Ok(inner) = panel.lock() else {
            return;
        };
        inner.port.map(endpoint_for)
    };
    let Some(endpoint) = endpoint else {
        return;
    };

    // Already up. This is the common case, and the reason this can be called
    // again on every page the panel opens: after the first success it costs a
    // string compare, and before one it is the retry that lets a browser
    // which was merely slow to start still end up as the tools' browser.
    if app_core::panel_browser::endpoint().as_deref() == Some(endpoint.as_str()) {
        return;
    }

    let deadline = tokio::time::Instant::now() + PUBLISH_DEADLINE;
    loop {
        match app_core::panel_browser::probe(&endpoint).await {
            Ok(probe) if has_a_page_to_drive(probe.pages) => {
                app_core::panel_browser::publish(&endpoint);
                tracing::info!(
                    target: "browser_panel",
                    endpoint = %endpoint,
                    product = %probe.product,
                    pages = probe.pages,
                    "panel browser answers DevTools; the agent's tools drive it",
                );
                return;
            }
            // Reachable, with nothing on it to drive. Publishing anyway would
            // point the tools at a browser they cannot open a page in, so the
            // tools stay on their own browser and this line explains why the
            // panel is not being driven. The retries above and the callers
            // below are what turn this into a wait rather than a verdict: a
            // page the panel is actually showing answers with one.
            Ok(probe) => {
                tracing::warn!(
                    target: "browser_panel",
                    endpoint = %endpoint,
                    product = %probe.product,
                    "panel browser answers DevTools but lists no page; the agent's tools stay on their configured browser",
                );
                return;
            }
            Err(error) => {
                if tokio::time::Instant::now() >= deadline {
                    tracing::warn!(
                        target: "browser_panel",
                        endpoint = %endpoint,
                        error = %error,
                        "panel browser never answered DevTools; the agent's tools stay on their configured browser",
                    );
                    return;
                }
            }
        }
        tokio::time::sleep(PUBLISH_RETRY).await;
    }
}

/// Publishing is a Windows-only concern: a macOS webview was never given a
/// DevTools port, so there is nothing to probe and nothing to announce.
#[cfg(not(windows))]
async fn publish_drivable(_panel: Arc<Mutex<Inner>>) {}

/// Whether a URL is the blank page the panel keeps alive rather than a page
/// somebody asked for.
#[cfg(any(windows, test))]
fn is_blank(url: &str) -> bool {
    let url = url.trim();
    url == "about:blank" || url.starts_with("about:blank#")
}

/// The label for a page before its document has a title of its own.
pub(super) fn title_for(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| "新标签页".to_string())
}

/// Whether a page load should turn the panel's spare page into a tab: it has to
/// be the spare page, still unused by any tab, and it has to have arrived
/// somewhere real — another blank page is not something to show.
#[cfg(windows)]
fn should_promote(is_open_tab: bool, is_warm: bool, url: &str) -> bool {
    is_warm && !is_open_tab && !is_blank(url)
}

/// Give the panel's spare blank page a tab, now that a real page is loading
/// in it. Returns whether anything changed.
#[cfg(windows)]
fn promote(inner: &mut Inner, label: &str, url: &str) -> bool {
    let is_open_tab = inner.tabs.iter().any(|tab| tab.info.tab_id == label);
    if !should_promote(is_open_tab, inner.is_warm(label), url) {
        return false;
    }
    let Some(Surface::Webview(webview)) = inner.warm.take() else {
        return false;
    };
    let info = PanelTab {
        tab_id: label.to_string(),
        url: url.to_string(),
        title: title_for(url),
    };
    inner.tabs.push(Tab {
        info,
        surface: Surface::Webview(webview),
    });
    if inner.active.is_none() {
        inner.active = Some(label.to_string());
    }
    true
}

/// The panel's tabs as a command sees them.
#[tauri::command]
pub async fn browser_panel_state(
    host: tauri::State<'_, BrowserPanelHost>,
) -> Result<PanelStateReport, String> {
    host.report()
}

/// A page's answer to a panel-driver evaluation.
///
/// The script the driver injects reports its outcome through the webview's
/// own Tauri IPC bridge (every webview carries it, including remote pages).
/// This command is the receiving end; the waiting caller is woken with the
/// payload. It is not part of the panel's UI surface — nothing in the
/// frontend calls it.
#[tauri::command]
pub async fn browser_panel_eval_result(
    host: tauri::State<'_, BrowserPanelHost>,
    call_id: String,
    payload: serde_json::Value,
) -> Result<(), String> {
    host.complete_eval(&call_id, payload);
    Ok(())
}

/// Bring the panel's browser up before anyone needs it.
///
/// Only Windows has anything to warm. There the panel's webview is the browser
/// the agent's tools attach to, so it is started warm and kept with one spare
/// blank page: that spare is the page the tools find, and its endpoint is
/// published as soon as it answers DevTools. Everywhere else the panel is a
/// viewer, its browser is the app's own webview engine, and the first tab
/// starts it — so this is a no-op and says so.
///
/// Idempotent. Two callers can arrive at once — startup and a settings change,
/// or either and the first tab — and only one browser may come out of it; the
/// second sees `warming` and is told what there is so far, while the first
/// publishes the endpoint as soon as it has one.
pub async fn warm(app: &AppHandle, host: &BrowserPanelHost) -> Result<Option<String>, String> {
    #[cfg(windows)]
    {
        let label = {
            let mut inner = host.inner.lock().map_err(|_| poisoned())?;
            // Already up, already on its way, or not needed: a tab of its own
            // means the browser exists, and a spare page beside one would be a
            // page nobody asked for.
            if inner.warming || inner.warm.is_some() || !inner.tabs.is_empty() {
                return Ok(inner.port.map(endpoint_for));
            }
            inner.warming = true;
            inner.allocate_id()
        };

        let target = Url::parse("about:blank").map_err(|error| error.to_string())?;
        let created = webview_surface::create_webview(app, host, &label, target);

        // Scoped so the lock is gone before the browser is probed below: the
        // probe waits on the browser process, and the callbacks that process
        // runs take this same lock.
        let endpoint = {
            let mut inner = host.inner.lock().map_err(|_| poisoned())?;
            inner.warming = false;
            inner.warm = Some(created?);
            inner.port.map(endpoint_for)
        };
        tracing::info!(
            target: "browser_panel",
            endpoint = %endpoint.clone().unwrap_or_default(),
            "panel browser is warm",
        );
        publish_drivable(Arc::clone(&host.inner)).await;
        return Ok(endpoint);
    }
    #[cfg(not(windows))]
    {
        let _ = (app, host);
        Ok(None)
    }
}

/// Windows: the panel's spare blank page, if one exists.
///
/// It is the tab the next open is about to make: same browser, same profile,
/// already started, with the agent's tools possibly attached to it. Anything it
/// shows is replaced by what the user asked for. Everywhere else there is no
/// spare — the browser starts with the first tab and ends with the last.
#[cfg(windows)]
fn take_spare(host: &BrowserPanelHost) -> Result<Option<Surface>, String> {
    Ok(host.inner.lock().map_err(|_| poisoned())?.warm.take())
}

#[cfg(not(windows))]
fn take_spare(_host: &BrowserPanelHost) -> Result<Option<Surface>, String> {
    Ok(None)
}

/// Give the spare page back after a navigation onto it failed: it is still the
/// panel's only page, and losing it would leave the browser with nothing for
/// the agent to act on.
#[cfg(windows)]
fn give_spare(host: &BrowserPanelHost, surface: Surface) {
    if let Ok(mut inner) = host.inner.lock() {
        inner.warm = Some(surface);
    }
}

#[cfg(not(windows))]
fn give_spare(_host: &BrowserPanelHost, _surface: Surface) {}

/// Open a URL as a new panel tab and show it.
#[tauri::command]
pub async fn browser_panel_open(
    app: AppHandle,
    host: tauri::State<'_, BrowserPanelHost>,
    url: String,
) -> Result<PanelTab, String> {
    let target = normalize_url(&url)?;

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (&app, &host, target);
        return Err(UNSUPPORTED.to_string());
    }

    #[cfg(any(windows, target_os = "macos"))]
    {
        let webview = match take_spare(&host)? {
            Some(surface) => {
                let Surface::Webview(webview) = surface;
                if let Err(error) = webview.navigate(target.clone()) {
                    give_spare(&host, Surface::Webview(webview));
                    return Err(format!("无法打开 {url}:{error}"));
                }
                webview
            }
            None => {
                let label = {
                    let mut inner = host.inner.lock().map_err(|_| poisoned())?;
                    inner.allocate_id()
                };
                webview_surface::create_webview(&app, &host, &label, target.clone())?
            }
        };
        let tab_id = tab_id_of(webview.label());
        let info = PanelTab {
            tab_id: tab_id.clone(),
            url: target.to_string(),
            title: title_for(target.as_str()),
        };
        {
            let mut inner = host.inner.lock().map_err(|_| poisoned())?;
            inner.tabs.push(Tab {
                info: info.clone(),
                surface: Surface::Webview(webview),
            });
            if inner.active.is_none() {
                inner.active = Some(tab_id);
            }
        }
        finish_open(&app, &host);
        Ok(info)
    }
}

/// What every open path does once the tab exists: publishing is about the
/// agent's tools and must not hold the page up, and the strip needs the new
/// state.
fn finish_open(app: &AppHandle, host: &BrowserPanelHost) {
    tauri::async_runtime::spawn(publish_drivable(Arc::clone(&host.inner)));
    host.emit_tabs(app);
}

/// Close a panel tab. The page and its session go with it; the profile and
/// its logins stay.
#[tauri::command]
pub async fn browser_panel_close(
    app: AppHandle,
    host: tauri::State<'_, BrowserPanelHost>,
    tab_id: String,
) -> Result<(), String> {
    let last = {
        let mut inner = host.inner.lock().map_err(|_| poisoned())?;
        let index = inner
            .tabs
            .iter()
            .position(|tab| tab.info.tab_id == tab_id)
            .ok_or_else(|| format!("面板里没有这个标签页:{tab_id}"))?;
        let tab = inner.tabs.remove(index);
        if inner.active.as_deref() == Some(tab_id.as_str()) {
            inner.active = inner.tabs.last().map(|tab| tab.info.tab_id.clone());
        }
        (tab, inner.tabs.is_empty())
    };
    // Closed outside the lock: closing runs platform IO.
    last.0.surface.close().await?;
    host.emit_tabs(&app);
    host.place()?;
    if last.1 {
        // Windows: the browser tools are attached to this browser, so leaving
        // it with no page at all would leave them with nothing to act on, and
        // the next page they open would have nowhere to appear.
        #[cfg(windows)]
        warm(&app, &host).await?;
        // Everywhere else the browser went with its last page, which is the
        // right shape: it was only ever a viewer, and the next tab starts a
        // new one.
    }
    Ok(())
}

/// Show one tab and hide the others.
#[tauri::command]
pub async fn browser_panel_activate(
    app: AppHandle,
    host: tauri::State<'_, BrowserPanelHost>,
    tab_id: String,
) -> Result<(), String> {
    {
        let mut inner = host.inner.lock().map_err(|_| poisoned())?;
        if !inner.tabs.iter().any(|tab| tab.info.tab_id == tab_id) {
            return Err(format!("面板里没有这个标签页:{tab_id}"));
        }
        inner.active = Some(tab_id);
        inner.shown = true;
    }
    host.place()?;
    // The page is on screen for the first time. If the browser had nothing to
    // show when it was last asked, this is the moment it does — and the moment
    // the agent's tools can be pointed at it.
    tauri::async_runtime::spawn(publish_drivable(Arc::clone(&host.inner)));
    host.emit_tabs(&app);
    Ok(())
}

/// Hide the browser without closing anything: the panel is showing something
/// else, so the page must stop covering it.
#[tauri::command]
pub async fn browser_panel_hide(
    host: tauri::State<'_, BrowserPanelHost>,
) -> Result<(), String> {
    {
        let mut inner = host.inner.lock().map_err(|_| poisoned())?;
        inner.shown = false;
    }
    host.place()
}

/// Send a tab to a URL.
#[tauri::command]
pub async fn browser_panel_navigate(
    host: tauri::State<'_, BrowserPanelHost>,
    tab_id: String,
    url: String,
) -> Result<(), String> {
    let target = normalize_url(&url)?;
    host.tab(&tab_id)?
        .navigate(&target)
        .await
        .map_err(|error| format!("无法打开 {url}:{error}"))
}

/// Reload a tab.
#[tauri::command]
pub async fn browser_panel_reload(
    host: tauri::State<'_, BrowserPanelHost>,
    tab_id: String,
) -> Result<(), String> {
    let surface = host.tab(&tab_id)?;
    let url = surface.current_url().await?;
    surface
        .navigate(&url)
        .await
        .map_err(|error| format!("无法重新加载:{error}"))
}

/// Step back or forward in a tab's history.
#[tauri::command]
pub async fn browser_panel_history(
    host: tauri::State<'_, BrowserPanelHost>,
    tab_id: String,
    direction: String,
) -> Result<(), String> {
    let call = match direction.as_str() {
        "back" => "history.back()",
        "forward" => "history.forward()",
        other => return Err(format!("未知的历史方向:{other}")),
    };
    host.tab(&tab_id)?
        .eval(call)
        .await
        .map_err(|error| format!("无法切换历史:{error}"))
}

/// Where the panel's slot is now, in logical pixels. The webview is moved onto
/// it.
///
/// A zero-sized rect is taken as "the panel is not showing a page" — the panel
/// is collapsed or the layout is mid-flight — and hides the pages for as long
/// as it lasts: the page is painted above the app's own UI, so one left in
/// place would cover whatever the panel now shows.
#[tauri::command]
pub async fn browser_panel_bounds(
    host: tauri::State<'_, BrowserPanelHost>,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    if !(x.is_finite() && y.is_finite() && width.is_finite() && height.is_finite()) {
        return Err("面板尺寸无效".to_string());
    }
    {
        let mut inner = host.inner.lock().map_err(|_| poisoned())?;
        inner.slot = Slot {
            x,
            y,
            width,
            height,
        };
    }
    host.place()
}

#[cfg(test)]
mod tests {
    use super::{Slot, normalize_url, title_for};

    #[test]
    fn a_bare_host_is_read_as_https() {
        assert_eq!(
            normalize_url("example.com/a?b=1").unwrap().as_str(),
            "https://example.com/a?b=1"
        );
        // `localhost:5173` parses as a URL whose scheme is `localhost`; it is a
        // host, not a way to fetch a page.
        assert_eq!(
            normalize_url("localhost:5173").unwrap().as_str(),
            "https://localhost:5173/"
        );
    }

    #[test]
    fn real_schemes_are_left_alone() {
        for url in [
            "http://127.0.0.1:5173/x",
            "https://tauri.app/",
            "about:blank",
            "data:text/html,<h1>hi</h1>",
        ] {
            assert_eq!(normalize_url(url).unwrap().as_str(), url, "{url}");
        }
    }

    #[test]
    fn an_empty_url_is_refused() {
        assert!(normalize_url("   ").is_err());
    }

    #[test]
    fn a_tab_id_is_its_webview_label() {
        assert_eq!(super::tab_id_of("tab-7"), "tab-7");
    }

    #[test]
    fn a_collapsed_slot_has_no_room_for_a_page() {
        let collapsed = |width, height| Slot {
            x: 0.0,
            y: 0.0,
            width,
            height,
        };
        // The panel is hidden: `display: none` measures as an empty rect, and
        // that rect must not be pushed onto the page.
        assert!(!collapsed(0.0, 0.0).has_area());
        assert!(!collapsed(0.0, 800.0).has_area());
        assert!(!collapsed(420.0, 0.0).has_area());
        assert!(collapsed(420.0, 800.0).has_area());
    }

    #[test]
    fn a_waiting_page_is_never_where_a_page_is_shown() {
        // Where every webview is created: far outside the window, so the first
        // paint cannot flash a page across the app before the UI has said where
        // the panel is. Its size is a real one — it is the position, not the
        // size, that keeps it out of sight.
        let waiting = Slot::default();
        assert!(waiting.x.abs() > 1_000.0 && waiting.y.abs() > 1_000.0);
        assert!(waiting.width > 0.0 && waiting.height > 0.0);
    }

    #[test]
    fn a_page_is_labelled_by_its_host_until_it_has_a_title() {
        assert_eq!(title_for("https://example.com/a/b?c=1"), "example.com");
        assert_eq!(title_for("http://127.0.0.1:5173/"), "127.0.0.1");
        // No host to read: the strip says the same thing it says for a page the
        // user opened with the "+" button.
        assert_eq!(title_for("about:blank"), "新标签页");
        assert_eq!(title_for("not a url"), "新标签页");
    }

    #[cfg(windows)]
    mod windows_only {
        use super::super::{endpoint_for, has_a_page_to_drive, is_blank, should_promote};

        #[test]
        fn the_browser_is_started_with_the_port_the_agent_uses() {
            let args = super::super::webview_surface::browser_args(45678);
            // wry *replaces* its own defaults with this string rather than
            // adding to it, so a default lost here is a silent behaviour
            // change: the mini menu, the PDF toolbar and the smart-screen
            // overlay come back, and autoplay starts needing a gesture. Hence
            // the exact string.
            assert_eq!(
                args,
                concat!(
                    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection ",
                    "--autoplay-policy=no-user-gesture-required ",
                    "--remote-debugging-port=45678",
                )
            );
            assert_eq!(endpoint_for(45678), "http://127.0.0.1:45678");
        }

        #[test]
        fn a_browser_with_no_page_is_not_worth_publishing() {
            assert!(has_a_page_to_drive(1));
            assert!(has_a_page_to_drive(3));
            // Zero is the case that has to stay unpublished: with no page to
            // drive the tools would be pointed at a browser where every call
            // fails.
            assert!(!has_a_page_to_drive(0));
        }

        #[test]
        fn the_panels_own_blank_page_is_not_a_page() {
            assert!(is_blank("about:blank"));
            assert!(is_blank("  about:blank  "));
            assert!(!is_blank("about:srcdoc"));
            assert!(!is_blank("https://example.com/"));
            assert!(!is_blank("about:blank.example.com"));
        }

        #[test]
        fn the_agents_first_real_page_becomes_a_tab() {
            // The spare blank page arriving at a real URL: this is the agent
            // navigating, or a link landing in a page nobody has opened yet.
            assert!(should_promote(false, true, "https://example.com/"));
            // A page that is already a tab updates itself; it is not promoted
            // twice.
            assert!(!should_promote(true, true, "https://example.com/"));
            // Not the spare page: a real tab's own page has nothing to promote.
            assert!(!should_promote(false, false, "https://example.com/"));
            // Still blank: nothing to show, so it stays the spare page.
            assert!(!should_promote(false, true, "about:blank"));
        }
    }
}
