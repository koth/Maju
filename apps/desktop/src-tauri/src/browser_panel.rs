//! The right panel's browser: a real WebView2 webview per tab.
//!
//! The panel used to be a screencast of the session's headless Chromium: every
//! frame travelled to the webview as a base64 JPEG and every keystroke travelled
//! back over CDP. That made typing wait on a picture, and left the input method
//! aiming at a hidden field instead of the page, because a page shown as an
//! image cannot be typed into.
//!
//! This module puts the page itself in the panel instead. Each tab is a WebView2
//! child webview of the app window, in its own profile
//! (`~/.kodex/browser/panel`) so cookies and logins survive, rendering and the
//! input method are the platform's own, and the agent reaches the very same
//! browser through the DevTools port this profile is started with — no second
//! browser, nothing to keep alive, nothing to reconnect.
//!
//! That makes the panel's browser *the* browser: the page the user reads and
//! the page the agent acts on are one page. Two consequences shape this module.
//! The browser is kept alive with one unused blank page even while no tab is
//! open, so the agent always has somewhere to attach and a page it opens has
//! somewhere to appear — that page becomes a tab the first time a real URL
//! lands in it. And its endpoint is published to the app
//! (`app_core::panel_browser`) once it answers DevTools, which is what makes the
//! browser tools drive the panel instead of starting a browser of their own.
//!
//! See `docs/browser-view-subsystem.md`.

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Url, Webview, WebviewBuilder,
    WebviewUrl,
};

/// Where a webview waits before the panel says where its slot is.
///
/// A newly created child webview paints immediately, and creating it over the
/// app UI would flash a page across whatever the user was looking at. It is
/// created far outside the window and moved in when its tab is shown.
const OFFSCREEN: f64 = -20_000.0;

/// The panel slot, in logical pixels relative to the window's client area.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Slot {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
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
    /// webview given one would reflow the page inside it to nothing.
    fn has_area(&self) -> bool {
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
    /// "the agent is not driving this panel".
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
    /// `started` or `finished`, as CDP names the two halves of a load.
    event: String,
}

struct Tab {
    info: PanelTab,
    webview: Webview,
}

#[derive(Default)]
struct Inner {
    /// The DevTools port the profile's browser process is started with. It is
    /// fixed for the life of the environment, so it is chosen once.
    port: Option<u16>,
    next_id: u64,
    tabs: Vec<Tab>,
    active: Option<String>,
    slot: Slot,
    /// Whether the panel is currently showing a web tab at all.
    shown: bool,
    /// The panel's browser before any tab exists: one blank page, hidden, so
    /// the browser (and the agent's way into it) is up before anyone needs it.
    /// It is not a tab — it becomes one when a real page lands in it.
    warm: Option<Webview>,
    /// Whether a warm page is being created right now. Creating a webview runs
    /// on the main thread and cannot happen under this lock, so two callers
    /// racing for the first one need a flag rather than the lock to tell them
    /// apart.
    warming: bool,
}

/// The panel's tabs and their webviews, one browser per app.
#[derive(Default)]
pub struct BrowserPanelHost {
    /// Shared with the webview callbacks, which run on the main thread while
    /// the commands run on the async runtime and must see the same state.
    inner: Arc<Mutex<Inner>>,
}

impl Inner {
    /// The id the next webview gets. It is the webview's label, so events can
    /// name their tab without a lookup table.
    fn allocate_id(&mut self) -> String {
        self.next_id += 1;
        format!("tab-{}", self.next_id)
    }

    /// Whether `label` is the panel's unused blank page.
    fn is_warm(&self, label: &str) -> bool {
        self.warm
            .as_ref()
            .is_some_and(|webview| webview.label() == label)
    }
}

