use crate::lsp::LspService;
use crate::open_workspaces::{OpenWorkspaceRecord, OpenWorkspaceState};
use app_core::{Application, UiPatchCursor, UiSnapshotUpdate, normalize_tracked_path};
use session_store::SessionStore;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use terminal_service::{TerminalEventSink, TerminalService};
use workspace_model::{
    AgentCliId, OpenWorkspaceItem, SessionListItem, TerminalOpenRequest, TerminalResizeRequest,
    TerminalSession, TerminalWriteRequest, UiSnapshot, WorkspaceDescriptor, WorkspaceSessionList,
};

pub struct AppState {
    workspaces: Mutex<WorkspaceRegistry>,
    lsp_service: LspService,
    terminal_service: TerminalService,
}

#[derive(Default)]
struct WorkspaceRegistry {
    workspaces: HashMap<String, WorkspaceEntry>,
    active_workspace: Option<String>,
}

enum WorkspaceEntry {
    Connected(Application),
    Dormant(WorkspaceMetadata),
}

struct WorkspaceMetadata {
    workspace: WorkspaceDescriptor,
    sessions: Vec<SessionListItem>,
}

impl AppState {
    pub fn new() -> Self {
        app_core::startup_perf::mark("state/new", "");
        let lsp_service =
            app_core::startup_perf::measure("state/new_lsp_service", "", LspService::new);
        let terminal_service = TerminalService::new();
        Self {
            workspaces: Mutex::new(WorkspaceRegistry::default()),
            lsp_service,
            terminal_service,
        }
    }

    pub fn set_terminal_event_sink(&self, sink: TerminalEventSink) {
        self.terminal_service.set_event_sink(sink);
    }

    pub fn open_workspace(
        &self,
        path: PathBuf,
        agent: Option<AgentCliId>,
    ) -> Result<UiSnapshot, String> {
        app_core::startup_perf::mark("state/open_workspace/start", path.display().to_string());
        let key = workspace_key(&path);
        let mut guard = app_core::startup_perf::measure("state/open_workspace/lock", &key, || {
            self.workspaces.lock().map_err(|e| e.to_string())
        })?;
        let snapshot =
            app_core::startup_perf::measure("state/open_workspace/connect", &key, || {
                connect_workspace_locked(&mut guard, key.clone(), path, agent)
            })?;
        guard.active_workspace = Some(key);
        app_core::startup_perf::mark("state/open_workspace/end", "");
        Ok(snapshot)
    }

    pub fn restore_dormant_workspace(&self, path: PathBuf) -> Result<(), String> {
        app_core::startup_perf::mark(
            "state/restore_dormant_workspace/start",
            path.display().to_string(),
        );
        let key = workspace_key(&path);
        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        if guard.workspaces.contains_key(&key) {
            app_core::startup_perf::mark("state/restore_dormant_workspace/end", "already_open");
            return Ok(());
        }

        let workspace = workspace_descriptor(&path);
        let sessions = load_lightweight_sessions(&path).unwrap_or_default();
        let log_key = key.clone();
        guard.workspaces.insert(
            key,
            WorkspaceEntry::Dormant(WorkspaceMetadata {
                workspace,
                sessions,
            }),
        );
        app_core::startup_perf::mark("state/restore_dormant_workspace/end", &log_key);
        Ok(())
    }

    pub fn close_workspace(&self) -> Result<(), String> {
        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        let Some(active_key) = guard.active_workspace.clone() else {
            return Ok(());
        };

        let closing_root = guard.workspaces.get(&active_key).and_then(entry_path);
        guard.workspaces.remove(&active_key);
        if let Some(root) = closing_root {
            self.lsp_service.shutdown_workspace(&root);
            self.terminal_service.shutdown_workspace(&root);
        }
        let next_key = guard.workspaces.keys().next().cloned();
        guard.active_workspace = next_key.clone();
        if let Some(next_key) = next_key {
            if let Some(path) = entry_path(&guard.workspaces[&next_key]) {
                let _ = connect_workspace_locked(&mut guard, next_key, path, None);
            }
        }
        Ok(())
    }

    pub fn shutdown_all(&self) {
        if let Ok(mut guard) = self.workspaces.lock() {
            guard.workspaces.clear();
            guard.active_workspace = None;
        }
        self.lsp_service.shutdown_all();
        self.terminal_service.shutdown_all();
    }

    pub fn lsp_service(&self) -> LspService {
        self.lsp_service.clone()
    }

    pub fn terminal_open(&self, request: TerminalOpenRequest) -> Result<TerminalSession, String> {
        let path = self.resolve_terminal_workspace(request.workspace_root)?;
        if request.force_new {
            self.terminal_service
                .open_workspace_new(path, request.cols, request.rows)
                .map_err(|e| e.to_string())
        } else {
            self.terminal_service
                .open_workspace(path, request.cols, request.rows)
                .map_err(|e| e.to_string())
        }
    }

