use crate::state::AppState;
use tauri::{AppHandle, State};
use workspace_model::BrowserPreflight;

/// Run browser preflight so the settings pane can show why the capability is
/// or is not usable, instead of the user enabling it and seeing nothing.
#[tauri::command]
pub fn browser_preflight(state: State<'_, AppState>) -> Result<BrowserPreflight, String> {
    state.browser_preflight()
}

/// Hand a URL to the operating system for a scheme the app cannot render itself
/// (`mailto:`, `tel:`, and similar handlers).
///
/// Web links are **refused** here on purpose. `http`/`https` belongs in the
/// panel's own browser, which is a real webview the app renders itself, so a web
/// URL reaching this command is a defect: it is rejected and logged at `warn`
/// with the caller's `source` so the exact click that fell through can be traced
/// back, instead of silently opening the user's system browser.
#[tauri::command]
pub async fn open_external_url(app: AppHandle, url: String, source: String) -> Result<(), String> {
    let target = url.trim().to_string();
    if target.is_empty() {
        return Ok(());
    }
    if is_web_url(&target) {
        tracing::warn!(
            target: "browser",
            source = %source,
            url = %target,
            "refused to hand a web link to the system browser; web links belong in the browser panel"
        );
        return Err(format!(
            "web links open in the browser panel, not the system browser: {target}"
        ));
    }
    tracing::info!(
        target: "browser",
        source = %source,
        url = %target,
        "handing a link to the system browser"
    );
    use tauri_plugin_shell::ShellExt;
    app.shell()
        .open(target, None)
        .map_err(|e| format!("failed to open the link: {e}"))
}

/// Whether a URL uses a scheme the browser panel owns.
fn is_web_url(url: &str) -> bool {
    let lowered = url.trim().to_ascii_lowercase();
    lowered.starts_with("http://") || lowered.starts_with("https://")
}