/// The DevTools endpoint for a port, in the shape a CDP client connects to.
fn endpoint_for(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// Where the panel's browser keeps its profile: its own logins, its own
/// cookies, kept across restarts, separate from the app and from any other
/// browser on the machine.
fn profile_dir() -> Result<PathBuf, String> {
    let paths = app_core::AppPaths::resolve().map_err(|error| error.to_string())?;
    Ok(paths.root().join("browser").join("panel"))
}

/// The switches the panel's webviews start with.
///
/// `--remote-debugging-port` is the agent's way in and the panel's reason for
/// owning a profile: this browser process is the one the tools drive, so the
/// page the user reads and the page the agent acts on are the same page.
///
/// The other two are wry's own defaults. `additional_browser_args` replaces
/// them rather than adding to them, so they are repeated here to keep the
/// panel from gaining the PDF toolbar, the smart-screen overlay and a
/// gesture requirement for autoplay.
fn browser_args(port: u16) -> String {
    format!(
        "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
         --autoplay-policy=no-user-gesture-required \
         --remote-debugging-port={port}"
    )
}

/// Turn what the user typed into a URL a browser can load.
///
/// `localhost:5173` parses as a URL whose scheme is `localhost`, which no
/// webview can fetch; only schemes that name a way to get the page are taken as
/// they are, and everything else is read as a host.
fn normalize_url(raw: &str) -> Result<Url, String> {
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
        .map_err(|error| format!("这不是一个网址：{trimmed}（{error}）"))
}

/// The panel tab id behind a webview label.
fn tab_id_of(label: &str) -> String {
    label.to_string()
}

/// The window the panel's webviews are children of.
fn main_window(app: &AppHandle) -> Result<tauri::Window, String> {
    app.get_window("main")
        .or_else(|| app.get_webview_window("main").map(|w| w.as_ref().window()))
        .ok_or_else(|| "主窗口不存在".to_string())
}

impl BrowserPanelHost {
    /// Pick the DevTools port the first time a tab is created, and remember it.
    fn port(&self) -> Result<u16, String> {
        let mut inner = self.inner.lock().map_err(|_| poisoned())?;
        if let Some(port) = inner.port {
            return Ok(port);
        }
        // Bound to 0 and released: the port only has to be free at this
        // moment, and the browser takes it immediately after.
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|error| format!("无法为面板浏览器分配端口：{error}"))?;
        let port = listener
            .local_addr()
            .map_err(|error| format!("无法为面板浏览器分配端口：{error}"))?
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

    fn tab(&self, tab_id: &str) -> Result<Webview, String> {
        let inner = self.inner.lock().map_err(|_| poisoned())?;
        inner
            .tabs
            .iter()
            .find(|tab| tab.info.tab_id == tab_id)
            .map(|tab| tab.webview.clone())
            .ok_or_else(|| format!("面板里没有这个标签页：{tab_id}"))
    }

    /// Put every tab at the slot, and show the active one if the panel is on a
    /// web tab. Positions are applied to the whole set: the tabs share one
    /// slot, so they stay in step whichever one is shown next.
    fn place(&self) -> Result<(), String> {
        let (tabs, active, slot, shown) = {
            let inner = self.inner.lock().map_err(|_| poisoned())?;
            (
                inner
                    .tabs
                    .iter()
                    .map(|tab| (tab.info.tab_id.clone(), tab.webview.clone()))
                    .collect::<Vec<_>>(),
                inner.active.clone(),
                inner.slot,
                inner.shown,
            )
        };
        for (tab_id, webview) in &tabs {
            let visible = shown && slot.has_area() && active.as_deref() == Some(tab_id.as_str());
            if visible {
                let _ = webview.set_position(LogicalPosition::new(slot.x, slot.y));
                let _ = webview.set_size(LogicalSize::new(slot.width, slot.height));
                let _ = webview.show();
            } else {
                let _ = webview.hide();
            }
        }
        Ok(())
    }

    fn emit_tabs(&self, app: &AppHandle) {
        if let Ok(inner) = self.inner.lock() {
            report_tabs(app, &inner);
        }
    }
}

/// Tell the UI what the panel's tabs are now.
///
/// The tab list is the only source of tab truth in the app, so every change —
/// a tab opened, closed, promoted from the warm page, retitled — ends here.
fn report_tabs(app: &AppHandle, inner: &Inner) {
    let _ = app.emit(
        "browser_panel:tabs",
        TabsReport {
            tabs: inner.tabs.iter().map(|tab| tab.info.clone()).collect(),
            active: inner.active.clone(),
        },
    );
}

fn poisoned() -> String {
    "面板浏览器状态已损坏".to_string()
}

/// Create one of the panel's webviews: the same profile, the same DevTools
/// port, the same callbacks, whichever tab it is going to be.
///
/// `label` is the webview's label and the tab id it will report as, so the
/// callbacks need no lookup table to say which tab they are talking about.
///
/// This must not be called while holding the host lock: creating a webview runs
/// on the main thread, which also runs the page-load callback below, and that
/// callback takes the same lock.
fn create_webview(
    app: &AppHandle,
    host: &BrowserPanelHost,
    label: &str,
    target: Url,
) -> Result<Webview, String> {
    let port = host.port()?;

    // The profile directory has to exist before WebView2 is handed it, and the
    // failure belongs at the point the panel opens its first page.
    let profile = profile_dir()?;
    std::fs::create_dir_all(&profile)
        .map_err(|error| format!("无法创建面板浏览器目录 {}：{error}", profile.display()))?;

    let inner = Arc::clone(&host.inner);
    let loaded = app.clone();
    let loaded_id = tab_id_of(label);
    let builder = WebviewBuilder::new(label.to_string(), WebviewUrl::External(target))
        .data_directory(profile)
        .additional_browser_args(&browser_args(port))
        .focused(false)
        .on_page_load(move |_webview, payload| {
            let event = match payload.event() {
                tauri::webview::PageLoadEvent::Started => "started",
                tauri::webview::PageLoadEvent::Finished => "finished",
            };
            let url = payload.url().to_string();
            // A real page in the panel's spare blank one means the agent (or a
            // link) is about to put something on screen: it becomes a tab now,
            // so the page has a name in the strip instead of living unseen.
            if let Ok(mut state) = inner.lock() {
                if promote(&mut state, &loaded_id, &url) {
                    report_tabs(&loaded, &state);
                }
            }
            let _ = loaded.emit(
                "browser_panel:page",
                PageReport {
                    tab_id: loaded_id.clone(),
                    url: url.clone(),
                    event: event.to_string(),
                },
            );
            let _ = loaded.emit(
                "browser_panel:url",
                serde_json::json!({ "tab_id": loaded_id.clone(), "url": url }),
            );
        })
        // A page's own title is the label a person recognises; the URL host is
        // only what we have until the document says otherwise.
        .on_document_title_changed({
            let inner = Arc::clone(&host.inner);
            let titled = app.clone();
            let titled_id = tab_id_of(label);
            move |_webview, title| {
                let Ok(mut state) = inner.lock() else {
                    return;
                };
                let Some(tab) = state
                    .tabs
                    .iter_mut()
                    .find(|tab| tab.info.tab_id == titled_id)
                else {
                    return;
                };
                let title = title.trim().to_string();
                if title.is_empty() || tab.info.title == title {
                    return;
                }
                tab.info.title = title;
                report_tabs(&titled, &state);
            }
        })
        // A link that asks for a new window has no window to go to: the panel
        // is the browser, so the frontend opens it as a tab. Without a handler
        // WebView2 refuses the request and the click does nothing at all.
        .on_new_window({
            let opened = app.clone();
            let opener = tab_id_of(label);
            move |url, _features| {
                let _ = opened.emit(
                    "browser_panel:new_window",
                    serde_json::json!({
                        "tab_id": opener.clone(),
                        "url": url.to_string(),
                    }),
                );
                tauri::webview::NewWindowResponse::Deny
            }
        });

    // Created out of the way and hidden: the webview is placed by `place` once
    // the UI says where the slot is.
    let window = main_window(app)?;
    let webview = window
        .add_child(
            builder,
            LogicalPosition::new(OFFSCREEN, OFFSCREEN),
            LogicalSize::new(1024.0, 768.0),
        )
        .map_err(|error| format!("无法创建面板浏览器：{error}"))?;
    let _ = webview.hide();
    Ok(webview)
}

/// How long the panel's browser is given to start answering DevTools, and how
/// often it is asked while it does.
const PUBLISH_DEADLINE: Duration = Duration::from_secs(5);
const PUBLISH_RETRY: Duration = Duration::from_millis(200);

/// Whether a probe says the tools have something to drive here.
///
/// WebView2 does not implement `Target.createTarget`, so a browser with no page
/// on it is a browser the tools cannot use: the provider connects, finds no tab
/// to act on, and its first call fails. Published anyway, it would take every
/// call down with it instead of leaving the tools on the browser settings named.
fn has_a_page_to_drive(pages: usize) -> bool {
    pages > 0
}

/// Publish the panel's DevTools endpoint, once something is listening on it and
/// there is a page on the other end to drive.
///
/// The agent's browser tools take a published panel endpoint over everything
/// settings say — see `app_core::panel_browser` — so what is published has to be
/// usable. An endpoint put up too early would not leave the tools to the browser
/// they were configured with, it would take them down with it: every call would
/// fail with `connection refused`, and the browser the user can see would be the
/// one browser nobody could drive. An endpoint with no page on it is the same
/// failure one step later, because WebView2 has no `Target.createTarget` for the
/// tools to open one with.
///
/// Retried, because `add_child` returns once the webview exists and the browser
/// process behind it is a moment behind that. The outcome is logged either way,
/// so "the panel works but the agent drives something else" is a line in
/// `app.log` rather than a mystery.
///
/// Safe to call again: it returns immediately once the endpoint is published,
/// so the callers may ask on every page the panel opens rather than only at
/// startup.
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
    // string compare, and before one it is the retry that lets a browser which
    // was merely slow to start still end up as the tools' browser.
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
            // point the tools at a browser they cannot open a page in — WebView2
            // has no `Target.createTarget` — so the tools stay on their own
            // browser and this line explains why the panel is not being driven.
            // The retries above and the callers below are what turn this into a
            // wait rather than a verdict: a page the panel is actually showing
            // answers with one.
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

/// Give the panel's spare blank page a tab, now that a real page is loading in
/// it. Returns whether anything changed.
fn promote(inner: &mut Inner, label: &str, url: &str) -> bool {
    let is_open_tab = inner.tabs.iter().any(|tab| tab.info.tab_id == label);
    if !should_promote(is_open_tab, inner.is_warm(label), url) {
        return false;
    }
    let Some(webview) = inner.warm.take() else {
        return false;
    };
    let info = PanelTab {
        tab_id: label.to_string(),
        url: url.to_string(),
        title: title_for(url),
    };
    inner.tabs.push(Tab { info, webview });
    if inner.active.is_none() {
        inner.active = Some(label.to_string());
    }
    true
}

/// Whether a page load in a webview should turn the panel's spare page into a
/// tab: it has to be the spare page, still unused by any tab, and it has to have
/// arrived somewhere real — another blank page is not something to show.
fn should_promote(is_open_tab: bool, is_warm: bool, url: &str) -> bool {
    is_warm && !is_open_tab && !is_blank(url)
}

/// Whether a URL is the blank page the panel keeps alive rather than a page
/// somebody asked for.
fn is_blank(url: &str) -> bool {
    let url = url.trim();
    url == "about:blank" || url.starts_with("about:blank#")
}

/// The label for a page before its document has a title of its own.
fn title_for(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| "新标签页".to_string())
}

