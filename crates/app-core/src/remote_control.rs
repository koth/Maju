//! Transport-agnostic control surface for the mobile remote-control plane.
//!
//! The `RemoteControl` trait mirrors the operations the Tauri command bridge
//! performs today, taking and returning `workspace-model` DTOs only — no
//! Tauri, no WebSocket, no transport types — so it can be driven by the
//! local command bridge and the relay client interchangeably.
//!
//! Layering: the trait lives in `app-core` (where `Application` and the
//! session-lifecycle methods live). Operations that are per-active-session
//! (`get_state`, `send_prompt`, `resolve_permission`, `cancel`, `stop_tool`,
//! `create_session`, `switch_session`) are implemented here directly against
//! `Application`. `list_sessions` crosses the workspace-registry boundary
//! (it lives on `AppState` in the Tauri shell), so its implementation is
//! provided by the shell, not here.

use workspace_model::{
    AgentCliId, PermissionInputResponse, UiSnapshot, UserPromptContent, WorkspaceSessionList,
};

use crate::application::AppUpdate;

/// Transport-agnostic control over a running kodex session.
///
/// All methods are async so the same trait serves a synchronous local
/// command bridge and a network-bound relay client. Implementations hold
/// their own concurrency story (the local impl borrows `Application` behind
/// its mutex; the relay impl routes over a socket).
pub trait RemoteControl: Send + Sync {
    /// List sessions for the current workspace context.
    fn list_sessions(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<WorkspaceSessionList>, String>> + Send;

    /// Create a new session in the active workspace context.
    fn create_session(
        &self,
        workspace_root: Option<String>,
        agent: Option<AgentCliId>,
    ) -> impl std::future::Future<Output = Result<String, String>> + Send;

    /// Switch the active session.
    fn switch_session(
        &self,
        session_id: String,
        workspace_root: Option<String>,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send;

    /// Send a prompt to the active session.
    fn send_prompt(
        &self,
        prompt: Vec<UserPromptContent>,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send;

    /// Get the current full UI snapshot of the active session.
    fn get_state(&self) -> impl std::future::Future<Output = Result<UiSnapshot, String>> + Send;

    /// Resolve a pending permission request.
    fn resolve_permission(
        &self,
        request_id: String,
        option_id: Option<String>,
        guidance: Option<String>,
        input_response: Option<PermissionInputResponse>,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send;

    /// Cancel the in-progress prompt.
    fn cancel(&self) -> impl std::future::Future<Output = Result<(), String>> + Send;

    /// Stop a specific tool invocation.
    fn stop_tool(
        &self,
        tool_call_id: String,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send;

    /// Subscribe to UI/permission update signals (see `AppUpdate`).
    fn subscribe_updates(&self) -> tokio::sync::broadcast::Receiver<AppUpdate>;
}

/// Implementation of [`RemoteControl`] that bridges into an `Application`
/// held behind a mutex, plus a `list_sessions` callback supplied by the
/// shell (which owns the workspace registry).
///
/// `list_sessions` is injected because it crosses the workspace-registry
/// boundary that lives in the Tauri shell's `AppState`, while the rest of
/// the operations live on `Application`.
pub struct AppCoreRemoteControl<F>
where
    F: Fn() -> Result<Vec<WorkspaceSessionList>, String> + Send + Sync,
{
    app: std::sync::Arc<std::sync::Mutex<crate::Application>>,
    list_sessions_fn: F,
}

impl<F> AppCoreRemoteControl<F>
where
    F: Fn() -> Result<Vec<WorkspaceSessionList>, String> + Send + Sync,
{
    pub fn new(
        app: std::sync::Arc<std::sync::Mutex<crate::Application>>,
        list_sessions_fn: F,
    ) -> Self {
        Self {
            app,
            list_sessions_fn,
        }
    }

 fn with_app<R>(&self, f: impl FnOnce(&mut crate::Application) -> Result<R, String>) -> Result<R, String> {
 let mut app = self.app.lock().map_err(|e| format!("Application mutex poisoned: {e}"))?;
 f(&mut app)
 }
}

#[allow(dead_code)]
impl<F> AppCoreRemoteControl<F>
where
    F: Fn() -> Result<Vec<WorkspaceSessionList>, String> + Send + Sync,
{
    /// Send a prompt marked as remote-origin (destructive permissions will
    /// require explicit approval instead of auto-resolving). Used by tests
    /// and the relay path; the local command bridge uses `send_prompt`.
    pub fn send_prompt_remote(
        &self,
        prompt: Vec<UserPromptContent>,
    ) -> Result<(), String> {
        self.with_app(|app| {
            app.set_remote_mode(true);
            app.send_prompt_content_background(prompt).map_err(|e| e.to_string())
        })
    }

    /// Clear remote-origin marking (e.g. after a turn ends). Safe to call
    /// from a non-prompt context.
    pub fn clear_remote_mode(&self) -> Result<(), String> {
        self.with_app(|app| {
            app.set_remote_mode(false);
            Ok(())
        })
    }
}

impl<F> RemoteControl for AppCoreRemoteControl<F>
where
    F: Fn() -> Result<Vec<WorkspaceSessionList>, String> + Send + Sync,
{
 fn list_sessions(&self) -> impl std::future::Future<Output = Result<Vec<WorkspaceSessionList>, String>> + Send {
 let result = (self.list_sessions_fn)();
 async move { result }
 }

 fn create_session(
 &self,
 _workspace_root: Option<String>,
 agent: Option<AgentCliId>,
 ) -> impl std::future::Future<Output = Result<String, String>> + Send {
 let result = self.with_app(|app| {
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
 let result = self.with_app(|app| app.session_switch(&session_id));
 async move { result }
 }

 fn send_prompt(
 &self,
 prompt: Vec<UserPromptContent>,
 ) -> impl std::future::Future<Output = Result<(), String>> + Send {
 let result = self.with_app(|app| {
 app.send_prompt_content_background(prompt).map_err(|e| e.to_string())
 });
 async move { result }
 }

 fn get_state(&self) -> impl std::future::Future<Output = Result<UiSnapshot, String>> + Send {
 let result = self.with_app(|app| {
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
 let result = self.with_app(|app| {
 app.resolve_tool_permission(&request_id, option_id, guidance, input_response)
 });
 async move { result }
 }

 fn cancel(&self) -> impl std::future::Future<Output = Result<(), String>> + Send {
 let result = self.with_app(|app| app.cancel_prompt());
 async move { result }
 }

 fn stop_tool(
 &self,
 tool_call_id: String,
 ) -> impl std::future::Future<Output = Result<(), String>> + Send {
 let result = self.with_app(|app| app.stop_tool(&tool_call_id));
 async move { result }
 }

 fn subscribe_updates(&self) -> tokio::sync::broadcast::Receiver<AppUpdate> {
 let app = self.app.lock().expect("Application mutex poisoned");
 app.subscribe_updates()
 }
}
