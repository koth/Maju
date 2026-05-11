use tauri::{AppHandle, Emitter};
use workspace_model::{UiSnapshot, UiSnapshotPatch};

pub fn emit_ui_snapshot(app: &AppHandle, snapshot: &UiSnapshot) {
    let _ = app.emit("ui:snapshot", snapshot);
}

pub fn emit_ui_snapshot_patch(app: &AppHandle, patch: &UiSnapshotPatch) {
    let _ = app.emit("ui:snapshot_patch", patch);
}

pub fn emit_session_config_updated(app: &AppHandle, snapshot: &UiSnapshot) {
    let _ = app.emit("session:config_updated", &snapshot.session_config);
}