/// The panel's tabs, the active one, and the endpoint the agent drives.
#[tauri::command]
pub async fn browser_panel_state(
    host: tauri::State<'_, BrowserPanelHost>,
) -> Result<PanelStateReport, String> {
    host.report()
}

/// Bring the panel's browser up before anyone needs it.
///
/// The browser tools attach to the panel's browser, and a user's first link
/// click should not wait for WebView2 to start, so the app calls this once at
/// startup (see `main.rs`). What it starts is not a tab: it is one blank page,
/// hidden, which becomes a tab the first time a real page lands in it.
///
/// Idempotent. Two callers can arrive at once — startup and a settings change,
/// or either and the first tab — and only one browser may come out of it; the
/// second sees `warming` and is told what there is so far, while the first
/// publishes the endpoint as soon as it has one.
pub async fn warm(app: &AppHandle, host: &BrowserPanelHost) -> Result<Option<String>, String> {
    let label = {
        let mut inner = host.inner.lock().map_err(|_| poisoned())?;
        // Already up, already on its way, or not needed: a tab of its own means
        // the browser exists, and a spare page beside one would be a page
        // nobody asked for.
        if inner.warming || inner.warm.is_some() || !inner.tabs.is_empty() {
            return Ok(inner.port.map(endpoint_for));
        }
        inner.warming = true;
        inner.allocate_id()
    };

    let target = Url::parse("about:blank").map_err(|error| error.to_string())?;
    let created = create_webview(app, host, &label, target);

    // Scoped so the lock is gone before the browser is probed below: the probe
    // waits on the browser process, and the callbacks that process runs take
    // this same lock.
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
    Ok(endpoint)
}

