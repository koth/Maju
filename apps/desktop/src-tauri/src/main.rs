#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod events;
mod lsp;
mod open_workspaces;
mod recent_workspaces;
mod state;

use app_core::{UiPatchCursor, UiSnapshotUpdate};
use state::AppState;
#[cfg(target_os = "macos")]
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::Manager;

#[cfg(target_os = "macos")]
const BUNDLED_CODEX_ACP_RESOURCE_DIR: &str = "bundled-codex-acp";
const KODEX_SSH_ASKPASS_ENV: &str = "KODEX_SSH_ASKPASS";
const KODEX_SSH_ASKPASS_PASSWORD_ENV: &str = "KODEX_SSH_ASKPASS_PASSWORD";

fn main() {
    if std::env::var_os(KODEX_SSH_ASKPASS_ENV).as_deref() == Some(std::ffi::OsStr::new("1")) {
        if let Some(password) = std::env::var_os(KODEX_SSH_ASKPASS_PASSWORD_ENV) {
            let mut stdout = std::io::stdout();
            let _ = std::io::Write::write_all(&mut stdout, password.to_string_lossy().as_bytes());
            let _ = std::io::Write::write_all(&mut stdout, b"\n");
            let _ = std::io::Write::flush(&mut stdout);
        }
        return;
    }

    app_core::startup_perf::start_run("kodex-desktop");
    app_core::startup_perf::mark("desktop/main_enter", "");
    install_panic_logger();
    let snapshot_bridge_running = Arc::new(AtomicBool::new(true));

    app_core::startup_perf::mark("desktop/builder_start", "");
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppState::new())
        .setup({
            let snapshot_bridge_running = snapshot_bridge_running.clone();
            move |app| {
                app_core::startup_perf::mark("desktop/setup_start", "");
                if let Err(error) = install_bundled_codex_acp(app) {
                    app_core::startup_perf::mark(
                        "desktop/codex_acp_install_failed",
                        &error.to_string(),
                    );
                }
                if let Err(error) =
                    commands::settings::install_bundled_claude_agent_acp_if_missing(app.handle())
                {
                    app_core::startup_perf::mark("desktop/claude_agent_acp_install_failed", &error);
                }
                let terminal_app = app.handle().clone();
                app.state::<AppState>()
                    .set_terminal_event_sink(Arc::new(move |event| {
                        events::emit_terminal_event(&terminal_app, event);
                    }));
                start_snapshot_bridge(app.handle().clone(), snapshot_bridge_running);
                app_core::startup_perf::mark("desktop/setup_end", "");
                Ok(())
            }
        })
        .on_window_event({
            let snapshot_bridge_running = snapshot_bridge_running.clone();
            move |window, event| {
                if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                    snapshot_bridge_running.store(false, Ordering::Release);
                    window.state::<AppState>().shutdown_all();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::session::session_get_state,
            commands::session::session_send_prompt,
            commands::session::session_retry_user_message,
            commands::session::session_set_config_control,
            commands::session::session_resolve_permission,
            commands::session::session_cancel,
            commands::session::session_list,
            commands::session::session_switch,
            commands::session::session_create,
            commands::session::session_delete,
            commands::session::session_archive,
            commands::session::session_get_changes,
            commands::session::session_list_change_sets,
            commands::session::session_list_change_set_files,
            commands::session::session_get_change_set_file_diff,
            commands::session::session_get_file_diff,
            commands::session::session_reconnect,
            commands::git::git_status,
            commands::git::git_stage,
            commands::git::git_unstage,
            commands::git::git_commit,
            commands::git::git_refresh,
            commands::editor::editor_open_file,
            commands::editor::editor_save_file,
            commands::editor::editor_get_content,
            commands::lsp::editor_lsp_open_document,
            commands::lsp::editor_lsp_change_document,
            commands::lsp::editor_lsp_save_document,
            commands::lsp::editor_lsp_close_document,
            commands::lsp::editor_lsp_get_diagnostics,
            commands::lsp::editor_lsp_request,
            commands::perf::startup_perf_mark,
            commands::fs::fs_list_dir,
            commands::fs::fs_rename,
            commands::fs::fs_delete_file,
            commands::fs::fs_reveal,
            commands::search::fs_search,
            commands::settings::settings_get_agent_snapshot,
            commands::settings::settings_detect_agents,
            commands::settings::settings_select_agent,
            commands::settings::settings_select_theme,
            commands::settings::settings_get_remote_profiles,
            commands::settings::settings_save_remote_profile,
            commands::settings::settings_delete_remote_profile,
            commands::settings::settings_validate_remote_profile,
            commands::settings::settings_save_codex_acp_provider_key,
            commands::settings::settings_select_codex_acp_provider,
            commands::settings::settings_select_codex_default_mode,
            commands::settings::settings_select_agent_provider_profile,
            commands::settings::settings_save_agent_provider_secret,
            commands::settings::settings_save_provider_models,
            commands::settings::settings_reset_provider_models,
            commands::settings::settings_select_claude_fast_model,
            commands::settings::settings_install_agent,
            commands::settings::settings_get_lsp_snapshot,
            commands::settings::settings_save_lsp_server,
            commands::settings::settings_reset_lsp_server,
            commands::settings::settings_probe_lsp_server,
            commands::review::review_get_diff,
            commands::review::review_get_git_diff_content,
            commands::review::review_apply_patch,
            commands::review::review_reject_patch,
            commands::workspace::workspace_open,
            commands::workspace::workspace_open_remote_linux,
            commands::workspace::workspace_open_remote_profile,
            commands::workspace::workspace_close,
            commands::workspace::workspace_archive,
            commands::workspace::workspace_list_open,
            commands::workspace::workspace_has_open,
            commands::workspace::workspace_restore_open,
            commands::workspace::workspace_set_active,
            commands::workspace::workspace_get_recent,
            commands::workspace::workspace_remove_recent,
            commands::terminal::terminal_open,
            commands::terminal::terminal_write,
            commands::terminal::terminal_scrollback,
            commands::terminal::terminal_resize,
            commands::terminal::terminal_terminate,
            commands::terminal::terminal_restart,
            commands::terminal::terminal_list,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Kodex")
        .run({
            let snapshot_bridge_running = snapshot_bridge_running.clone();
            move |app, event| {
                if matches!(event, tauri::RunEvent::Ready) {
                    app_core::startup_perf::mark("desktop/run_ready", "");
                }
                if let tauri::RunEvent::ExitRequested { .. } = event {
                    app_core::startup_perf::mark("desktop/exit_requested", "");
                    snapshot_bridge_running.store(false, Ordering::Release);
                    app.state::<AppState>().shutdown_all();
                }
            }
        });
}

#[cfg(target_os = "macos")]
fn install_bundled_codex_acp(app: &tauri::App) -> anyhow::Result<()> {
    let paths = app_core::AppPaths::resolve()?;
    let target = app_core::settings::codex_acp_binary_path(&paths);
    if target.is_file() {
        return Ok(());
    }

    let source = app
        .path()
        .resource_dir()?
        .join(BUNDLED_CODEX_ACP_RESOURCE_DIR)
        .join("codex-acp");
    if !source.is_file() {
        return Ok(());
    }

    let bin_dir = app_core::settings::codex_acp_bin_dir(&paths);
    std::fs::create_dir_all(&bin_dir)?;
    copy_executable(&source, &target)?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn install_bundled_codex_acp(_app: &tauri::App) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn copy_executable(source: &Path, target: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::copy(source, target)?;
    let mut permissions = std::fs::metadata(target)?.permissions();
    permissions.set_mode(permissions.mode() | 0o755);
    std::fs::set_permissions(target, permissions)?;
    Ok(())
}

fn start_snapshot_bridge(app: tauri::AppHandle, running: Arc<AtomicBool>) {
    app_core::startup_perf::mark("desktop/snapshot_bridge_spawn", "");
    std::thread::spawn(move || {
        app_core::startup_perf::mark("desktop/snapshot_bridge_thread_start", "");
        let mut cursor = UiPatchCursor::default();

        while running.load(Ordering::Acquire) {
            let next_update = app
                .state::<AppState>()
                .poll_active_and_get_update(&mut cursor)
                .ok();

            match next_update {
                Some(Some(UiSnapshotUpdate::Full(snapshot))) => {
                    events::emit_ui_snapshot(&app, &snapshot);
                }
                Some(Some(UiSnapshotUpdate::Patch(patch))) => {
                    events::emit_ui_snapshot_patch(&app, &patch);
                }
                Some(None) => {}
                None => {
                    cursor = UiPatchCursor::default();
                }
            }

            std::thread::sleep(Duration::from_millis(220));
        }
    });
}

fn install_panic_logger() {
    std::panic::set_hook(Box::new(|info| {
        let Ok(paths) = app_core::AppPaths::resolve() else {
            return;
        };
        let logs_dir = paths.logs_dir();
        let _ = std::fs::create_dir_all(&logs_dir);
        let log_path = logs_dir.join("kodex-panic.log");
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis().to_string())
            .unwrap_or_else(|_| "unknown".into());
        let payload = format!("[{timestamp}] {info}\n");
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .and_then(|mut file| {
                use std::io::Write;
                file.write_all(payload.as_bytes())
            });
    }));
}