    pub fn terminal_write(&self, request: TerminalWriteRequest) -> Result<(), String> {
        self.terminal_service
            .write(&request.terminal_id, &request.data)
            .map_err(|e| e.to_string())
    }

    pub fn terminal_resize(
        &self,
        request: TerminalResizeRequest,
    ) -> Result<TerminalSession, String> {
        self.terminal_service
            .resize(&request.terminal_id, request.cols, request.rows)
            .map_err(|e| e.to_string())
    }

    pub fn terminal_terminate(&self, terminal_id: &str) -> Result<(), String> {
        self.terminal_service
            .terminate(terminal_id)
            .map_err(|e| e.to_string())
    }

    pub fn terminal_restart(
        &self,
        request: TerminalResizeRequest,
    ) -> Result<TerminalSession, String> {
        self.terminal_service
            .restart(&request.terminal_id, request.cols, request.rows)
            .map_err(|e| e.to_string())
    }

    pub fn terminal_list(
        &self,
        workspace_root: Option<String>,
    ) -> Result<Vec<TerminalSession>, String> {
        let path = self.resolve_terminal_workspace(workspace_root)?;
        self.terminal_service
            .list_workspace(path)
            .map_err(|e| e.to_string())
    }

    fn resolve_terminal_workspace(
        &self,
        workspace_root: Option<String>,
    ) -> Result<PathBuf, String> {
        if let Some(path) = workspace_root {
            return Ok(PathBuf::from(path));
        }
        let guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        let active_key = guard
            .active_workspace
            .as_deref()
            .ok_or("No workspace open")?;
        entry_path(
            guard
                .workspaces
                .get(active_key)
                .ok_or("Workspace is not open")?,
        )
        .ok_or_else(|| "Workspace is not open".into())
    }

    pub fn list_open_workspaces(&self) -> Result<Vec<OpenWorkspaceItem>, String> {
        let guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        Ok(open_workspace_items(&guard))
    }

    pub fn open_workspace_state(&self) -> Result<OpenWorkspaceState, String> {
        let items = self.list_open_workspaces()?;
        let active_path = items
            .iter()
            .find(|item| item.is_active)
            .map(|item| item.workspace.root.display().to_string());
        let workspaces = items
            .into_iter()
            .map(|item| OpenWorkspaceRecord {
                path: item.workspace.root.display().to_string(),
            })
            .collect();
        Ok(OpenWorkspaceState {
            active_path,
            workspaces,
        })
    }

    pub fn list_workspace_sessions(&self) -> Result<Vec<WorkspaceSessionList>, String> {
        let guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        let active = guard.active_workspace.as_deref();
        let mut items = guard
            .workspaces
            .iter()
            .map(|(key, entry)| workspace_session_list(key, entry, Some(key.as_str()) == active))
            .collect::<Result<Vec<_>, _>>()?;
        sort_workspaces(
            &mut items,
            |item| item.workspace.name.as_str(),
            |item| item.is_active,
        );
        Ok(items)
    }

    pub fn set_active_workspace(&self, path: String) -> Result<UiSnapshot, String> {
        app_core::startup_perf::mark("state/set_active_workspace/start", &path);
        let path = PathBuf::from(path);
        let key = workspace_key(&path);
        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        let snapshot =
            app_core::startup_perf::measure("state/set_active_workspace/connect", &key, || {
                connect_workspace_locked(&mut guard, key.clone(), path, None)
            })?;
        guard.active_workspace = Some(key);
        app_core::startup_perf::mark("state/set_active_workspace/end", "");
        Ok(snapshot)
    }

    pub fn has_open_workspaces(&self) -> Result<bool, String> {
        let guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        Ok(!guard.workspaces.is_empty())
    }

    pub fn has_running_codex_acp_session(&self) -> Result<bool, String> {
        let guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        Ok(guard.workspaces.values().any(|entry| match entry {
            WorkspaceEntry::Connected(app) => app.has_running_codex_acp_session(),
            WorkspaceEntry::Dormant(_) => false,
        }))
    }

    pub fn poll_active_and_get_snapshot_since(
        &self,
        last_revision: u64,
    ) -> Result<Option<UiSnapshot>, String> {
        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        let active_key = guard.active_workspace.clone().ok_or("No workspace open")?;
        let app = match guard.workspaces.get_mut(&active_key) {
            Some(WorkspaceEntry::Connected(app)) => app,
            _ => return Err("No connected workspace open".into()),
        };
        app.poll_prompt_progress();
        if app.ui.revision == last_revision {
            Ok(None)
        } else {
            Ok(Some(app.lightweight_ui_snapshot()))
        }
    }

