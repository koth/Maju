//! Commands for the right-panel browser view (see
//! `docs/browser-view-subsystem.md`).
//!
//! The view is a second CDP client of the session's browser: it streams
//! screencast frames to the UI and forwards the user's input back. One
//! [`BrowserView`] per session, created on first attach and disposed when the
//! panel detaches or the session is switched away.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use app_core::browser_view::{
    BrowserView, BrowserViewSink, PageTarget, ViewInputEvent, ViewStatus,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

/// Live views keyed by session id.
#[derive(Default)]
pub struct BrowserViewHost {
    views: Mutex<HashMap<String, Arc<BrowserView>>>,
}

#[derive(Clone, Serialize)]
pub struct BrowserViewStatusReport {
    pub session_id: String,
    pub status: String,
}

#[derive(Clone, Serialize)]
pub struct BrowserViewTargetsReport {
    pub session_id: String,
    pub targets: Vec<PageTarget>,
}

#[derive(Clone, Serialize)]
pub struct BrowserViewMetaReport {
    pub session_id: String,
    pub target_id: String,
    pub url: Option<String>,
    pub title: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct BrowserViewFrameReport {
    pub session_id: String,
    pub target_id: String,
    pub frame: String,
    pub seq: u64,
}

#[derive(Clone, Serialize)]
pub struct BrowserViewPageReport {
    pub target_id: String,
}

fn status_word(status: ViewStatus) -> &'static str {
    match status {
        ViewStatus::Connecting => "connecting",
        ViewStatus::Live => "live",
        ViewStatus::Closed => "closed",
        ViewStatus::Failed => "failed",
    }
}

/// Bridges the view service to tauri events. The session id is stamped on
/// every event so a panel bound to one session ignores frames from another.
struct TauriViewSink {
    app: AppHandle,
    session_id: String,
}

impl BrowserViewSink for TauriViewSink {
    fn status(&self, status: ViewStatus, detail: Option<String>) {
        let _ = self.app.emit(
            "browser_view:status",
            serde_json::json!({
                "session_id": self.session_id,
                "status": status_word(status),
                "detail": detail,
            }),
        );
    }

    fn targets(&self, targets: Vec<PageTarget>) {
        let _ = self.app.emit(
            "browser_view:targets",
            BrowserViewTargetsReport {
                session_id: self.session_id.clone(),
                targets,
            },
        );
    }

    fn meta(&self, target_id: &str, url: Option<String>, title: Option<String>) {
        let _ = self.app.emit(
            "browser_view:meta",
            BrowserViewMetaReport {
                session_id: self.session_id.clone(),
                target_id: target_id.to_string(),
                url,
                title,
            },
        );
    }

    fn frame(&self, target_id: &str, jpeg_base64: &str, seq: u64) {
        let _ = self.app.emit(
            "browser_view:frame",
            BrowserViewFrameReport {
                session_id: self.session_id.clone(),
                target_id: target_id.to_string(),
                frame: jpeg_base64.to_string(),
                seq,
            },
        );
    }
}

/// The shared browser server this session's browser is served by.
///
/// The built-in browser IS the browser-use tools' browser (one per session,
/// managed behind the same capability), so a configuration that withholds the
/// tools also declines the panel — callers fall back to the system browser.
fn browser_server(
) -> Result<Arc<app_core::browser_server::BrowserServerService>, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    let settings = app_core::settings::load_app_settings(&paths);
    if !settings.browser.enabled {
        return Err("browser tools are disabled".to_string());
    }
    let shared = app_core::shared_mcp::shared_mcp()
        .browser_server(&paths, &settings.browser)
        .map_err(|e| e.to_string())?;
    Ok(shared.service())
}

