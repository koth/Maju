use crate::open_workspaces::{OpenWorkspaceRecord, OpenWorkspaceState};
use app_core::{Application, normalize_tracked_path};
use session_store::SessionStore;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use workspace_model::{
    AgentCliId, OpenWorkspaceItem, SessionListItem, UiSnapshot, WorkspaceDescriptor,
    WorkspaceSessionList,
};

pub struct AppState {
    workspaces: Mutex<WorkspaceRegistry>,
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
        Self {
            workspaces: Mutex::new(WorkspaceRegistry::default()),
        }
    }

    pub fn open_workspace(
        &self,
        path: PathBuf,
        agent: Option<AgentCliId>,
    ) -> Result<UiSnapshot, String> {
        let key = workspace_key(&path);
        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        let snapshot = connect_workspace_locked(&mut guard, key.clone(), path, agent)?;
        guard.active_workspace = Some(key);
        Ok(snapshot)
    }

    pub fn restore_dormant_workspace(&self, path: PathBuf) -> Result<(), String> {
        let key = workspace_key(&path);
        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        if guard.workspaces.contains_key(&key) {
            return Ok(());
        }

        let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
        let store = SessionStore::open(paths.root(), &path).map_err(|e| e.to_string())?;
        let sessions = store.list_sessions().map_err(|e| e.to_string())?;
        let workspace = workspace_descriptor(&path);
        guard.workspaces.insert(
            key,
            WorkspaceEntry::Dormant(WorkspaceMetadata {
                workspace,
                sessions,
            }),
        );
        Ok(())
    }

    pub fn close_workspace(&self) -> Result<(), String> {
        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        let Some(active_key) = guard.active_workspace.clone() else {
            return Ok(());
        };

        guard.workspaces.remove(&active_key);
        let next_key = guard.workspaces.keys().next().cloned();
        guard.active_workspace = next_key.clone();
        if let Some(next_key) = next_key {
            if let Some(path) = entry_path(&guard.workspaces[&next_key]) {
                let _ = connect_workspace_locked(&mut guard, next_key, path, None);
            }
        }
        Ok(())
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
        let path = PathBuf::from(path);
        let key = workspace_key(&path);
        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        let snapshot = connect_workspace_locked(&mut guard, key.clone(), path, None)?;
        guard.active_workspace = Some(key);
        Ok(snapshot)
    }

    pub fn has_open_workspaces(&self) -> Result<bool, String> {
        let guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        Ok(!guard.workspaces.is_empty())
    }

    pub fn poll_all_and_get_active_snapshot(&self) -> Result<UiSnapshot, String> {
        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        for entry in guard.workspaces.values_mut() {
            if let WorkspaceEntry::Connected(app) = entry {
                app.poll_prompt_progress();
            }
        }
        let active_key = guard.active_workspace.clone().ok_or("No workspace open")?;
        let app = match guard.workspaces.get(&active_key) {
            Some(WorkspaceEntry::Connected(app)) => app,
            _ => return Err("No connected workspace open".into()),
        };
        Ok(app.ui.clone())
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
}

fn connect_workspace_locked(
    guard: &mut WorkspaceRegistry,
    key: String,
    path: PathBuf,
    agent: Option<AgentCliId>,
) -> Result<UiSnapshot, String> {
    if let Some(WorkspaceEntry::Connected(application)) = guard.workspaces.get(&key) {
        return Ok(application.ui.clone());
    }

    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    let agent_command = match agent {
        Some(agent) => app_core::settings::command_for_agent(agent)
            .unwrap_or_else(|| app_core::settings::resolve_agent_command_with_settings(&paths)),
        None => app_core::settings::resolve_agent_command_with_settings(&paths),
    };
    let application = Application::bootstrap_with_app_paths(path, agent_command, paths)
        .map_err(|e| e.to_string())?;
    let snapshot = application.ui.clone();
    guard
        .workspaces
        .insert(key, WorkspaceEntry::Connected(application));
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
        .unwrap_or_else(uuid::Uuid::new_v4)
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
