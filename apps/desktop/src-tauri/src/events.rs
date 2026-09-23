use tauri::{AppHandle, Emitter};
use terminal_service::TerminalServiceEvent;
use workspace_model::{
    AutomationFiredEvent, ProxyRetryStatus, RemoteOpenProgressEvent, UiSnapshot, UiSnapshotPatch,
};

pub fn emit_ui_snapshot(app: &AppHandle, snapshot: &UiSnapshot) {
    let _ = app.emit("ui:snapshot", snapshot);
}

pub fn emit_ui_snapshot_patch(app: &AppHandle, patch: &UiSnapshotPatch) {
    let _ = app.emit("ui:snapshot_patch", patch);
}

pub fn emit_session_config_updated(app: &AppHandle, snapshot: &UiSnapshot) {
    let _ = app.emit("session:config_updated", &snapshot.session_config);
}

pub fn emit_terminal_event(app: &AppHandle, event: TerminalServiceEvent) {
    match event {
        TerminalServiceEvent::Output(output) => {
            let _ = app.emit("terminal:output", output);
        }
        TerminalServiceEvent::Status(status) => {
            let _ = app.emit("terminal:status", status);
        }
        TerminalServiceEvent::Exit(exit) => {
            let _ = app.emit("terminal:exit", exit);
        }
    }
}

pub fn emit_remote_open_progress(app: &AppHandle, progress: &RemoteOpenProgressEvent) {
    let _ = app.emit("remote_open:progress", progress);
}

/// Push the current upstream-retry status (or `None` when no retry is in
/// flight) to the frontend so it can render/clear the retry animation. The
/// snapshot bridge calls this each wake when the value changes.
pub fn emit_proxy_retry_status(app: &AppHandle, status: Option<&ProxyRetryStatus>) {
    let _ = app.emit("proxy:retry", status);
}

/// "到点提醒": fired when an automation (定时任务) reaches its trigger point
/// and its prompt run is dispatched — or when dispatch fails (the reminder
/// still surfaces, carrying the error).
pub fn emit_automation_fired(app: &AppHandle, event: &AutomationFiredEvent) {
    let _ = app.emit("automation:fired", event);
}