    pub fn poll_active_and_get_update(
        &self,
        cursor: &mut UiPatchCursor,
    ) -> Result<Option<UiSnapshotUpdate>, String> {
        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        let active_key = guard.active_workspace.clone().ok_or("No workspace open")?;
        let app = match guard.workspaces.get_mut(&active_key) {
            Some(WorkspaceEntry::Connected(app)) => app,
            _ => return Err("No connected workspace open".into()),
        };
        app.poll_prompt_progress();
        Ok(app.lightweight_ui_update(cursor))
    }

    pub fn with_app<F, R>(&self, f: F) -> Result<R, String>
    where
        F: FnOnce(&mut Application) -> Result<R, String>,
    {
        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        let active_key = guard.active_workspace.clone().ok_or("No workspace open")?;
        let app = match guard.workspaces.get_mut(&active_key) {
            Some(WorkspaceEntry::Connected(app)) => app,
            _ => return Err("No connected workspace open".into()),
        };
        f(app)
    }

    pub fn with_workspace_app<F, R>(&self, path: Option<String>, f: F) -> Result<R, String>
    where
        F: FnOnce(&mut Application) -> Result<R, String>,
    {
        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        let key = match path {
            Some(path) => workspace_key(&PathBuf::from(path)),
            None => guard.active_workspace.clone().ok_or("No workspace open")?,
        };
        let path = entry_path(guard.workspaces.get(&key).ok_or("Workspace is not open")?)
            .ok_or("Workspace is not open")?;
        connect_workspace_locked(&mut guard, key.clone(), path, None)?;
        guard.active_workspace = Some(key.clone());
        let app = match guard.workspaces.get_mut(&key) {
            Some(WorkspaceEntry::Connected(app)) => app,
            _ => return Err("Workspace is not connected".into()),
        };
        f(app)
    }

    pub fn delete_session(&self, workspace_root: Option<String>, id: &str) -> Result<(), String> {
        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        let key = match workspace_root {
            Some(path) => workspace_key(&PathBuf::from(path)),
            None => guard.active_workspace.clone().ok_or("No workspace open")?,
        };
        let is_active_workspace = guard.active_workspace.as_deref() == Some(key.as_str());

        if is_active_workspace {
            let path = entry_path(guard.workspaces.get(&key).ok_or("Workspace is not open")?)
                .ok_or("Workspace is not open")?;
            connect_workspace_locked(&mut guard, key.clone(), path, None)?;
            let app = match guard.workspaces.get_mut(&key) {
                Some(WorkspaceEntry::Connected(app)) => app,
                _ => return Err("Workspace is not connected".into()),
            };
            return app.session_delete(id);
        }

        let entry = guard
            .workspaces
            .get_mut(&key)
            .ok_or("Workspace is not open")?;
        match entry {
            WorkspaceEntry::Connected(app) => app.session_delete(id),
            WorkspaceEntry::Dormant(metadata) => {
                let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
                let store = SessionStore::open(paths.root(), &metadata.workspace.root)
                    .map_err(|e| e.to_string())?;
                store.delete_session(id).map_err(|e| e.to_string())?;
                metadata.sessions = load_lightweight_sessions(&metadata.workspace.root)?;
                Ok(())
            }
        }
    }
}

fn connect_workspace_locked(
    guard: &mut WorkspaceRegistry,
    key: String,
    path: PathBuf,
    agent: Option<AgentCliId>,
) -> Result<UiSnapshot, String> {
    app_core::startup_perf::mark("state/connect_workspace/start", &key);
    if let Some(WorkspaceEntry::Connected(application)) = guard.workspaces.get(&key) {
        return app_core::startup_perf::measure(
            "state/connect_workspace/snapshot_existing",
            &key,
            || Ok(application.lightweight_ui_snapshot()),
        );
    }

    let paths = app_core::startup_perf::measure("state/connect_workspace/app_paths", &key, || {
        app_core::AppPaths::resolve().map_err(|e| e.to_string())
    })?;
    let agent_command = app_core::startup_perf::measure(
        "state/connect_workspace/resolve_agent_command",
        &key,
        || match agent {
            Some(agent) => app_core::settings::command_for_agent_with_paths(agent, &paths)
                .unwrap_or_else(|| app_core::settings::resolve_agent_command_with_settings(&paths)),
            None => app_core::settings::resolve_agent_command_with_settings(&paths),
        },
    );
    let application = app_core::startup_perf::measure(
        "state/connect_workspace/application_bootstrap",
        &key,
        || {
            Application::bootstrap_with_app_paths(path, agent_command, paths)
                .map_err(|e| e.to_string())
        },
    )?;
    let snapshot = app_core::startup_perf::measure(
        "state/connect_workspace/lightweight_snapshot",
        &key,
        || application.lightweight_ui_snapshot(),
    );
    guard
        .workspaces
        .insert(key, WorkspaceEntry::Connected(application));
    app_core::startup_perf::mark("state/connect_workspace/end", "");
    Ok(snapshot)
}