/// Open a URL as a new panel tab and show it.
#[tauri::command]
pub async fn browser_panel_open(
    app: AppHandle,
    host: tauri::State<'_, BrowserPanelHost>,
    url: String,
) -> Result<PanelTab, String> {
    let target = normalize_url(&url)?;

    // The spare blank page is this tab, when there is one: it is the same
    // browser, the same profile, already started, and the agent may already be
    // attached to it. Anything it shows is replaced by what the user asked for.
    let adopted = {
        let mut inner = host.inner.lock().map_err(|_| poisoned())?;
        inner.warm.take()
    };
    let webview = match adopted {
        Some(webview) => {
            if let Err(error) = webview.navigate(target.clone()) {
                // Hand the page back as the spare: it is still the panel's only
                // page, and losing it would leave the browser with nothing for
                // the agent to act on.
                if let Ok(mut inner) = host.inner.lock() {
                    inner.warm = Some(webview);
                }
                return Err(format!("无法打开 {url}：{error}"));
            }
            webview
        }
        None => {
            let label = {
                let mut inner = host.inner.lock().map_err(|_| poisoned())?;
                inner.allocate_id()
            };
            create_webview(&app, &host, &label, target.clone())?
        }
    };
    // Off the tab's critical path: publishing is about the agent's tools, and
    // the page the user just asked for should not wait on it. Done here rather
    // than only where the browser is first created, because this is the one
    // moment the browser is certainly up — so a browser that was too slow to
    // answer at startup is still picked up as the tools' browser, instead of
    // being written off for the rest of the run.
    tauri::async_runtime::spawn(publish_drivable(Arc::clone(&host.inner)));
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
            webview,
        });
        if inner.active.is_none() {
            inner.active = Some(tab_id.clone());
        }
    }
    host.emit_tabs(&app);
    Ok(info)
}

