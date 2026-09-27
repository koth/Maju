use app_core::browser_install::InstallPhaseSink;
use std::sync::Arc;
use tauri::Emitter;
use workspace_model::BrowserInstallState;

/// Start a provider install, or join the one already running.
///
/// The provisioner lives in `app-core`; this command only starts it and
/// forwards its phase transitions onto the app's event bus, so the settings
/// pane never has to poll.
#[tauri::command]
pub fn browser_install(app: tauri::AppHandle) -> Result<BrowserInstallState, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    app_core::browser_install::on_phase(&paths, Arc::new(TauriPhaseSink { app: app.clone() }));
    app_core::browser_install::install(&paths)
}

/// Read the current install state without starting anything.
#[tauri::command]
pub fn browser_install_state() -> Result<BrowserInstallState, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    Ok(app_core::browser_install::install_state(&paths))
}

/// Re-run preflight, so installing and reopening settings clears the warning
/// without restarting the app.
#[tauri::command]
pub fn browser_refresh_preflight() -> Result<workspace_model::BrowserPreflight, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    Ok(app_core::settings::browser_preflight(&paths).into())
}

struct TauriPhaseSink {
    app: tauri::AppHandle,
}

impl InstallPhaseSink for TauriPhaseSink {
    fn on_phase(&self, phase: &app_core::browser_install::ProvisionPhase) {
        let state = workspace_model::BrowserInstallState {
            label: phase.label(),
            running: phase.is_running(),
            verified: phase.succeeded(),
            // Not read here: the shell does not hold the provisioner, and the
            // pane re-reads the real check after the operation ends.
            installed: false,
            phase: match phase {
                app_core::browser_install::ProvisionPhase::Resolving => {
                    workspace_model::BrowserInstallPhase::Resolving
                }
                app_core::browser_install::ProvisionPhase::InstallingPackage => {
                    workspace_model::BrowserInstallPhase::InstallingPackage
                }
                app_core::browser_install::ProvisionPhase::InstallingChromium => {
                    workspace_model::BrowserInstallPhase::InstallingChromium
                }
                app_core::browser_install::ProvisionPhase::Verifying => {
                    workspace_model::BrowserInstallPhase::Verifying
                }
                app_core::browser_install::ProvisionPhase::Complete { verified } => {
                    workspace_model::BrowserInstallPhase::Complete {
                        verified: *verified,
                    }
                }
                app_core::browser_install::ProvisionPhase::Failed { step, detail } => {
                    workspace_model::BrowserInstallPhase::Failed {
                        step: step.clone(),
                        detail: detail.clone(),
                    }
                }
            },
        };
        let _ = self.app.emit("browser:install_progress", state);
    }
}
