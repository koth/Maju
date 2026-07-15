//! Shell-side implementation of `app_core::RemoteControl` backed by the
//! active workspace's `Application` (via `AppState`). This is the concrete
//! control surface the relay client (task 7+) drives when a phone connects.
//! Local Tauri commands keep calling `with_app` directly; this impl is the
//! remote contract, not a replacement for the local command bridge.

use app_core::{AppUpdate, RemoteControl};
use tauri::{AppHandle, Manager};
use workspace_model::{
    AgentCliId, PermissionInputResponse, UiSnapshot, UserPromptContent, WorkspaceSessionList,
};

use crate::state::AppState;

#[allow(dead_code)]
pub struct DesktopRemoteControl {
    app: AppHandle,
}

#[allow(dead_code)]
impl DesktopRemoteControl {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl RemoteControl for DesktopRemoteControl {
    fn list_sessions(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<WorkspaceSessionList>, String>> + Send {
        let result = self.app.state::<AppState>().list_workspace_sessions();
        async move { result }
    }

    fn create_session(
        &self,
        _workspace_root: Option<String>,
        agent: Option<AgentCliId>,
    ) -> impl std::future::Future<Output = Result<String, String>> + Send {
        let result = self.app.state::<AppState>().with_app(|app| {
            app.session_create(agent)?;
            Ok(app.ui.session.id.to_string())
        });
        async move { result }
    }

    fn switch_session(
        &self,
        session_id: String,
        _workspace_root: Option<String>,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send {
        let result = self
            .app
            .state::<AppState>()
            .with_app(|app| app.session_switch(&session_id));
        async move { result }
    }

    fn send_prompt(
        &self,
        prompt: Vec<UserPromptContent>,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send {
        let result = self
            .app
            .state::<AppState>()
            .with_app(|app| {
                app.set_remote_mode(true);
                app.send_prompt_content_background(prompt).map_err(|e| e.to_string())
            });
        async move { result }
    }

    fn get_state(&self) -> impl std::future::Future<Output = Result<UiSnapshot, String>> + Send {
        let result = self.app.state::<AppState>().with_app(|app| {
            app.poll_prompt_progress();
            Ok(app.lightweight_ui_snapshot())
        });
        async move { result }
    }

    fn resolve_permission(
        &self,
        request_id: String,
        option_id: Option<String>,
        guidance: Option<String>,
        input_response: Option<PermissionInputResponse>,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send {
        let result = self.app.state::<AppState>().with_app(|app| {
            app.resolve_tool_permission(&request_id, option_id, guidance, input_response)
        });
        async move { result }
    }

    fn cancel(&self) -> impl std::future::Future<Output = Result<(), String>> + Send {
        let result = self
            .app
            .state::<AppState>()
            .with_app(|app| app.cancel_prompt());
        async move { result }
    }

    fn stop_tool(
        &self,
        tool_call_id: String,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send {
        let result = self
            .app
            .state::<AppState>()
            .with_app(|app| app.stop_tool(&tool_call_id));
        async move { result }
    }

    fn subscribe_updates(&self) -> tokio::sync::broadcast::Receiver<AppUpdate> {
        self.app
            .state::<AppState>()
            .subscribe_active_updates()
            .ok()
            .flatten()
            .unwrap_or_else(|| tokio::sync::broadcast::channel(1).1)
    }
}