/// Get the session's view, connecting (and if needed starting the session's
/// browser) on first use. Attach is idempotent.
async fn view_for(
    app: &AppHandle,
    host: &BrowserViewHost,
    session_id: &str,
) -> Result<Arc<BrowserView>, String> {
    if let Some(view) = host
        .views
        .lock()
        .map_err(|_| "browser view host poisoned".to_string())?
        .get(session_id)
        .cloned()
    {
        return Ok(view);
    }

    let service = browser_server()?;
    let endpoint = app_core::browser_view::cdp_endpoint_for(&service, session_id).await?;
    let sink = Arc::new(TauriViewSink {
        app: app.clone(),
        session_id: session_id.to_string(),
    });
    let view = BrowserView::attach(&endpoint, sink)
        .await
        .map_err(|e| format!("failed to connect the browser view: {e}"))?;
    host.views
        .lock()
        .map_err(|_| "browser view host poisoned".to_string())?
        .insert(session_id.to_string(), view.clone());
    Ok(view)
}

async fn with_view<F, Fut, T>(
    app: &AppHandle,
    host: &BrowserViewHost,
    session_id: &str,
    op: F,
) -> Result<T, String>
where
    F: FnOnce(Arc<BrowserView>) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let view = view_for(app, host, session_id).await?;
    op(view).await
}

/// Connect the view and start streaming targets/frames.
#[tauri::command]
pub async fn browser_view_attach(
    app: AppHandle,
    host: State<'_, BrowserViewHost>,
    session_id: String,
) -> Result<BrowserViewStatusReport, String> {
    view_for(&app, &host, &session_id).await?;
    Ok(BrowserViewStatusReport {
        session_id,
        status: "live".to_string(),
    })
}

/// Stop the view. The browser and the agent's tools are untouched.
#[tauri::command]
pub async fn browser_view_detach(
    host: State<'_, BrowserViewHost>,
    session_id: String,
) -> Result<(), String> {
    let view = host
        .views
        .lock()
        .map_err(|_| "browser view host poisoned".to_string())?
        .remove(&session_id);
    if let Some(view) = view {
        view.detach().await;
    }
    Ok(())
}

/// Switch which page the panel is showing.
#[tauri::command]
pub async fn browser_view_focus(
    app: AppHandle,
    host: State<'_, BrowserViewHost>,
    session_id: String,
    target_id: String,
) -> Result<(), String> {
    with_view(&app, &host, &session_id, |view| async move {
        view.focus(&target_id)
            .await
            .map_err(|e| format!("failed to focus browser page: {e}"))
    })
    .await
}

/// Open a URL in the session's browser as a new page (link clicks).
#[tauri::command]
pub async fn browser_view_open_page(
    app: AppHandle,
    host: State<'_, BrowserViewHost>,
    session_id: String,
    url: String,
) -> Result<BrowserViewPageReport, String> {
    let target_id = with_view(&app, &host, &session_id, |view| async move {
        view.open_page(&url)
            .await
            .map_err(|e| format!("failed to open browser page: {e}"))
    })
    .await?;
    Ok(BrowserViewPageReport { target_id })
}

/// Close a browser page (tab close).
#[tauri::command]
pub async fn browser_view_close_page(
    app: AppHandle,
    host: State<'_, BrowserViewHost>,
    session_id: String,
    target_id: String,
) -> Result<(), String> {
    with_view(&app, &host, &session_id, |view| async move {
        view.close_page(&target_id)
            .await
            .map_err(|e| format!("failed to close browser page: {e}"))
    })
    .await
}

/// Track the panel slot's size so the page lays out for it (logical pixels).
#[tauri::command]
pub async fn browser_view_set_size(
    app: AppHandle,
    host: State<'_, BrowserViewHost>,
    session_id: String,
    target_id: String,
    width: f64,
    height: f64,
) -> Result<(), String> {
    with_view(&app, &host, &session_id, |view| async move {
        view.set_size(&target_id, width, height)
            .await
            .map_err(|e| format!("failed to size browser page: {e}"))
    })
    .await
}

/// Forward a user input event (mouse / key / text) to the page.
#[tauri::command]
pub async fn browser_view_input(
    app: AppHandle,
    host: State<'_, BrowserViewHost>,
    session_id: String,
    target_id: String,
    event: ViewInputEvent,
) -> Result<(), String> {
    with_view(&app, &host, &session_id, |view| async move {
        view.input(&target_id, &event)
            .await
            .map_err(|e| format!("failed to forward browser input: {e}"))
    })
    .await
}
