#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod events;
mod recent_workspaces;
mod state;

use state::AppState;
use std::time::Duration;
use tauri::Manager;

fn main() {
    install_panic_logger();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::new())
        .setup(|app| {
            start_snapshot_bridge(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::session::session_get_state,
            commands::session::session_send_prompt,
            commands::session::session_set_config_control,
            commands::session::session_resolve_permission,
            commands::session::session_cancel,
            commands::session::session_list,
            commands::session::session_switch,
            commands::session::session_create,
            commands::session::session_delete,
            commands::session::session_get_changes,
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
            commands::fs::fs_list_dir,
            commands::search::fs_search,
            commands::settings::settings_get_agent_snapshot,
            commands::settings::settings_detect_agents,
            commands::settings::settings_select_agent,
            commands::settings::settings_install_agent,
            commands::review::review_get_diff,
            commands::review::review_apply_patch,
            commands::review::review_reject_patch,
            commands::workspace::workspace_open,
            commands::workspace::workspace_close,
            commands::workspace::workspace_get_recent,
            commands::workspace::workspace_remove_recent,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Kodex");
}

fn start_snapshot_bridge(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let mut last_snapshot_json = String::new();

        loop {
            let next_snapshot = app
                .state::<AppState>()
                .with_app(|application| {
                    application.poll_prompt_progress();
                    Ok(application.ui.clone())
                })
                .ok();

            match next_snapshot {
                Some(snapshot) => {
                    if let Ok(json) = serde_json::to_string(&snapshot) {
                        if json != last_snapshot_json {
                            last_snapshot_json = json;
                            events::emit_ui_snapshot(&app, &snapshot);
                        }
                    } else {
                        events::emit_ui_snapshot(&app, &snapshot);
                    }
                }
                None => {
                    last_snapshot_json.clear();
                }
            }

            std::thread::sleep(Duration::from_millis(120));
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
