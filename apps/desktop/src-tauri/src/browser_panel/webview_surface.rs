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
use std::sync::atomic::{AtomicU64, Ordering};

use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Url, Webview, WebviewBuilder, WebviewUrl,
};

use super::{BrowserPanelHost, OFFSCREEN, PageReport, Slot, main_window, report_tabs, tab_id_of};
// `profile_dir` lives in this file (below), not in the parent: importing it from
// `super` is a Windows-only compile error, which is why it is not here.
#[cfg(windows)]
use super::promote;

/// Labels for short-lived authentication popups. They are not panel tabs.
static POPUP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Authentication providers used by the panel's pages. They are allowed to
/// open a real popup even when the page did not specify width/height.
fn is_auth_popup_url(url: &Url) -> bool {
    matches!(
        url.host_str().unwrap_or_default(),
        "accounts.google.com" | "appleid.apple.com" | "login.microsoftonline.com" | "login.live.com"
    )
}

/// Whether a `window.open` request needs a real popup instead of a panel tab.
///
/// OAuth relies on `window.opener`: the provider posts the result back to the
/// page that opened it and then closes itself. Turning that request into an
/// ordinary tab drops the opener relationship, so X's "continue with Google"
/// waits forever. A requested size is the generic signal for a popup; known
/// identity-provider origins cover pages that omit it. Plain `target="_blank"`
/// links have neither and keep opening as panel tabs.
fn should_open_popup(url: &Url, has_size: bool) -> bool {
    has_size || is_auth_popup_url(url)
}

fn next_popup_label() -> String {
    format!(
        "browser-popup-{}",
        POPUP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

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
        // An OAuth popup must stay a popup: its result travels through
        // `window.opener`, which a separate panel tab does not have. Ordinary
        // `target="_blank"` links still become panel tabs, as before.
        .on_new_window({
            let opened = app.clone();
            let opener = tab_id_of(label);
            move |url, features| {
                if should_open_popup(&url, features.size().is_some()) {
                    let title = url.host_str().unwrap_or("登录").to_string();
                    let builder = tauri::WebviewWindowBuilder::new(
                        &opened,
                        next_popup_label(),
                        WebviewUrl::External(url.clone()),
                    )
                    .title(title)
                    .inner_size(520.0, 720.0)
                    .focused(true)
                    // Shares the opener's WebView2 environment/configuration:
                    // required by the platform to attach the new webview.
                    .window_features(features);
                    match builder.build() {
                        Ok(window) => {
                            return tauri::webview::NewWindowResponse::Create { window };
                        }
                        Err(error) => {
                            tracing::warn!(
                                target: "browser_panel",
                                %url,
                                %error,
                                "could not open authentication popup; falling back to a tab",
                            );
                        }
                    }
                }
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

/// Where a surface goes after a layout or paint-lease change.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Placement {
    /// No agent work needs frames: hide the controller and let WebView2 idle.
    Hidden,
    /// Keep the controller painting without putting the page over the app UI.
    /// The size is deliberately left alone so parking never reflows the page.
    Parked,
    /// The panel's real on-screen slot.
    Visible(Slot),
}

/// Decide a surface's native placement.
///
/// `IsVisible = false` is not just an OS-level hide in WebView2: it stops the
/// compositor, and CDP screenshots wait indefinitely for a frame that will
/// never arrive. A paint lease therefore parks the page off screen *visible*
/// instead of hiding it. Inactive pages return to `Hidden` as soon as the last
/// lease drops, so this does not leave every background tab rendering forever.
pub(super) fn placement_for(slot: Option<Slot>, paint_held: bool) -> Placement {
    match slot {
        Some(slot) => Placement::Visible(slot),
        None if paint_held => Placement::Parked,
        None => Placement::Hidden,
    }
}

/// Put a webview at the slot, park it paintably off screen, or hide it.
pub(super) fn place(webview: &Webview, slot: Option<Slot>, paint_held: bool) {
    match placement_for(slot, paint_held) {
        Placement::Visible(slot) => {
            let _ = webview.set_position(LogicalPosition::new(slot.x, slot.y));
            let _ = webview.set_size(LogicalSize::new(slot.width, slot.height));
            let _ = webview.show();
        }
        Placement::Parked => {
            let _ = webview.set_position(LogicalPosition::new(OFFSCREEN, OFFSCREEN));
            let _ = webview.show();
        }
        Placement::Hidden => {
            let _ = webview.hide();
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn slot() -> Slot {
        Slot {
            x: 40.0,
            y: 80.0,
            width: 640.0,
            height: 480.0,
        }
    }

    #[test]
    fn a_paint_lease_parks_hidden_pages_instead_of_stopping_their_frames() {
        assert_eq!(placement_for(None, false), Placement::Hidden);
        assert_eq!(placement_for(None, true), Placement::Parked);
    }

    #[test]
    fn a_visible_slot_always_wins_over_parking() {
        assert_eq!(
            placement_for(Some(slot()), false),
            Placement::Visible(slot())
        );
        assert_eq!(
            placement_for(Some(slot()), true),
            Placement::Visible(slot())
        );
    }

    #[test]
    fn auth_popups_keep_the_opener_while_ordinary_links_stay_tabs() {
        let google = Url::parse("https://accounts.google.com/gsi/select").unwrap();
        let ordinary = Url::parse("https://x.com/some/article").unwrap();

        // Google's popup sometimes omits width/height; its origin still marks
        // it as an opener-dependent flow.
        assert!(should_open_popup(&google, false));
        // Any explicitly sized window is a popup by intent.
        assert!(should_open_popup(&ordinary, true));
        // A normal target="_blank" keeps the panel-tab behaviour.
        assert!(!should_open_popup(&ordinary, false));
    }

    #[test]
    fn popup_labels_do_not_collide() {
        assert_ne!(next_popup_label(), next_popup_label());
    }
}