fn workspace_session_list(
    key: &str,
    entry: &WorkspaceEntry,
    is_active: bool,
) -> Result<WorkspaceSessionList, String> {
    match entry {
        WorkspaceEntry::Connected(app) => app.session_list().map(|sessions| WorkspaceSessionList {
            workspace: app.ui.workspace.clone(),
            sessions,
            active_session_id: app.ui.session.id,
            is_active,
            connected: true,
        }),
        WorkspaceEntry::Dormant(metadata) => Ok(WorkspaceSessionList {
            workspace: metadata.workspace.clone(),
            sessions: metadata.sessions.clone(),
            active_session_id: active_session_id(&metadata.sessions),
            is_active,
            connected: false,
        }),
    }
    .map_err(|e| format!("{key}: {e}"))
}

fn load_lightweight_sessions(path: &Path) -> Result<Vec<SessionListItem>, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    SessionStore::open(paths.root(), path)
        .and_then(|store| store.list_session_summaries())
        .map_err(|e| e.to_string())
}

fn open_workspace_items(guard: &WorkspaceRegistry) -> Vec<OpenWorkspaceItem> {
    let active = guard.active_workspace.as_deref();
    let mut items = guard
        .workspaces
        .iter()
        .map(|(key, entry)| match entry {
            WorkspaceEntry::Connected(app) => OpenWorkspaceItem {
                workspace: app.ui.workspace.clone(),
                active_session_id: app.ui.session.id,
                session_count: app
                    .session_list()
                    .map(|sessions| sessions.len())
                    .unwrap_or(0),
                is_active: Some(key.as_str()) == active,
                connected: true,
            },
            WorkspaceEntry::Dormant(metadata) => OpenWorkspaceItem {
                workspace: metadata.workspace.clone(),
                active_session_id: active_session_id(&metadata.sessions),
                session_count: metadata.sessions.len(),
                is_active: Some(key.as_str()) == active,
                connected: false,
            },
        })
        .collect::<Vec<_>>();
    sort_workspaces(
        &mut items,
        |item| item.workspace.name.as_str(),
        |item| item.is_active,
    );
    items
}

fn active_session_id(sessions: &[SessionListItem]) -> uuid::Uuid {
    sessions
        .first()
        .and_then(|session| uuid::Uuid::parse_str(&session.id).ok())
        .unwrap_or_else(uuid::Uuid::nil)
}

fn entry_path(entry: &WorkspaceEntry) -> Option<PathBuf> {
    Some(match entry {
        WorkspaceEntry::Connected(app) => app.ui.workspace.root.clone(),
        WorkspaceEntry::Dormant(metadata) => metadata.workspace.root.clone(),
    })
}

fn workspace_descriptor(workspace_root: &Path) -> WorkspaceDescriptor {
    WorkspaceDescriptor {
        id: uuid::Uuid::new_v4(),
        name: workspace_root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("工作区")
            .to_string(),
        root: workspace_root.to_path_buf(),
    }
}

fn sort_workspaces<T, F, G>(items: &mut [T], name: F, is_active: G)
where
    F: Fn(&T) -> &str,
    G: Fn(&T) -> bool,
{
    items.sort_by(|a, b| name(a).cmp(name(b)));
    items.sort_by_key(|item| !is_active(item));
}

fn workspace_key(path: &PathBuf) -> String {
    normalize_tracked_path(&path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dormant_workspace_session_list_exposes_names_without_connection() {
        let workspace = WorkspaceDescriptor {
            id: uuid::Uuid::new_v4(),
            name: "Dormant".into(),
            root: PathBuf::from("D:/work/Dormant"),
        };
        let session = SessionListItem {
            id: uuid::Uuid::new_v4().to_string(),
            title: "Stored session".into(),
            status: "Idle".into(),
            created_at: "1".into(),
            updated_at: "2".into(),
            message_count: 0,
            acp_session_id: None,
            agent_cli: None,
        };
        let entry = WorkspaceEntry::Dormant(WorkspaceMetadata {
            workspace,
            sessions: vec![session.clone()],
        });

        let list = workspace_session_list("dormant", &entry, false).unwrap();

        assert!(!list.connected);
        assert_eq!(list.sessions.len(), 1);
        assert_eq!(list.sessions[0].title, "Stored session");
        assert_eq!(list.sessions[0].message_count, 0);
    }
}
