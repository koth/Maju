//! Shell-side implementation of `app_core::RemoteControl` backed by the
//! active workspace's `Application` (via `AppState`). This is the concrete
//! control surface the relay client (task 7+) drives when a phone connects.
//! Local Tauri commands keep calling `with_app` directly; this impl is the
//! remote contract, not a replacement for the local command bridge.

use app_core::{AppUpdate, RemoteControl};
use tauri::{AppHandle, Manager};
use workspace_model::{
    AgentCliId, AgentOptionEntry, AgentOptionsList, PermissionInputResponse, SessionConfigState,
    SessionFileChange, UserPromptContent, WorkspaceSessionList,
};

use crate::state::AppState;

#[derive(Clone)]
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
        workspace_root: Option<String>,
        agent: Option<AgentCliId>,
        preset: Option<String>,
    ) -> impl std::future::Future<Output = Result<String, String>> + Send {
        // Honor the phone-supplied workspace root: session creation must land
        // in the workspace the user picked on the phone, not whichever
        // workspace happens to be active on the desktop (mirrors the local
        // `session_create` command's `with_workspace_app` routing).
        let result = self.app.state::<AppState>().with_workspace_app(workspace_root, |app| {
            app.session_create(agent, preset)?;
            Ok(app.ui.session.id.to_string())
        });
        async move { result }
    }

    fn set_config_control(
        &self,
        control_id: String,
        value_id: String,
        provider: Option<String>,
    ) -> impl std::future::Future<Output = Result<SessionConfigState, String>> + Send {
        let result = self
            .app
            .state::<AppState>()
            .with_app(|app| app.set_session_config_control(&control_id, &value_id, provider.as_deref()));
        async move { result }
    }

    fn switch_session(
        &self,
        session_id: String,
        workspace_root: Option<String>,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send {
        // Route through the session's OWN workspace: resuming a session
        // against the currently-active workspace makes the dsh harness
        // reject the resume with `session-conflict` (the persisted session's
        // cwd differs from the requested one). The phone sends the root from
        // the `ListSessions` grouping; the local `session_switch` command
        // does the same via `with_workspace_app`.
        let result = self
            .app
            .state::<AppState>()
            .with_workspace_app(workspace_root, |app| app.session_switch(&session_id));
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
                app.send_prompt_content_background(prompt)
                    .map(|_outcome| ())
                    .map_err(|e| e.to_string())
            });
        async move { result }
    }

    fn get_state(
        &self,
        known: Option<(String, u64)>,
    ) -> impl std::future::Future<Output = Result<app_core::RemoteGetState, String>> + Send {
        let result = self
            .app
            .state::<AppState>()
            .with_app(|app| app.remote_get_state(known));
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

    fn get_file_diff(
        &self,
        message_id: String,
        path: String,
    ) -> impl std::future::Future<Output = Result<SessionFileChange, String>> + Send {
        let result = self
            .app
            .state::<AppState>()
            .with_app(|app| app.session_turn_file_diff(&message_id, &path));
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

/// Answer the phone's `ListAgentOptions`: the selectable agents (from the
/// settings snapshot) plus the DeepSeek Harness preset list. The preset
/// list is best-effort — it spawns the `dsh web` host on demand and can
/// block for seconds, so it runs off the async runtime, and a failure
/// degrades to an empty list (the phone then offers only the deployment
/// default) instead of failing the whole request.
pub async fn list_agent_options() -> Result<AgentOptionsList, String> {
    tokio::task::spawn_blocking(|| {
        let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
        let snapshot = app_core::settings::settings_snapshot(&paths);
        let agents = snapshot
            .agents
            .iter()
            .map(|agent| AgentOptionEntry {
                id: agent.id,
                label: agent.label.clone(),
                installed: agent.installed,
                selected: agent.selected,
            })
            .collect();
        let dsh_presets = dsh_preset_options(&paths).unwrap_or_default();
        Ok(AgentOptionsList {
            agents,
            dsh_presets,
            dsh_default_preset: snapshot.settings.dsh_default_preset,
        })
    })
    .await
    .map_err(|e| format!("list agent options task failed: {e}"))?
}

fn dsh_preset_options(
    paths: &app_core::AppPaths,
) -> Result<Vec<workspace_model::DshPresetOption>, String> {
    let host = app_core::dsh_bringup::dsh_bringup().ensure_harness_host(paths)?;
    let client = host.client().clone();
    let value = host
        .runtime()
        .block_on(client.agent_preset_list(uuid::Uuid::new_v4().to_string()))
        .map_err(|e| format!("agentPreset.list failed: {e}"))?;
    Ok(value
        .presets
        .iter()
        .map(|preset| workspace_model::DshPresetOption {
            id: preset.id.clone(),
            label: preset.name.clone().unwrap_or_else(|| preset.id.clone()),
            description: preset.description.clone(),
        })
        .collect())
}
