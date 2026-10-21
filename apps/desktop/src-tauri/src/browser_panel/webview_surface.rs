//! The panel's surface: one child webview of the app window, per tab.
//!
//! Every desktop platform gets the same surface — WebView2 on Windows,
//! WKWebView on macOS — created with `Window::add_child`, so a page is a subview
//! of the app window itself rather than a window floating above it. Rendering,
//! scrolling, the caret and the input method are the platform's own, which is
//! the whole reason the panel is a webview and not a picture of one (see
//! `docs/browser-view-subsystem.md`).
//!
//! The one platform difference is what the webview is *started* with: Windows
//! passes the panel's DevTools port, which is what makes this webview the
//! browser the agent's tools drive. WKWebView has no CDP endpoint at all, so on
//! macOS the panel is a viewer and the tools keep the browser their settings
//! name. Nothing else about a tab differs by platform.

use std::sync::Arc;

use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Url, Webview, WebviewBuilder, WebviewUrl,
};

use super::{BrowserPanelHost, OFFSCREEN, PageReport, Slot, main_window, report_tabs, tab_id_of};
#[cfg(windows)]
use super::{promote, profile_dir};

/// Create one of the panel's webviews, whichever tab it is going to be.
///
/// `label` is the webview's label and the tab id it will report as, so the
/// callbacks need no lookup table to say which tab they are talking about.
///
/// This must not be called while holding the host lock: creating a webview runs
/// on the main thread, which also runs the page-load callback below, and that
/// callback takes the same lock.
pub(super) fn create_webview(
    app: &AppHandle,
    host: &BrowserPanelHost,
    label: &str,
    target: Url,
) -> Result<Webview, String> {
    let loaded = app.clone();
    let loaded_id = tab_id_of(label);
    let inner = Arc::clone(&host.inner);
    let page_events = host.page_events.clone();
    let builder = WebviewBuilder::new(label.to_string(), WebviewUrl::External(target))
        .focused(false)
        .on_page_load(move |_webview, payload| {
            let event: &'static str = match payload.event() {
                tauri::webview::PageLoadEvent::Started => "started",
                tauri::webview::PageLoadEvent::Finished => "finished",
            };
            let url = payload.url().to_string();
            // Windows: a real page in the panel's spare blank one means the
            // agent (or a link) is about to put something on screen, so it
            // becomes a tab now and has a name in the strip instead of living
            // unseen.
            #[cfg(windows)]
            if let Ok(mut state) = inner.lock() {
                if promote(&mut state, &loaded_id, &url) {
                    report_tabs(&loaded, &state);
                }
            }
            // The panel driver's view of the same moment: which tab is loading,
            // and when it stops. The timestamp is the guard against a load
            // whose `finished` never arrives (cancelled or failed navigations
            // are not always paired): `is_loading` expires entries by age.
            if let Ok(mut state) = inner.lock() {
                if event == "started" {
                    state
                        .loading
                        .insert(loaded_id.clone(), std::time::Instant::now());
                } else {
                    state.loading.remove(&loaded_id);
                }
            }
            let _ = page_events.send(super::PageEvent {
                tab_id: loaded_id.clone(),
                event,
            });
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
        // the webview refuses the request and the click does nothing at all.
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

    // Windows: the panel's own profile, kept across restarts, and the DevTools
    // port the agent's tools attach to — one WebView2 browser process serves
    // every panel tab. macOS has neither to configure: WKWebView keeps its
    // cookies in the app's own data store and speaks no CDP.
    #[cfg(windows)]
    let builder = {
        let profile = profile_dir()?;
        std::fs::create_dir_all(&profile)
            .map_err(|error| format!("无法创建面板浏览器目录 {}：{error}", profile.display()))?;
        builder
            .data_directory(profile)
            .additional_browser_args(&browser_args(host.port()?))
    };

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

/// Put a webview at the slot, or hide it entirely.
pub(super) fn place(webview: &Webview, slot: Option<Slot>) {
    if let Some(slot) = slot {
        let _ = webview.set_position(LogicalPosition::new(slot.x, slot.y));
        let _ = webview.set_size(LogicalSize::new(slot.width, slot.height));
        let _ = webview.show();
    } else {
        let _ = webview.hide();
    }
}

/// Where the Windows panel's browser keeps its profile: its own logins, its
/// own cookies, kept across restarts, separate from the app and from any other
/// browser on the machine.
#[cfg(windows)]
pub(super) fn profile_dir() -> Result<std::path::PathBuf, String> {
    let paths = app_core::AppPaths::resolve().map_err(|error| error.to_string())?;
    Ok(paths.root().join("browser").join("panel"))
}

/// `--remote-debugging-port` is the agent's way in and the panel's reason for
/// owning a profile: this browser process is the one the tools drive, so the
/// page the user reads and the page the agent acts on are the same page.
///
/// The other two are wry's own defaults. `additional_browser_args` replaces
/// them rather than adding to it, so they are repeated here to keep the panel
/// from gaining the PDF toolbar, the smart-screen overlay and a gesture
/// requirement for autoplay.
#[cfg(windows)]
pub(super) fn browser_args(port: u16) -> String {
    format!(
        "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
         --autoplay-policy=no-user-gesture-required \
         --remote-debugging-port={port}"
    )
}
