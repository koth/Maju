use crate::codebuddy_proxy::CodebuddyProxyManager;
use crate::lsp::LspService;
use crate::open_workspaces::{OpenWorkspaceRecord, OpenWorkspaceState};
use crate::remote_control_manager::RemoteControlManager;
use app_core::{
    AppUpdate, Application, UiPatchCursor, UiSnapshotUpdate, normalize_tracked_path,
};
use session_store::SessionStore;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use terminal_service::{TerminalEventSink, TerminalService};
use workspace_model::{
    AgentCliId, EditorFileSnapshot, FileEntry, OpenWorkspaceItem, RemoteLinuxWorkspace,
    RepositorySnapshot, SessionListItem, TerminalOpenRequest, TerminalResizeRequest,
    TerminalSession, TerminalWriteRequest, UiSnapshot, WorkspaceDescriptor, WorkspaceLocation,
    WorkspaceSessionList,
};

use std::sync::Arc;

/// Resolve the project-less "聊天" workspace root (`~/.kodex/chats`).
/// Sessions created here are not bound to a real project directory.
fn chats_workspace_root() -> Result<PathBuf, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    Ok(paths.chats_workspace_root())
}

/// Whether `identifier` refers to the project-less chats workspace.
fn is_chats_workspace(identifier: &str) -> bool {
    let Ok(root) = chats_workspace_root() else {
        return false;
    };
    let normalized_target = normalize_tracked_path(identifier);
    let normalized_root = normalize_tracked_path(&root.display().to_string());
    normalized_target == normalized_root
}

pub struct AppState {
    workspaces: Mutex<WorkspaceRegistry>,
    lsp_service: LspService,
    terminal_service: TerminalService,
    codebuddy_proxy: Arc<CodebuddyProxyManager>,
    remote_control: Arc<RemoteControlManager>,
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

enum ResolvedTerminalWorkspace {
    Local(PathBuf),
    Remote(RemoteLinuxWorkspace),
}

impl AppState {
    pub fn new() -> Self {
        app_core::startup_perf::mark("state/new", "");
        let lsp_service =
            app_core::startup_perf::measure("state/new_lsp_service", "", LspService::new);
        let terminal_service = TerminalService::new();
        let remote_control = Arc::new(RemoteControlManager::new(
            app_core::AppPaths::resolve().unwrap_or_else(|_| app_core::AppPaths::from_root(
                std::env::current_dir().unwrap_or_default().join(".kodex"),
            )),
        ));
        Self {
            workspaces: Mutex::new(WorkspaceRegistry::default()),
            lsp_service,
            terminal_service,
            codebuddy_proxy: Arc::new(CodebuddyProxyManager::new()),
            remote_control,
        }
    }

    pub fn codebuddy_proxy(&self) -> Arc<CodebuddyProxyManager> {
        self.codebuddy_proxy.clone()
    }