/// Close a panel tab. The page and its session go with it; the profile and its
/// logins stay.
#[tauri::command]
pub async fn browser_panel_close(
    app: AppHandle,
    host: tauri::State<'_, BrowserPanelHost>,
    tab_id: String,
) -> Result<(), String> {
    let (closed, last) = {
        let mut inner = host.inner.lock().map_err(|_| poisoned())?;
        let index = inner
            .tabs
            .iter()
            .position(|tab| tab.info.tab_id == tab_id)
            .ok_or_else(|| format!("面板里没有这个标签页：{tab_id}"))?;
        let tab = inner.tabs.remove(index);
        if inner.active.as_deref() == Some(tab_id.as_str()) {
            inner.active = inner.tabs.last().map(|tab| tab.info.tab_id.clone());
        }
        (tab, inner.tabs.is_empty())
    };
    // Closed outside the lock: closing runs on the main thread.
    let _ = closed.webview.close();
    host.emit_tabs(&app);
    host.place()?;
    if last {
        // The browser tools are attached to this browser: leaving it with no
        // page at all would leave them with nothing to act on, and the next
        // page they open would have nowhere to appear.
        warm(&app, &host).await?;
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
            return Err(format!("面板里没有这个标签页：{tab_id}"));
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
/// else, so the native webview must stop covering it.
#[tauri::command]
pub async fn browser_panel_hide(host: tauri::State<'_, BrowserPanelHost>) -> Result<(), String> {
    let webviews = {
        let mut inner = host.inner.lock().map_err(|_| poisoned())?;
        inner.shown = false;
        inner
            .tabs
            .iter()
            .map(|tab| tab.webview.clone())
            .collect::<Vec<_>>()
    };
    for webview in webviews {
        let _ = webview.hide();
    }
    Ok(())
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
        .navigate(target)
        .map_err(|error| format!("无法打开 {url}：{error}"))
}

/// Reload a tab.
#[tauri::command]
pub async fn browser_panel_reload(
    host: tauri::State<'_, BrowserPanelHost>,
    tab_id: String,
) -> Result<(), String> {
    let webview = host.tab(&tab_id)?;
    let url = webview
        .url()
        .map_err(|error| format!("无法读取当前网址：{error}"))?;
    webview
        .navigate(url)
        .map_err(|error| format!("无法重新加载：{error}"))
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
        other => return Err(format!("未知的历史方向：{other}")),
    };
    host.tab(&tab_id)?
        .eval(call)
        .map_err(|error| format!("无法切换历史：{error}"))
}

/// Where the panel's slot is now, in logical pixels. The native webviews are
/// moved onto it.
///
/// A zero-sized rect is taken as "the panel is not showing a page" — the panel
/// is collapsed or the layout is mid-flight — and hides the webviews for as
/// long as it lasts: a native webview is painted above the app's own UI, so one
/// left in place would cover whatever the panel now shows.
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
    use super::{
        Slot, browser_args, endpoint_for, has_a_page_to_drive, is_blank, normalize_url,
        should_promote, tab_id_of, title_for,
    };

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
    fn a_browser_with_no_page_is_not_worth_publishing() {
        assert!(has_a_page_to_drive(1));
        assert!(has_a_page_to_drive(3));
        // Zero is the case that has to stay unpublished: with no page to drive
        // and no `Target.createTarget` to open one, the tools would be pointed
        // at a browser where every call fails.
        assert!(!has_a_page_to_drive(0));
    }

    #[test]
    fn the_browser_is_started_with_the_port_the_agent_uses() {
        let args = browser_args(45678);
        // wry *replaces* its own defaults with this string rather than adding
        // to it, so a default lost here is a silent behaviour change: the mini
        // menu, the PDF toolbar and the smart-screen overlay come back, and
        // autoplay starts needing a gesture. Hence the exact string.
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
    fn a_tab_id_is_its_webview_label() {
        assert_eq!(tab_id_of("tab-7"), "tab-7");
    }

    #[test]
    fn a_collapsed_slot_has_no_room_for_a_page() {
        assert!(Slot::default().has_area());
        let collapsed = |width, height| Slot {
            x: 0.0,
            y: 0.0,
            width,
            height,
        };
        // The panel is hidden: `display: none` measures as an empty rect, and
        // that rect must not be pushed onto the webview.
        assert!(!collapsed(0.0, 0.0).has_area());
        assert!(!collapsed(0.0, 800.0).has_area());
        assert!(!collapsed(420.0, 0.0).has_area());
        assert!(collapsed(420.0, 800.0).has_area());
    }

    #[test]
    fn the_panels_own_blank_page_is_not_a_page() {
        assert!(is_blank("about:blank"));
        // WebView2 reports the blank page with a fragment once something has
        // navigated inside it.
        assert!(is_blank("about:blank#blocked"));
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
        // A page that is already a tab updates itself; it is not promoted twice.
        assert!(!should_promote(true, true, "https://example.com/"));
        // Not the spare page: a real tab's own page (or a page still being
        // created) has nothing to promote.
        assert!(!should_promote(false, false, "https://example.com/"));
        // Still blank: nothing to show, so it stays the spare page.
        assert!(!should_promote(false, true, "about:blank"));
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
}
