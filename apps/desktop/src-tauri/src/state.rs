use app_core::Application;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct AppState {
    pub app: Mutex<Option<Application>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            app: Mutex::new(None),
        }
    }

    pub fn open_workspace(&self, path: PathBuf) -> Result<workspace_model::UiSnapshot, String> {
        let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
        let agent_command = app_core::settings::resolve_agent_command_with_settings(&paths);
        let application = Application::bootstrap_with_app_paths(path, agent_command, paths)
            .map_err(|e| e.to_string())?;
        let snapshot = application.ui.clone();
        let mut guard = self.app.lock().map_err(|e| e.to_string())?;
        *guard = Some(application);
        Ok(snapshot)
    }

    pub fn close_workspace(&self) -> Result<(), String> {
        let mut guard = self.app.lock().map_err(|e| e.to_string())?;
        *guard = None;
        Ok(())
    }

    pub fn with_app<F, R>(&self, f: F) -> Result<R, String>
    where
        F: FnOnce(&mut Application) -> Result<R, String>,
    {
        let mut guard = self.app.lock().map_err(|e| e.to_string())?;
        let app = guard.as_mut().ok_or("No workspace open")?;
        f(app)
    }
}
