use crate::lsp::{LanguageServerRegistry, LspDiagnostic, LspServerStatus};
use crate::state::AppState;
use serde_json::Value;
use tauri::State;

#[tauri::command]
pub fn editor_lsp_open_document(
    state: State<'_, AppState>,
    path: String,
    language_id: String,
    content: String,
) -> Result<LspServerStatus, String> {
    let workspace_root = local_workspace_root(&state)?;
    configure_lsp_from_settings(&state)?;
    state.lsp_service().open_document(
        &workspace_root,
        &language_id,
        std::path::Path::new(&path),
        &content,
    )
}

fn configure_lsp_from_settings(state: &State<'_, AppState>) -> Result<(), String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    let settings = app_core::settings::load_app_settings(&paths);
    state
        .lsp_service()
        .configure_registry(LanguageServerRegistry::from_settings(&settings));
    Ok(())
}

#[tauri::command]
pub fn editor_lsp_change_document(
    state: State<'_, AppState>,
    path: String,
    language_id: String,
    content: String,
) -> Result<i32, String> {
    let workspace_root = local_workspace_root(&state)?;
    state.lsp_service().change_document(
        &workspace_root,
        &language_id,
        std::path::Path::new(&path),
        &content,
    )
}

#[tauri::command]
pub fn editor_lsp_save_document(
    state: State<'_, AppState>,
    path: String,
    language_id: String,
    content: String,
) -> Result<(), String> {
    let workspace_root = local_workspace_root(&state)?;
    state.lsp_service().save_document(
        &workspace_root,
        &language_id,
        std::path::Path::new(&path),
        &content,
    )
}

#[tauri::command]
pub fn editor_lsp_close_document(
    state: State<'_, AppState>,
    path: String,
    language_id: String,
) -> Result<(), String> {
    let workspace_root = local_workspace_root(&state)?;
    state
        .lsp_service()
        .close_document(&workspace_root, &language_id, std::path::Path::new(&path));
    Ok(())
}

#[tauri::command]
pub fn editor_lsp_get_diagnostics(
    state: State<'_, AppState>,
    path: String,
    language_id: String,
) -> Result<Vec<LspDiagnostic>, String> {
    let workspace_root = local_workspace_root(&state)?;
    Ok(state.lsp_service().diagnostics_for_document(
        &workspace_root,
        &language_id,
        std::path::Path::new(&path),
    ))
}

#[tauri::command]
pub fn editor_lsp_request(
    state: State<'_, AppState>,
    language_id: String,
    method: String,
    params: Value,
) -> Result<Value, String> {
    let workspace_root = local_workspace_root(&state)?;
    state
        .lsp_service()
        .request(&workspace_root, &language_id, &method, params)
}

fn local_workspace_root(state: &State<'_, AppState>) -> Result<std::path::PathBuf, String> {
    state.with_app(|app| {
        if app.is_remote_workspace() {
            Err("Remote workspaces do not support local LSP servers yet".into())
        } else {
            Ok(app.ui.workspace.root.clone())
        }
    })
}
