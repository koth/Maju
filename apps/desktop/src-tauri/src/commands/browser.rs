use crate::state::AppState;
use tauri::State;
use workspace_model::BrowserPreflight;

/// Navigate the session's browser, refresh it, or close it.
///
/// These are user actions against the same browser the agent drives, so they
/// go through the same registry and the same per-session operation lock: a
/// manual click cannot interleave with an agent navigation halfway through.
#[tauri::command]
pub fn browser_navigate(
    state: State<'_, AppState>,
    request: workspace_model::BrowserNavigateRequest,
) -> Result<(), String> {
    state.browser_navigate(request)
}

#[tauri::command]
pub fn browser_refresh(
    state: State<'_, AppState>,
    request: workspace_model::BrowserSessionRequest,
) -> Result<(), String> {
    state.browser_refresh(request)
}

#[tauri::command]
pub fn browser_close(
    state: State<'_, AppState>,
    request: workspace_model::BrowserSessionRequest,
) -> Result<(), String> {
    state.browser_close(request)
}

/// Run browser preflight so the settings pane can show why the capability is
/// or is not usable, instead of the user enabling it and seeing nothing.
#[tauri::command]
pub fn browser_preflight(state: State<'_, AppState>) -> Result<BrowserPreflight, String> {
    state.browser_preflight()
}
