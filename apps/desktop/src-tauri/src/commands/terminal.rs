use crate::state::AppState;
use tauri::State;
use workspace_model::{
    TerminalIdRequest, TerminalOpenRequest, TerminalResizeRequest, TerminalScrollback,
    TerminalSession, TerminalWriteRequest,
};

#[tauri::command]
pub fn terminal_open(
    state: State<'_, AppState>,
    request: TerminalOpenRequest,
) -> Result<TerminalSession, String> {
    state.terminal_open(request)
}

#[tauri::command]
pub fn terminal_write(
    state: State<'_, AppState>,
    request: TerminalWriteRequest,
) -> Result<(), String> {
    state.terminal_write(request)
}

#[tauri::command]
pub fn terminal_scrollback(
    state: State<'_, AppState>,
    request: TerminalIdRequest,
) -> Result<TerminalScrollback, String> {
    let data = state.terminal_scrollback(&request.terminal_id)?;
    Ok(TerminalScrollback {
        terminal_id: request.terminal_id,
        data,
    })
}

#[tauri::command]
pub fn terminal_resize(
    state: State<'_, AppState>,
    request: TerminalResizeRequest,
) -> Result<TerminalSession, String> {
    state.terminal_resize(request)
}

#[tauri::command]
pub fn terminal_terminate(
    state: State<'_, AppState>,
    request: TerminalIdRequest,
) -> Result<(), String> {
    state.terminal_terminate(&request.terminal_id)
}

#[tauri::command]
pub fn terminal_restart(
    state: State<'_, AppState>,
    request: TerminalResizeRequest,
) -> Result<TerminalSession, String> {
    state.terminal_restart(request)
}

#[tauri::command]
pub fn terminal_list(
    state: State<'_, AppState>,
    workspace_root: Option<String>,
) -> Result<Vec<TerminalSession>, String> {
    state.terminal_list(workspace_root)
}
