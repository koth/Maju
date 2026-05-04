use tauri::{AppHandle, Emitter};
use workspace_model::UiSnapshot;

pub fn emit_session_status(app: &AppHandle, snapshot: &UiSnapshot) {
    let _ = app.emit("session:status", &snapshot.session);
}

pub fn emit_session_message(app: &AppHandle, snapshot: &UiSnapshot) {
    let _ = app.emit("session:message", &snapshot.messages);
}

pub fn emit_tool_updated(app: &AppHandle, snapshot: &UiSnapshot) {
    let _ = app.emit("tool:updated", &snapshot.tools);
}

pub fn emit_git_status_changed(app: &AppHandle, snapshot: &UiSnapshot) {
    let _ = app.emit("git:status_changed", &snapshot.repository);
}

pub fn emit_session_config_updated(app: &AppHandle, snapshot: &UiSnapshot) {
    let _ = app.emit("session:config_updated", &snapshot.session_config);
}