    pub fn remote_control(&self) -> Arc<RemoteControlManager> {
        self.remote_control.clone()
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

    pub fn open_remote_linux_workspace(
        &self,
        remote: RemoteLinuxWorkspace,
    ) -> Result<UiSnapshot, String> {
        if remote.ssh_target.trim().is_empty() {
            return Err("SSH target cannot be empty".into());
        }
        if remote.ssh_port == Some(0) {
            return Err("SSH port must be between 1 and 65535".into());
        }
        if !remote.remote_path.trim().starts_with('/') {
            return Err("Remote workspace path must be absolute".into());
        }
        let key = remote_workspace_key(&remote);
        {
            let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
            if let Some(WorkspaceEntry::Connected(application)) = guard.workspaces.get(&key) {
                let snapshot = application.lightweight_ui_snapshot();
                guard.active_workspace = Some(key);
                return Ok(snapshot);
            }
        }

        let (application, snapshot) = build_remote_workspace_application(key.as_str(), remote)?;
        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        if let Some(WorkspaceEntry::Connected(application)) = guard.workspaces.get(&key) {
            let snapshot = application.lightweight_ui_snapshot();
            guard.active_workspace = Some(key);
            return Ok(snapshot);
        }
        guard
            .workspaces
            .insert(key.clone(), WorkspaceEntry::Connected(application));
        guard.active_workspace = Some(key);
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

    pub fn restore_dormant_remote_workspace(
        &self,
        remote: RemoteLinuxWorkspace,
    ) -> Result<(), String> {
        let key = remote_workspace_key(&remote);
        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        if guard.workspaces.contains_key(&key) {
            return Ok(());
        }
        let workspace = remote_workspace_descriptor(remote);
        let sessions = load_lightweight_sessions(&workspace.root).unwrap_or_default();
        guard.workspaces.insert(
            key,
            WorkspaceEntry::Dormant(WorkspaceMetadata {
                workspace,
                sessions,
            }),
        );
        Ok(())
    }

    pub fn restore_active_dormant_remote_workspace(
        &self,
        remote: RemoteLinuxWorkspace,
    ) -> Result<UiSnapshot, String> {
        let key = remote_workspace_key(&remote);
        let snapshot = app_core::build_dormant_remote_workspace_ui(remote.clone())?;
        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        if !guard.workspaces.contains_key(&key) {
            let workspace = remote_workspace_descriptor(remote);
            let sessions = load_lightweight_sessions(&workspace.root).unwrap_or_default();
            guard.workspaces.insert(
                key.clone(),
                WorkspaceEntry::Dormant(WorkspaceMetadata {
                    workspace,
                    sessions,
                }),
            );
        }
        guard.active_workspace = Some(key);
        Ok(snapshot)
    }

    pub fn close_workspace(&self) -> Result<(), String> {
        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        let Some(active_key) = guard.active_workspace.clone() else {
            return Ok(());
        };

        let closing_entry_remote = guard
            .workspaces
            .get(&active_key)
            .map(entry_is_remote)
            .unwrap_or(false);
        let closing_root = guard.workspaces.get(&active_key).and_then(entry_path);
        let closing_remote_key = closing_entry_remote.then(|| active_key.clone());
        guard.workspaces.remove(&active_key);
        if let Some(remote_key) = closing_remote_key {
            self.terminal_service.shutdown_workspace_key(&remote_key);
        } else if let Some(root) = closing_root {
            self.lsp_service.shutdown_workspace(&root);
            self.terminal_service.shutdown_workspace(&root);
        }
        let next_key = guard.workspaces.keys().next().cloned();
        guard.active_workspace = next_key.clone();
        if let Some(next_key) = next_key {
            let (remote, path) = {
                let entry = &guard.workspaces[&next_key];
                (entry_remote(entry), entry_path(entry))
            };
            if let Some(remote) = remote {
                let _ = connect_remote_workspace_locked(&mut guard, next_key, remote);
            } else if let Some(path) = path {
                let _ = connect_workspace_locked(&mut guard, next_key, path, None);
            }
        }
        Ok(())
    }

    pub fn archive_workspace(&self, path: String) -> Result<Option<UiSnapshot>, String> {
        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        let key = workspace_key_for_identifier(&guard, &path)
            .ok_or_else(|| format!("Workspace is not open: {path}"))?;
        let is_active_workspace = guard.active_workspace.as_deref() == Some(key.as_str());
        let workspace_root = {
            let entry = guard
                .workspaces
                .get_mut(&key)
                .ok_or_else(|| format!("Workspace is not open: {path}"))?;
            let workspace_root = entry_workspace_root(entry);
            archive_workspace_sessions(&workspace_root)?;
            if let WorkspaceEntry::Dormant(metadata) = entry {
                metadata.sessions.clear();
            }
            workspace_root
        };
        let entry = guard
            .workspaces
            .remove(&key)
            .ok_or_else(|| format!("Workspace is not open: {path}"))?;
        app_core::startup_perf::mark(
            "state/archive_workspace",
            workspace_root.display().to_string(),
        );
        self.shutdown_workspace_entry(&key, &entry);
        drop(entry);

        if !is_active_workspace {
            return Ok(None);
        }

        let next_key = guard.workspaces.keys().next().cloned();
        let Some(next_key) = next_key else {
            guard.active_workspace = None;
            return Ok(None);
        };

        activate_workspace_locked(&mut guard, next_key).map(Some)
    }

    fn shutdown_workspace_entry(&self, key: &str, entry: &WorkspaceEntry) {
        if let Some(remote) = entry_remote(entry) {
            self.terminal_service
                .shutdown_workspace_key(&remote_workspace_key(&remote));
            return;
        }
        if let Some(root) = entry_path(entry) {
            self.lsp_service.shutdown_workspace(&root);
            self.terminal_service.shutdown_workspace(&root);
        } else {
            self.terminal_service.shutdown_workspace_key(key);
        }
    }

    pub fn shutdown_all(&self) {
        let workspaces = self.workspaces.lock().ok().map(|mut guard| {
            guard.active_workspace = None;
            std::mem::take(&mut guard.workspaces)
        });
        drop(workspaces);
        self.lsp_service.shutdown_all();
        self.terminal_service.shutdown_all();
        self.codebuddy_proxy.stop();
    }

    pub fn lsp_service(&self) -> LspService {
        self.lsp_service.clone()
    }

    pub fn terminal_open(&self, request: TerminalOpenRequest) -> Result<TerminalSession, String> {
        match self.resolve_terminal_workspace(request.workspace_root)? {
            ResolvedTerminalWorkspace::Local(path) => {
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
            ResolvedTerminalWorkspace::Remote(remote) => {
                if request.force_new {
                    self.terminal_service
                        .open_remote_workspace_new(&remote, request.cols, request.rows)
                        .map_err(|e| e.to_string())
                } else {
                    self.terminal_service
                        .open_remote_workspace(&remote, request.cols, request.rows)
                        .map_err(|e| e.to_string())
                }
            }
        }
    }

    pub fn terminal_write(&self, request: TerminalWriteRequest) -> Result<(), String> {
        self.terminal_service
            .write(&request.terminal_id, &request.data)
            .map_err(|e| e.to_string())
    }

    pub fn terminal_scrollback(&self, terminal_id: &str) -> Result<String, String> {
        self.terminal_service
            .scrollback(terminal_id)
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
        match self.resolve_terminal_workspace(workspace_root)? {
            ResolvedTerminalWorkspace::Local(path) => self
                .terminal_service
                .list_workspace(path)
                .map_err(|e| e.to_string()),
            ResolvedTerminalWorkspace::Remote(remote) => self
                .terminal_service
                .list_remote_workspace(&remote)
                .map_err(|e| e.to_string()),
        }
    }

    fn resolve_terminal_workspace(
        &self,
        workspace_root: Option<String>,
    ) -> Result<ResolvedTerminalWorkspace, String> {
        let guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        if let Some(path) = workspace_root {
            if let Some(key) = workspace_key_for_identifier(&guard, &path) {
                let entry = guard.workspaces.get(&key).ok_or("Workspace is not open")?;
                return terminal_workspace_from_entry(entry);
            }
            return Ok(ResolvedTerminalWorkspace::Local(PathBuf::from(path)));
        }
        let active_key = guard
            .active_workspace
            .as_deref()
            .ok_or("No workspace open")?;
        let entry = guard
            .workspaces
            .get(active_key)
            .ok_or("Workspace is not open")?;
        terminal_workspace_from_entry(entry)
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
            .map(open_workspace_record_path);
        let workspaces = items
            .into_iter()
            .map(|item| OpenWorkspaceRecord {
                path: open_workspace_record_path(&item),
                remote: match item.workspace.location {
                    WorkspaceLocation::RemoteLinux(remote) => {
                        Some(remote_workspace_for_storage(remote))
                    }
                    WorkspaceLocation::Local => None,
                },
            })
            .collect();
        Ok(OpenWorkspaceState {
            active_path,
            workspaces,
        })
    }

    pub fn list_workspace_sessions(&self) -> Result<Vec<WorkspaceSessionList>, String> {
        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        let active = guard.active_workspace.clone();
        let mut items = guard
            .workspaces
            .iter_mut()
            .map(|(key, entry)| workspace_session_list(key, entry, Some(key) == active.as_ref()))
            .collect::<Result<Vec<_>, _>>()?;
        sort_workspaces(&mut items, |item| item.workspace.name.as_str());
        Ok(items)
    }

    pub fn set_active_workspace(&self, path: String) -> Result<UiSnapshot, String> {
        app_core::startup_perf::mark("state/set_active_workspace/start", &path);
        let existing = {
            let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
            if let Some(key) = workspace_key_for_identifier(&guard, &path) {
                match guard.workspaces.get(&key).ok_or("Workspace is not open")? {
                    WorkspaceEntry::Connected(application) => {
                        let snapshot = application.lightweight_ui_snapshot();
                        guard.active_workspace = Some(key);
                        app_core::startup_perf::mark("state/set_active_workspace/end", "");
                        return Ok(snapshot);
                    }
                    entry => Some((key, entry_remote(entry), entry_path(entry))),
                }
            } else {
                None
            }
        };

        if let Some((key, remote, local_path)) = existing {
            if let Some(remote) = remote {
                let (application, snapshot) =
                    build_remote_workspace_application(key.as_str(), remote)?;
                let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
                if let Some(WorkspaceEntry::Connected(application)) = guard.workspaces.get(&key) {
                    let snapshot = application.lightweight_ui_snapshot();
                    guard.active_workspace = Some(key);
                    app_core::startup_perf::mark("state/set_active_workspace/end", "");
                    return Ok(snapshot);
                }
                guard
                    .workspaces
                    .insert(key.clone(), WorkspaceEntry::Connected(application));
                guard.active_workspace = Some(key);
                app_core::startup_perf::mark("state/set_active_workspace/end", "");
                return Ok(snapshot);
            }

            let local_path = local_path.ok_or("Workspace is not open")?;
            let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
            let snapshot = connect_workspace_locked(&mut guard, key.clone(), local_path, None)?;
            guard.active_workspace = Some(key);
            app_core::startup_perf::mark("state/set_active_workspace/end", "");
            return Ok(snapshot);
        }

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

    pub fn active_remote_linux_workspace(&self) -> Result<Option<RemoteLinuxWorkspace>, String> {
        let guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        let Some(active_key) = guard.active_workspace.as_deref() else {
            return Ok(None);
        };
        Ok(guard.workspaces.get(active_key).and_then(entry_remote))
    }

    pub fn poll_active_and_get_update(
        &self,
        cursor: &mut UiPatchCursor,
    ) -> Result<Option<UiSnapshotUpdate>, String> {
        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        let active_key = guard.active_workspace.clone().ok_or("No workspace open")?;
        poll_connected_workspaces(&mut guard);
        let app = match guard.workspaces.get_mut(&active_key) {
            Some(WorkspaceEntry::Connected(app)) => app,
            _ => return Err("No connected workspace open".into()),
        };
        Ok(app.lightweight_ui_update(cursor))
    }

    /// Subscribe to update signals from the active workspace's `Application`.
    /// Returns `Ok(None)` when no connected workspace is active. The returned
    /// receiver is detached from the registry mutex and continues to work
    /// after the lock is released. Callers should re-subscribe when the
    /// active workspace changes (see `active_workspace_key`).
    pub fn subscribe_active_updates(
        &self,
    ) -> Result<Option<tokio::sync::broadcast::Receiver<AppUpdate>>, String> {
        let guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        let active_key = guard.active_workspace.clone().ok_or("No workspace open")?;
        let app = match guard.workspaces.get(&active_key) {
            Some(WorkspaceEntry::Connected(app)) => app,
            _ => return Ok(None),
        };
        Ok(Some(app.subscribe_updates()))
    }

    /// The registry key of the active workspace, or `None` when no workspace
    /// is active. Used by the snapshot bridge to detect workspace switches
    /// and re-subscribe to the new `Application`'s update signals.
    pub fn active_workspace_key(&self) -> Result<Option<String>, String> {
        let guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        Ok(guard.active_workspace.clone())
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

    pub fn list_workspace_dir(&self, path: String) -> Result<Vec<FileEntry>, String> {
        let remote_config = {
            let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
            let active_key = guard.active_workspace.clone().ok_or("No workspace open")?;
            ensure_workspace_connected_locked(&mut guard, &active_key)?;
            let app = match guard.workspaces.get_mut(&active_key) {
                Some(WorkspaceEntry::Connected(app)) => app,
                _ => return Err("No connected workspace open".into()),
            };
            if app.is_remote_workspace() {
                Some(app.remote_ssh_session_config().ok_or(
                    "Remote workspace is missing SSH session config for remote filesystem list",
                )?)
            } else {
                return app.list_workspace_dir(&path);
            }
        };

        match remote_config {
            Some(config) => app_core::list_remote_workspace_dir(&config, &path),
            None => unreachable!("local workspace returns while holding the application lock"),
        }
    }

    pub fn open_workspace_file(&self, path: String) -> Result<EditorFileSnapshot, String> {
        let remote_config = {
            let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
            let active_key = guard.active_workspace.clone().ok_or("No workspace open")?;
            ensure_workspace_connected_locked(&mut guard, &active_key)?;
            let app = match guard.workspaces.get_mut(&active_key) {
                Some(WorkspaceEntry::Connected(app)) => app,
                _ => return Err("No connected workspace open".into()),
            };
            if app.is_remote_workspace() {
                Some(app.remote_ssh_session_config().ok_or(
                    "Remote workspace is missing SSH session config for remote editor file access",
                )?)
            } else {
                return app.editor_open_file(&path);
            }
        };

        match remote_config {
            Some(config) => app_core::read_remote_workspace_file(&config, &path),
            None => unreachable!("local workspace returns while holding the application lock"),
        }
    }

    pub fn git_refresh(&self) -> Result<RepositorySnapshot, String> {
        let remote_request = {
            let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
            let active_key = guard.active_workspace.clone().ok_or("No workspace open")?;
            let app = match guard.workspaces.get_mut(&active_key) {
                Some(WorkspaceEntry::Connected(app)) => app,
                _ => return Err("No connected workspace open".into()),
            };
            if app.is_remote_workspace() {
                Some((
                    active_key,
                    app.remote_ssh_session_config()
                        .ok_or("Remote workspace is missing SSH session config for git refresh")?,
                ))
            } else {
                app.refresh_repository();
                return Ok(app.ui.repository.clone());
            }
        };

        let Some((workspace_key, config)) = remote_request else {
            unreachable!("local workspace returns while holding the application lock")
        };
        let snapshot = app_core::refresh_remote_git_status(&config)?;

        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        if guard.active_workspace.as_deref() == Some(workspace_key.as_str())
            && let Some(WorkspaceEntry::Connected(app)) = guard.workspaces.get_mut(&workspace_key)
            && app.is_remote_workspace()
        {
            app.replace_repository_snapshot(snapshot.clone());
        }
        Ok(snapshot)
    }

    pub fn with_workspace_app<F, R>(&self, path: Option<String>, f: F) -> Result<R, String>
    where
        F: FnOnce(&mut Application) -> Result<R, String>,
    {
        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        let key = match path {
            Some(path) => workspace_key_for_identifier(&guard, &path)
                .unwrap_or_else(|| normalize_tracked_path(&path)),
            None => guard.active_workspace.clone().ok_or("No workspace open")?,
        };
        let (remote, path) = match guard.workspaces.get(&key) {
            Some(entry) => (entry_remote(entry), entry_path(entry)),
            None => {
                // Auto-open the project-less "聊天" workspace so a new chat
                // created from the sidebar hero does not require a real
                // project directory to be opened first.
                if is_chats_workspace(&key) {
                    let root = chats_workspace_root()?;
                    std::fs::create_dir_all(&root)
                        .map_err(|e| format!("创建聊天工作区目录失败: {e}"))?;
                    connect_workspace_locked(&mut guard, key.clone(), root, None)?;
                    (None, None)
                } else {
                    return Err("Workspace is not open".into());
                }
            }
        };
        if let Some(remote) = remote {
            connect_remote_workspace_locked(&mut guard, key.clone(), remote)?;
        } else if let Some(path) = path {
            if !matches!(guard.workspaces.get(&key), Some(WorkspaceEntry::Connected(_))) {
                connect_workspace_locked(&mut guard, key.clone(), path, None)?;
            }
        }
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
            Some(path) => workspace_key_for_identifier(&guard, &path)
                .unwrap_or_else(|| normalize_tracked_path(&path)),
            None => guard.active_workspace.clone().ok_or("No workspace open")?,
        };
        let is_active_workspace = guard.active_workspace.as_deref() == Some(key.as_str());

        if is_active_workspace {
            let (remote, path) = {
                let entry = guard.workspaces.get(&key).ok_or("Workspace is not open")?;
                (entry_remote(entry), entry_path(entry))
            };
            if let Some(remote) = remote {
                connect_remote_workspace_locked(&mut guard, key.clone(), remote)?;
            } else {
                let path = path.ok_or("Workspace is not open")?;
                connect_workspace_locked(&mut guard, key.clone(), path, None)?;
            }
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

    pub fn archive_session(&self, workspace_root: Option<String>, id: &str) -> Result<(), String> {
        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        let key = match workspace_root {
            Some(path) => workspace_key_for_identifier(&guard, &path)
                .unwrap_or_else(|| normalize_tracked_path(&path)),
            None => guard.active_workspace.clone().ok_or("No workspace open")?,
        };
        let is_active_workspace = guard.active_workspace.as_deref() == Some(key.as_str());

        if is_active_workspace {
            let (remote, path) = {
                let entry = guard.workspaces.get(&key).ok_or("Workspace is not open")?;
                (entry_remote(entry), entry_path(entry))
            };
            if let Some(remote) = remote {
                connect_remote_workspace_locked(&mut guard, key.clone(), remote)?;
            } else {
                let path = path.ok_or("Workspace is not open")?;
                connect_workspace_locked(&mut guard, key.clone(), path, None)?;
            }
            let app = match guard.workspaces.get_mut(&key) {
                Some(WorkspaceEntry::Connected(app)) => app,
                _ => return Err("Workspace is not connected".into()),
            };
            return app.session_archive(id);
        }

        let entry = guard
            .workspaces
            .get_mut(&key)
            .ok_or("Workspace is not open")?;
        match entry {
            WorkspaceEntry::Connected(app) => app.session_archive(id),
            WorkspaceEntry::Dormant(metadata) => {
                let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
                let store = SessionStore::open(paths.root(), &metadata.workspace.root)
                    .map_err(|e| e.to_string())?;
                store.archive_session(id).map_err(|e| e.to_string())?;
                metadata.sessions = load_lightweight_sessions(&metadata.workspace.root)?;
                Ok(())
            }
        }
    }

    pub fn unarchive_session(
        &self,
        workspace_root: Option<String>,
        id: &str,
    ) -> Result<(), String> {
        let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
        let store = SessionStore::open_global(paths.root()).map_err(|e| e.to_string())?;
        store.unarchive_session(id).map_err(|e| e.to_string())?;

        let Some(workspace_root) = workspace_root else {
            return Ok(());
        };

        let mut guard = self.workspaces.lock().map_err(|e| e.to_string())?;
        if let Some(key) = workspace_key_for_identifier(&guard, &workspace_root)
            && let Some(WorkspaceEntry::Dormant(metadata)) = guard.workspaces.get_mut(&key)
        {
            metadata.sessions = load_lightweight_sessions(&metadata.workspace.root)?;
        }
        Ok(())
    }

    pub fn delete_archived_session(&self, id: &str) -> Result<(), String> {
        let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
        let store = SessionStore::open_global(paths.root()).map_err(|e| e.to_string())?;
        store.delete_archived_session(id).map_err(|e| e.to_string())
    }

    pub fn delete_all_archived_sessions(&self) -> Result<(), String> {
        let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
        let store = SessionStore::open_global(paths.root()).map_err(|e| e.to_string())?;
        store
            .delete_all_archived_sessions()
            .map_err(|e| e.to_string())
    }
}

impl Drop for AppState {
    fn drop(&mut self) {
        self.shutdown_all();
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

fn connect_remote_workspace_locked(
    guard: &mut WorkspaceRegistry,
    key: String,
    remote: RemoteLinuxWorkspace,
) -> Result<UiSnapshot, String> {
    if let Some(WorkspaceEntry::Connected(application)) = guard.workspaces.get(&key) {
        return Ok(application.lightweight_ui_snapshot());
    }

    let (application, snapshot) = build_remote_workspace_application(key.as_str(), remote)?;
    guard
        .workspaces
        .insert(key, WorkspaceEntry::Connected(application));
    Ok(snapshot)
}

fn activate_workspace_locked(
    guard: &mut WorkspaceRegistry,
    key: String,
) -> Result<UiSnapshot, String> {
    let (remote, path) = {
        let entry = guard.workspaces.get(&key).ok_or("Workspace is not open")?;
        if let WorkspaceEntry::Connected(application) = entry {
            let snapshot = application.lightweight_ui_snapshot();
            guard.active_workspace = Some(key);
            return Ok(snapshot);
        }
        (entry_remote(entry), entry_path(entry))
    };

    guard.active_workspace = Some(key.clone());
    if let Some(remote) = remote {
        return app_core::build_dormant_remote_workspace_ui(remote);
    }

    let path = path.ok_or("Workspace is not open")?;
    connect_workspace_locked(guard, key, path, None)
}

fn build_remote_workspace_application(
    key: &str,
    remote: RemoteLinuxWorkspace,
) -> Result<(Application, UiSnapshot), String> {
    app_core::startup_perf::mark("state/connect_remote_workspace/start", key);
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    let mut remote = remote;
    let agent_command = remote
        .agent_command
        .clone()
        .or_else(|| {
            let agent = remote
                .agent_cli
                .unwrap_or_else(|| app_core::settings::default_agent_for_new_work(&paths));
            app_core::settings::remote_linux_command_for_agent(agent)
        })
        .unwrap_or_else(acp_core::platform_default_agent_command);
    let agent_command = ensure_remote_agent_bootstrapped(&mut remote, agent_command, &paths)?;

    let application =
        Application::bootstrap_remote_linux_with_app_paths(remote, agent_command, paths)
            .map_err(|e| e.to_string())?;
    let snapshot = application.lightweight_ui_snapshot();
    app_core::startup_perf::mark("state/connect_remote_workspace/end", "");
    Ok((application, snapshot))
}

fn ensure_remote_agent_bootstrapped(
    remote: &mut RemoteLinuxWorkspace,
    agent_command: String,
    paths: &app_core::AppPaths,
) -> Result<String, String> {
    let Some(agent) = remote_agent_command_needs_bootstrap(remote, &agent_command) else {
        return Ok(agent_command);
    };
    let profile = remote
        .profile_id
        .and_then(|profile_id| {
            app_core::remote_profiles::get_remote_machine_profile(paths, profile_id).ok()
        })
        .unwrap_or_else(|| remote_machine_profile_from_workspace(remote));
    let bootstrap = app_core::remote_bootstrap::bootstrap_remote_agent(
        app_core::remote_bootstrap::RemoteAgentBootstrapRequest {
            request_id: uuid::Uuid::new_v4(),
            profile: &profile,
            remote_path: &remote.remote_path,
            ssh_password: remote.ssh_password.as_deref(),
            agent_cli: agent,
        },
        &app_core::remote_ssh::SystemRemoteSshCommandRunner,
        |_| {},
    )
    .map_err(|error| error.to_string())?;
    remote.agent_cli = Some(agent);
    remote.agent_command = Some(bootstrap.agent_command.clone());
    Ok(bootstrap.agent_command)
}

fn remote_agent_command_needs_bootstrap(
    remote: &RemoteLinuxWorkspace,
    agent_command: &str,
) -> Option<AgentCliId> {
    let agent = remote
        .agent_cli
        .or_else(|| remote_agent_id_from_standard_command(agent_command))?;
    is_standard_remote_agent_command(agent, agent_command).then_some(agent)
}

fn remote_agent_id_from_standard_command(agent_command: &str) -> Option<AgentCliId> {
    let command = agent_command.trim().to_ascii_lowercase();
    if app_core::settings::is_codex_acp_command(&command) {
        return Some(AgentCliId::CodexAcp);
    }
    if app_core::settings::is_claude_agent_acp_command(&command) {
        return Some(AgentCliId::ClaudeAgentAcp);
    }
    if command == "codebuddy --acp" || command == "codebuddy --acp --acp-transport streamable-http"
    {
        return Some(AgentCliId::Codebuddy);
    }
    None
}

fn is_standard_remote_agent_command(agent: AgentCliId, agent_command: &str) -> bool {
    let command = agent_command.trim();
    if app_core::settings::remote_linux_command_for_agent(agent).as_deref() == Some(command) {
        return true;
    }
    matches!(agent, AgentCliId::Codebuddy)
        && command == "codebuddy --acp --acp-transport streamable-http"
}

fn remote_machine_profile_from_workspace(
    remote: &RemoteLinuxWorkspace,
) -> workspace_model::RemoteMachineProfile {
    workspace_model::RemoteMachineProfile {
        id: remote.profile_id.unwrap_or_else(uuid::Uuid::new_v4),
        display_name: remote.display_name(),
        ssh_target: remote.ssh_target.clone(),
        ssh_port: remote.ssh_port,
        created_at_ms: 0,
        updated_at_ms: 0,
        last_validation: None,
    }
}

fn ensure_workspace_connected_locked(
    guard: &mut WorkspaceRegistry,
    key: &str,
) -> Result<(), String> {
    if matches!(
        guard.workspaces.get(key),
        Some(WorkspaceEntry::Connected(_))
    ) {
        return Ok(());
    }

    let (remote, path) = {
        let entry = guard.workspaces.get(key).ok_or("Workspace is not open")?;
        (entry_remote(entry), entry_path(entry))
    };
    if let Some(remote) = remote {
        connect_remote_workspace_locked(guard, key.to_string(), remote)?;
    } else {
        let path = path.ok_or("Workspace is not open")?;
        connect_workspace_locked(guard, key.to_string(), path, None)?;
    }
    Ok(())
}

fn workspace_session_list(
    key: &str,
    entry: &mut WorkspaceEntry,
    is_active: bool,
) -> Result<WorkspaceSessionList, String> {
    match entry {
        WorkspaceEntry::Connected(app) => app
            .session_list_after_poll_for_visibility(is_active)
            .map(|sessions| WorkspaceSessionList {
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

fn open_workspace_record_path(item: &OpenWorkspaceItem) -> String {
    match &item.workspace.location {
        WorkspaceLocation::RemoteLinux(remote) => remote.key(),
        WorkspaceLocation::Local => item.workspace.root.display().to_string(),
    }
}

fn remote_workspace_for_storage(mut remote: RemoteLinuxWorkspace) -> RemoteLinuxWorkspace {
    remote.local_port = None;
    remote.remote_port = None;
    remote.ssh_password = None;
    remote
}

fn load_lightweight_sessions(path: &Path) -> Result<Vec<SessionListItem>, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    SessionStore::open(paths.root(), path)
        .and_then(|store| store.list_session_summaries())
        .map_err(|e| e.to_string())
}

fn archive_workspace_sessions(path: &Path) -> Result<(), String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    SessionStore::open(paths.root(), path)
        .and_then(|store| store.archive_workspace_sessions())
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
    sort_workspaces(&mut items, |item| item.workspace.name.as_str());
    items
}

fn poll_connected_workspaces(guard: &mut WorkspaceRegistry) {
    for entry in guard.workspaces.values_mut() {
        if let WorkspaceEntry::Connected(app) = entry {
            app.poll_prompt_progress();
        }
    }
}

fn active_session_id(sessions: &[SessionListItem]) -> uuid::Uuid {
    sessions
        .first()
        .and_then(|session| uuid::Uuid::parse_str(&session.id).ok())
        .unwrap_or_else(uuid::Uuid::nil)
}

fn entry_path(entry: &WorkspaceEntry) -> Option<PathBuf> {
    if entry_is_remote(entry) {
        return None;
    }
    Some(match entry {
        WorkspaceEntry::Connected(app) => app.ui.workspace.root.clone(),
        WorkspaceEntry::Dormant(metadata) => metadata.workspace.root.clone(),
    })
}

fn entry_workspace_root(entry: &WorkspaceEntry) -> PathBuf {
    match entry {
        WorkspaceEntry::Connected(app) => app.ui.workspace.root.clone(),
        WorkspaceEntry::Dormant(metadata) => metadata.workspace.root.clone(),
    }
}

fn entry_is_remote(entry: &WorkspaceEntry) -> bool {
    entry_remote(entry).is_some()
}

fn entry_remote(entry: &WorkspaceEntry) -> Option<RemoteLinuxWorkspace> {
    let location = match entry {
        WorkspaceEntry::Connected(app) => &app.ui.workspace.location,
        WorkspaceEntry::Dormant(metadata) => &metadata.workspace.location,
    };
    match location {
        WorkspaceLocation::RemoteLinux(remote) => Some(remote.clone()),
        WorkspaceLocation::Local => None,
    }
}

fn terminal_workspace_from_entry(
    entry: &WorkspaceEntry,
) -> Result<ResolvedTerminalWorkspace, String> {
    if let Some(remote) = entry_remote(entry) {
        return Ok(ResolvedTerminalWorkspace::Remote(remote));
    }
    entry_path(entry)
        .map(ResolvedTerminalWorkspace::Local)
        .ok_or_else(|| "Workspace is not open".into())
}

fn workspace_key_for_identifier(guard: &WorkspaceRegistry, identifier: &str) -> Option<String> {
    let normalized = normalize_tracked_path(identifier);
    if guard.workspaces.contains_key(&normalized) {
        return Some(normalized);
    }

    guard.workspaces.iter().find_map(|(key, entry)| {
        let entry_root =
            entry_path(entry).map(|path| normalize_tracked_path(&path.display().to_string()));
        if entry_root.as_deref() == Some(normalized.as_str()) {
            return Some(key.clone());
        }

        let remote = entry_remote(entry)?;
        if normalize_tracked_path(&remote.remote_path) == normalized
            || normalize_tracked_path(&remote.key()) == normalized
        {
            return Some(key.clone());
        }
        None
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
        location: WorkspaceLocation::Local,
    }
}

fn remote_workspace_descriptor(remote: RemoteLinuxWorkspace) -> WorkspaceDescriptor {
    WorkspaceDescriptor {
        id: uuid::Uuid::new_v4(),
        name: remote.display_name(),
        root: PathBuf::from(remote.key()),
        location: WorkspaceLocation::RemoteLinux(remote),
    }
}

fn sort_workspaces<T, F>(items: &mut [T], name: F)
where
    F: Fn(&T) -> &str,
{
    // Sort by name only and keep the order stable. Previously the active
    // workspace was forced to the front, which reordered the list every time
    // the user switched sessions — visually jarring and unpredictable.
    items.sort_by(|a, b| name(a).cmp(name(b)));
}

fn workspace_key(path: &PathBuf) -> String {
    normalize_tracked_path(&path.display().to_string())
}

fn remote_workspace_key(remote: &RemoteLinuxWorkspace) -> String {
    normalize_tracked_path(&remote.key())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::ffi::OsString;
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::sync::{Mutex, OnceLock};

    #[test]
    fn dormant_workspace_session_list_exposes_names_without_connection() {
        let workspace = WorkspaceDescriptor {
            id: uuid::Uuid::new_v4(),
            name: "Dormant".into(),
            root: PathBuf::from("D:/work/Dormant"),
            location: WorkspaceLocation::Local,
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
            runtime_status: Default::default(),
            attention_state: Default::default(),
        };
        let mut entry = WorkspaceEntry::Dormant(WorkspaceMetadata {
            workspace,
            sessions: vec![session.clone()],
        });

        let list = workspace_session_list("dormant", &mut entry, false).unwrap();

        assert!(!list.connected);
        assert_eq!(list.sessions.len(), 1);
        assert_eq!(list.sessions[0].title, "Stored session");
        assert_eq!(list.sessions[0].message_count, 0);
    }

    #[test]
    fn dormant_remote_workspace_session_list_preserves_remote_location() {
        let remote = RemoteLinuxWorkspace {
            profile_id: None,
            ssh_target: "devbox".into(),
            ssh_port: None,
            remote_path: "/srv/kodex".into(),
            ssh_password: None,
            agent_cli: Some(AgentCliId::CodexAcp),
            agent_command: Some("codex-acp".into()),
            local_port: None,
            remote_port: None,
        };
        let workspace = remote_workspace_descriptor(remote.clone());
        let key = remote_workspace_key(&remote);
        let mut entry = WorkspaceEntry::Dormant(WorkspaceMetadata {
            workspace,
            sessions: Vec::new(),
        });

        let list = workspace_session_list(&key, &mut entry, true).unwrap();

        assert!(!list.connected);
        assert!(list.is_active);
        assert_eq!(list.workspace.root, PathBuf::from(remote.key()));
        assert_eq!(entry_remote(&entry), Some(remote));
    }

    #[test]
    fn remote_workspace_entry_path_is_not_treated_as_local_path() {
        let remote = RemoteLinuxWorkspace {
            profile_id: None,
            ssh_target: "devbox".into(),
            ssh_port: Some(22),
            remote_path: "/srv/kodex".into(),
            ssh_password: None,
            agent_cli: Some(AgentCliId::CodexAcp),
            agent_command: Some("codex-acp".into()),
            local_port: None,
            remote_port: None,
        };
        let entry = WorkspaceEntry::Dormant(WorkspaceMetadata {
            workspace: remote_workspace_descriptor(remote.clone()),
            sessions: Vec::new(),
        });

        assert_eq!(entry_remote(&entry), Some(remote));
        assert!(entry_path(&entry).is_none());
    }

    #[test]
    fn stale_remote_standard_agent_command_requires_bootstrap() {
        let remote = RemoteLinuxWorkspace {
            profile_id: None,
            ssh_target: "devbox".into(),
            ssh_port: Some(22),
            remote_path: "/srv/kodex".into(),
            ssh_password: None,
            agent_cli: Some(AgentCliId::CodexAcp),
            agent_command: Some("codex-acp".into()),
            local_port: None,
            remote_port: None,
        };

        assert_eq!(
            remote_agent_command_needs_bootstrap(&remote, "codex-acp"),
            Some(AgentCliId::CodexAcp)
        );
    }

    #[test]
    fn verified_remote_agent_command_does_not_bootstrap_again() {
        let remote = RemoteLinuxWorkspace {
            profile_id: None,
            ssh_target: "devbox".into(),
            ssh_port: Some(22),
            remote_path: "/srv/kodex".into(),
            ssh_password: None,
            agent_cli: Some(AgentCliId::CodexAcp),
            agent_command: Some(
                "/root/.kodex/remote-agents/codex-acp/current/bin/codex-acp".into(),
            ),
            local_port: None,
            remote_port: None,
        };

        assert_eq!(
            remote_agent_command_needs_bootstrap(
                &remote,
                "/root/.kodex/remote-agents/codex-acp/current/bin/codex-acp"
            ),
            None
        );
    }

    #[test]
    fn stale_codebuddy_streamable_command_requires_bootstrap() {
        let remote = RemoteLinuxWorkspace {
            profile_id: None,
            ssh_target: "devbox".into(),
            ssh_port: Some(22),
            remote_path: "/srv/kodex".into(),
            ssh_password: None,
            agent_cli: None,
            agent_command: Some("codebuddy --acp --acp-transport streamable-http".into()),
            local_port: None,
            remote_port: None,
        };

        assert_eq!(
            remote_agent_command_needs_bootstrap(
                &remote,
                "codebuddy --acp --acp-transport streamable-http"
            ),
            Some(AgentCliId::Codebuddy)
        );
    }

    #[test]
    fn open_workspace_state_persists_remote_key_without_runtime_ports() {
        let state = AppState::new();
        let remote = RemoteLinuxWorkspace {
            profile_id: None,
            ssh_target: "devbox".into(),
            ssh_port: Some(22),
            remote_path: "/srv/kodex".into(),
            ssh_password: Some("secret".into()),
            agent_cli: Some(AgentCliId::CodexAcp),
            agent_command: Some("codex-acp".into()),
            local_port: Some(3456),
            remote_port: Some(4567),
        };

        state
            .restore_active_dormant_remote_workspace(remote.clone())
            .unwrap();

        let open = state.open_workspace_state().unwrap();
        assert_eq!(open.active_path.as_deref(), Some(remote.key().as_str()));
        assert_eq!(open.workspaces.len(), 1);
        assert_eq!(open.workspaces[0].path, remote.key());
        let stored_remote = open.workspaces[0].remote.as_ref().unwrap();
        assert_eq!(stored_remote.remote_path, "/srv/kodex");
        assert_eq!(stored_remote.local_port, None);
        assert_eq!(stored_remote.remote_port, None);
        assert_eq!(stored_remote.ssh_password, None);
    }

    #[test]
    fn workspace_identifier_resolves_remote_path_to_remote_workspace_key() {
        let remote = RemoteLinuxWorkspace {
            profile_id: None,
            ssh_target: "devbox".into(),
            ssh_port: Some(22),
            remote_path: "/srv/kodex".into(),
            ssh_password: None,
            agent_cli: Some(AgentCliId::CodexAcp),
            agent_command: Some("codex-acp".into()),
            local_port: Some(3456),
            remote_port: Some(4567),
        };
        let key = remote_workspace_key(&remote);
        let mut guard = WorkspaceRegistry::default();
        guard.workspaces.insert(
            key.clone(),
            WorkspaceEntry::Dormant(WorkspaceMetadata {
                workspace: remote_workspace_descriptor(remote.clone()),
                sessions: Vec::new(),
            }),
        );

        assert_eq!(
            workspace_key_for_identifier(&guard, "/srv/kodex").as_deref(),
            Some(key.as_str())
        );
        assert_eq!(
            workspace_key_for_identifier(&guard, &remote.key()).as_deref(),
            Some(key.as_str())
        );
    }

    #[test]
    fn restore_dormant_remote_workspace_registers_without_connecting() {
        let state = AppState::new();
        let remote = RemoteLinuxWorkspace {
            profile_id: None,
            ssh_target: "devbox".into(),
            ssh_port: None,
            remote_path: "/srv/kodex".into(),
            ssh_password: None,
            agent_cli: Some(AgentCliId::CodexAcp),
            agent_command: Some("codex-acp".into()),
            local_port: Some(3456),
            remote_port: Some(4567),
        };

        state
            .restore_dormant_remote_workspace(remote.clone())
            .unwrap();

        let workspaces = state.list_workspace_sessions().unwrap();
        assert_eq!(workspaces.len(), 1);
        assert!(!workspaces[0].connected);
        assert_eq!(workspaces[0].workspace.root, PathBuf::from(remote.key()));
        assert!(matches!(
            workspaces[0].workspace.location,
            WorkspaceLocation::RemoteLinux(_)
        ));

        let open = state.list_open_workspaces().unwrap();
        assert_eq!(open.len(), 1);
        assert!(!open[0].connected);
    }

    #[test]
    fn activating_remote_workspace_keeps_local_workspace_entries() {
        let state = AppState::new();
        let local_path = std::env::temp_dir().join(format!(
            "kodex-remote-context-local-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&local_path).unwrap();
        state.restore_dormant_workspace(local_path.clone()).unwrap();
        let remote = RemoteLinuxWorkspace {
            profile_id: None,
            ssh_target: "devbox".into(),
            ssh_port: Some(36000),
            remote_path: "/srv/kodex".into(),
            ssh_password: None,
            agent_cli: Some(AgentCliId::CodexAcp),
            agent_command: Some("codex-acp".into()),
            local_port: Some(3456),
            remote_port: Some(4567),
        };
        state
            .restore_dormant_remote_workspace(remote.clone())
            .unwrap();
        let remote_key = remote_workspace_key(&remote);
        {
            let mut guard = state.workspaces.lock().unwrap();
            guard.active_workspace = Some(remote_key.clone());
        }

        let workspaces = state.list_workspace_sessions().unwrap();
        assert_eq!(workspaces.len(), 2);
        assert!(
            workspaces
                .iter()
                .any(|workspace| workspace.workspace.root == local_path)
        );
        let remote_workspace = workspaces
            .iter()
            .find(|workspace| workspace.workspace.root == PathBuf::from(remote.key()))
            .expect("remote workspace remains listed");
        assert!(remote_workspace.is_active);
        assert!(matches!(
            remote_workspace.workspace.location,
            WorkspaceLocation::RemoteLinux(_)
        ));

        state.shutdown_all();
        let _ = std::fs::remove_dir_all(&local_path);
    }

    #[test]
    fn active_local_workspace_opens_terminal_dock_session() {
        let path = std::env::temp_dir().join(format!(
            "kodex-local-terminal-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).unwrap();
        let key = workspace_key(&path);
        let state = AppState::new();
        {
            let mut guard = state.workspaces.lock().unwrap();
            guard.active_workspace = Some(key.clone());
            guard.workspaces.insert(
                key,
                WorkspaceEntry::Dormant(WorkspaceMetadata {
                    workspace: workspace_descriptor(&path),
                    sessions: Vec::new(),
                }),
            );
        }

        let session = state
            .terminal_open(TerminalOpenRequest {
                workspace_root: None,
                force_new: false,
                cols: 80,
                rows: 24,
            })
            .unwrap();

        assert!(!session.terminal_id.is_empty());
        assert_eq!(session.cols, 80);
        assert_eq!(session.rows, 24);
        let canonical_path = path.canonicalize().unwrap_or_else(|_| path.clone());
        assert_eq!(
            normalize_tracked_path(&session.workspace_root),
            normalize_tracked_path(&canonical_path.display().to_string())
        );

        let resized = state
            .terminal_resize(TerminalResizeRequest {
                terminal_id: session.terminal_id.clone(),
                cols: 100,
                rows: 30,
            })
            .unwrap();
        assert_eq!(resized.cols, 100);
        assert_eq!(resized.rows, 30);

        let listed = state.terminal_list(None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].terminal_id, session.terminal_id);

        state.terminal_terminate(&session.terminal_id).unwrap();
        state.shutdown_all();
        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn active_remote_workspace_lists_terminal_sessions_by_remote_key() {
        let state = AppState::new();
        let remote = remote_terminal_fixture();
        state
            .restore_active_dormant_remote_workspace(remote.clone())
            .unwrap();

        let sessions = state.terminal_list(None).unwrap();
        let explicit_sessions = state.terminal_list(Some(remote.key())).unwrap();

        assert!(sessions.is_empty());
        assert!(explicit_sessions.is_empty());
    }

    #[test]
    fn explicit_terminal_workspace_resolves_remote_key_before_local_fallback() {
        let state = AppState::new();
        let remote = remote_terminal_fixture();
        state
            .restore_dormant_remote_workspace(remote.clone())
            .unwrap();

        match state
            .resolve_terminal_workspace(Some(remote.key()))
            .expect("remote key resolves")
        {
            ResolvedTerminalWorkspace::Remote(resolved) => assert_eq!(resolved, remote),
            ResolvedTerminalWorkspace::Local(path) => {
                panic!(
                    "expected remote workspace, got local path {}",
                    path.display()
                )
            }
        }

        match state
            .resolve_terminal_workspace(Some("__missing_terminal_workspace__".into()))
            .expect("unknown explicit workspace falls back to local compatibility")
        {
            ResolvedTerminalWorkspace::Local(path) => {
                assert_eq!(path, PathBuf::from("__missing_terminal_workspace__"));
            }
            ResolvedTerminalWorkspace::Remote(_) => panic!("unexpected remote workspace"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn remote_terminal_open_and_close_workspace_use_remote_terminal_key() {
        let _lock = ssh_path_lock().lock().unwrap();
        let dir = std::env::temp_dir().join(format!("kodex-fake-ssh-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let fake_ssh = dir.join("ssh");
        fs::write(
            &fake_ssh,
            "#!/bin/sh\ntrap 'exit 0' TERM INT HUP\nwhile true; do sleep 1; done\n",
        )
        .unwrap();
        fs::set_permissions(&fake_ssh, fs::Permissions::from_mode(0o700)).unwrap();
        let _path_guard = prepend_path_for_test(&dir);

        let state = AppState::new();
        let remote = remote_terminal_fixture();
        state
            .restore_active_dormant_remote_workspace(remote.clone())
            .unwrap();

        let session = state
            .terminal_open(TerminalOpenRequest {
                workspace_root: None,
                force_new: false,
                cols: 80,
                rows: 24,
            })
            .unwrap();

        assert_eq!(session.workspace_root, remote.key());
        assert_eq!(session.cwd, remote.remote_path);
        assert_eq!(
            state
                .terminal_service
                .list_workspace_key(&remote.key())
                .unwrap()
                .len(),
            1
        );

        state.close_workspace().unwrap();
        assert!(
            state
                .terminal_service
                .list_workspace_key(&remote.key())
                .unwrap()
                .is_empty()
        );
        state.shutdown_all();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn active_dormant_workspace_lists_files_after_connecting() {
        let path = std::env::temp_dir().join(format!(
            "kodex-dormant-list-files-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("hello.txt"), "hello").unwrap();
        let key = workspace_key(&path);
        let state = AppState::new();
        {
            let mut guard = state.workspaces.lock().unwrap();
            guard.active_workspace = Some(key.clone());
            guard.workspaces.insert(
                key.clone(),
                WorkspaceEntry::Dormant(WorkspaceMetadata {
                    workspace: workspace_descriptor(&path),
                    sessions: Vec::new(),
                }),
            );
        }

        let entries = state.list_workspace_dir(String::new()).unwrap();

        assert!(entries.iter().any(|entry| entry.name == "hello.txt"));
        let guard = state.workspaces.lock().unwrap();
        assert!(matches!(
            guard.workspaces.get(&key),
            Some(WorkspaceEntry::Connected(_))
        ));
        drop(guard);
        state.shutdown_all();
        let _ = std::fs::remove_dir_all(&path);
    }

    fn remote_terminal_fixture() -> RemoteLinuxWorkspace {
        RemoteLinuxWorkspace {
            profile_id: None,
            ssh_target: "devbox".into(),
            ssh_port: Some(2222),
            remote_path: "/srv/kodex".into(),
            ssh_password: None,
            agent_cli: Some(AgentCliId::CodexAcp),
            agent_command: Some("codex-acp".into()),
            local_port: Some(3456),
            remote_port: Some(4567),
        }
    }

    #[cfg(unix)]
    fn ssh_path_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[cfg(unix)]
    struct PathOverride {
        previous: Option<OsString>,
    }

    #[cfg(unix)]
    impl Drop for PathOverride {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(previous) => unsafe {
                    std::env::set_var("PATH", previous);
                },
                None => unsafe {
                    std::env::remove_var("PATH");
                },
            }
        }
    }

    #[cfg(unix)]
    fn prepend_path_for_test(path: &Path) -> PathOverride {
        let previous = std::env::var_os("PATH");
        let mut paths = vec![path.to_path_buf()];
        if let Some(previous) = previous.as_ref() {
            paths.extend(std::env::split_paths(previous));
        }
        let next = std::env::join_paths(paths).unwrap();
        unsafe {
            std::env::set_var("PATH", next);
        }
        PathOverride { previous }
    }
}
