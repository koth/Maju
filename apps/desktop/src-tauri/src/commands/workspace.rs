use crate::events;
use crate::open_workspaces::OpenWorkspaces;
use crate::recent_workspaces::{RecentEntry, RecentWorkspaces};
use crate::state::AppState;
use std::path::PathBuf;
use tauri::{AppHandle, State};
use workspace_model::{
    AgentCliId, OpenWorkspaceItem, RemoteLinuxWorkspace, RemoteOpenPhaseKind,
    RemoteOpenPhaseStatus, RemoteOpenProgressEvent, RemoteOpenRequest, UiSnapshot,
};

fn recent_store() -> Result<RecentWorkspaces, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    Ok(RecentWorkspaces::new(paths.workspaces_dir()))
}

fn open_store() -> Result<OpenWorkspaces, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    Ok(OpenWorkspaces::new(paths.workspaces_dir()))
}

pub fn save_open_workspace_state(state: &AppState) -> Result<(), String> {
    let open_state =
        app_core::startup_perf::measure("workspace/save_open_state/build", "", || {
            state.open_workspace_state()
        })?;
    app_core::startup_perf::measure("workspace/save_open_state/write", "", || {
        open_store()?.save(&open_state);
        Ok::<(), String>(())
    })?;
    Ok(())
}

#[tauri::command]
pub fn workspace_open(
    state: State<'_, AppState>,
    path: String,
    agent: Option<AgentCliId>,
) -> Result<UiSnapshot, String> {
    let dir = PathBuf::from(&path);
    if !dir.is_dir() {
        return Err(format!("Not a directory: {path}"));
    }
    let snapshot = state.open_workspace(dir, agent)?;
    recent_store()?.add(&path);
    save_open_workspace_state(&state)?;
    Ok(snapshot)
}

#[tauri::command]
pub fn workspace_open_remote_linux(
    state: State<'_, AppState>,
    remote: RemoteLinuxWorkspace,
) -> Result<UiSnapshot, String> {
    let snapshot = state.open_remote_linux_workspace(remote.clone())?;
    recent_store()?.add_remote(remote);
    save_open_workspace_state(&state)?;
    Ok(snapshot)
}

#[tauri::command]
pub fn workspace_open_remote_profile(
    app: AppHandle,
    state: State<'_, AppState>,
    request: RemoteOpenRequest,
) -> Result<UiSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    open_remote_profile_with_paths(state.inner(), &paths, request, Some(&app))
}

#[tauri::command]
pub fn workspace_close(state: State<'_, AppState>) -> Result<(), String> {
    state.close_workspace()?;
    save_open_workspace_state(&state)
}

#[tauri::command]
pub fn workspace_list_open(state: State<'_, AppState>) -> Result<Vec<OpenWorkspaceItem>, String> {
    state.list_open_workspaces()
}

#[tauri::command]
pub fn workspace_has_open(state: State<'_, AppState>) -> Result<bool, String> {
    state.has_open_workspaces()
}

#[tauri::command]
pub fn workspace_restore_open(state: State<'_, AppState>) -> Result<Option<UiSnapshot>, String> {
    app_core::startup_perf::mark("workspace_restore_open/start", "");
    let saved = app_core::startup_perf::measure("workspace_restore_open/load_saved", "", || {
        open_store().map(|store| store.load())
    })?;
    app_core::startup_perf::mark(
        "workspace_restore_open/saved",
        format!(
            "workspace_count={} active_path={}",
            saved.workspaces.len(),
            saved.active_path.as_deref().unwrap_or("<none>")
        ),
    );
    if saved.workspaces.is_empty() {
        app_core::startup_perf::mark("workspace_restore_open/end", "empty");
        return Ok(None);
    }

    let workspaces = saved.workspaces;
    let preferred_active = saved
        .active_path
        .clone()
        .or_else(|| workspaces.first().map(|workspace| workspace.path.clone()));
    let mut snapshot = None;
    let mut opened_active_path: Option<String> = None;

    if let Some(active_path) = preferred_active {
        let active_record = workspaces
            .iter()
            .find(|workspace| open_workspace_record_matches(workspace, &active_path))
            .cloned();
        if let Some(remote) = active_record
            .as_ref()
            .and_then(|record| record.remote.clone())
        {
            snapshot = Some(state.restore_active_dormant_remote_workspace(remote)?);
            opened_active_path = Some(active_path);
        } else {
            let dir = PathBuf::from(&active_path);
            let exists = app_core::startup_perf::measure(
                "workspace_restore_open/active_is_dir",
                &active_path,
                || dir.is_dir(),
            );
            if exists {
                snapshot = Some(app_core::startup_perf::measure(
                    "workspace_restore_open/open_active",
                    &active_path,
                    || state.open_workspace(dir, None),
                )?);
                opened_active_path = Some(active_path);
            } else {
                app_core::startup_perf::mark("workspace_restore_open/active_missing", active_path);
            }
        }
    }

    if snapshot.is_none() {
        for workspace in &workspaces {
            if let Some(remote) = workspace.remote.clone() {
                snapshot = Some(state.restore_active_dormant_remote_workspace(remote)?);
                opened_active_path = Some(workspace.path.clone());
                break;
            }
            let dir = PathBuf::from(&workspace.path);
            let exists = app_core::startup_perf::measure(
                "workspace_restore_open/fallback_is_dir",
                &workspace.path,
                || dir.is_dir(),
            );
            if exists {
                snapshot = Some(app_core::startup_perf::measure(
                    "workspace_restore_open/open_fallback",
                    &workspace.path,
                    || state.open_workspace(dir, None),
                )?);
                opened_active_path = Some(workspace.path.clone());
                break;
            }
        }
    }

    for workspace in workspaces {
        if Some(workspace.path.as_str()) != opened_active_path.as_deref() {
            if let Some(remote) = workspace.remote {
                state.restore_dormant_remote_workspace(remote)?;
            } else {
                let path = workspace.path;
                let dormant_path = path.clone();
                app_core::startup_perf::measure(
                    "workspace_restore_open/register_dormant",
                    &path,
                    || state.restore_dormant_workspace(PathBuf::from(dormant_path)),
                )?;
            }
        }
    }

    app_core::startup_perf::measure("workspace_restore_open/save_state", "", || {
        save_open_workspace_state(&state)
    })?;
    app_core::startup_perf::mark(
        "workspace_restore_open/end",
        format!(
            "opened={}",
            opened_active_path.as_deref().unwrap_or("<none>")
        ),
    );
    Ok(snapshot)
}

fn open_workspace_record_matches(
    record: &crate::open_workspaces::OpenWorkspaceRecord,
    path: &str,
) -> bool {
    if record.path == path {
        return true;
    }
    record.remote.as_ref().is_some_and(|remote| {
        remote.key() == path
            || app_core::normalize_tracked_path(&remote.remote_path)
                == app_core::normalize_tracked_path(path)
    })
}

#[tauri::command]
pub fn workspace_set_active(
    state: State<'_, AppState>,
    path: String,
) -> Result<UiSnapshot, String> {
    let snapshot = state.set_active_workspace(path)?;
    save_open_workspace_state(&state)?;
    Ok(snapshot)
}

#[tauri::command]
pub fn workspace_get_recent() -> Vec<RecentEntry> {
    app_core::startup_perf::measure("workspace_get_recent", "", || {
        recent_store().map(|store| store.load()).unwrap_or_default()
    })
}

#[tauri::command]
pub fn workspace_remove_recent(path: String) {
    if let Ok(store) = recent_store() {
        store.remove(&path);
    }
}

fn open_remote_profile_with_paths(
    state: &AppState,
    paths: &app_core::AppPaths,
    request: RemoteOpenRequest,
    app: Option<&AppHandle>,
) -> Result<UiSnapshot, String> {
    open_remote_profile_with_runner(
        state,
        paths,
        request,
        app,
        &app_core::remote_ssh::SystemRemoteSshCommandRunner,
    )
}

fn open_remote_profile_with_runner<R>(
    state: &AppState,
    paths: &app_core::AppPaths,
    request: RemoteOpenRequest,
    app: Option<&AppHandle>,
    runner: &R,
) -> Result<UiSnapshot, String>
where
    R: app_core::remote_ssh::RemoteSshCommandRunner,
{
    open_remote_profile_with_progress(state, paths, request, runner, |progress| {
        emit_remote_open_progress(app, &progress)
    })
}

fn open_remote_profile_with_progress<R, F>(
    state: &AppState,
    paths: &app_core::AppPaths,
    mut request: RemoteOpenRequest,
    runner: &R,
    mut progress: F,
) -> Result<UiSnapshot, String>
where
    R: app_core::remote_ssh::RemoteSshCommandRunner,
    F: FnMut(RemoteOpenProgressEvent),
{
    let request_id = request.request_id.unwrap_or_else(uuid::Uuid::new_v4);
    request.request_id = Some(request_id);
    let profile = app_core::remote_profiles::get_remote_machine_profile(paths, request.profile_id)
        .map_err(|e| e.to_string())?;
    let bootstrap = app_core::remote_bootstrap::bootstrap_remote_agent(
        app_core::remote_bootstrap::RemoteAgentBootstrapRequest {
            request_id,
            profile: &profile,
            remote_path: &request.remote_path,
            ssh_password: request.ssh_password.as_deref(),
            agent_cli: request.agent_cli,
        },
        runner,
        |event| progress(event),
    )
    .map_err(|e| e.to_string())?;
    let mut remote = app_core::remote_profiles::remote_workspace_from_profile(paths, request)
        .map_err(|e| e.to_string())?;
    remote.agent_command = Some(bootstrap.agent_command);
    progress(RemoteOpenProgressEvent {
        request_id,
        phase: RemoteOpenPhaseKind::AcpLaunch,
        status: RemoteOpenPhaseStatus::Running,
        elapsed_ms: 0,
        message: Some("正在启动远程 ACP 会话".into()),
    });
    let snapshot = match state.open_remote_linux_workspace(remote.clone()) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            progress(RemoteOpenProgressEvent {
                request_id,
                phase: RemoteOpenPhaseKind::AcpLaunch,
                status: RemoteOpenPhaseStatus::Failed,
                elapsed_ms: 0,
                message: Some(error.clone()),
            });
            return Err(error);
        }
    };
    progress(RemoteOpenProgressEvent {
        request_id,
        phase: RemoteOpenPhaseKind::AcpLaunch,
        status: RemoteOpenPhaseStatus::Succeeded,
        elapsed_ms: 0,
        message: Some("远程 ACP 会话已启动".into()),
    });
    recent_store()?.add_remote(remote);
    save_open_workspace_state(state)?;
    Ok(snapshot)
}

fn emit_remote_open_progress(app: Option<&AppHandle>, progress: &RemoteOpenProgressEvent) {
    if let Some(app) = app {
        events::emit_remote_open_progress(app, progress);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use app_core::remote_ssh::{RemoteSshCommand, RemoteSshCommandRunner, RemoteSshOutput};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct FakeRunner {
        outputs: Arc<Mutex<Vec<RemoteSshOutput>>>,
    }

    impl FakeRunner {
        fn new(outputs: Vec<RemoteSshOutput>) -> Self {
            Self {
                outputs: Arc::new(Mutex::new(outputs)),
            }
        }
    }

    impl RemoteSshCommandRunner for FakeRunner {
        fn run_ssh_command(&self, _command: &RemoteSshCommand) -> RemoteSshOutput {
            self.outputs.lock().unwrap().remove(0)
        }
    }

    fn ssh_ok(stdout: &str) -> RemoteSshOutput {
        RemoteSshOutput {
            success: true,
            stdout: stdout.into(),
            stderr: String::new(),
            timed_out: false,
            elapsed_ms: 1,
        }
    }

    #[test]
    fn open_workspace_record_matches_legacy_remote_path() {
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
        let record = crate::open_workspaces::OpenWorkspaceRecord {
            path: remote.key(),
            remote: Some(remote),
        };

        assert!(open_workspace_record_matches(&record, "/srv/kodex"));
        assert!(open_workspace_record_matches(
            &record,
            "ssh://devbox:22/srv/kodex"
        ));
    }

    #[test]
    fn remote_profile_open_failure_keeps_existing_workspace_state() {
        let dir = std::env::temp_dir().join(format!(
            "kodex-remote-profile-open-failure-{}",
            uuid::Uuid::new_v4()
        ));
        let workspace = dir.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let paths = app_core::AppPaths::from_root(dir.join(".kodex"));
        let state = AppState::new();
        state.restore_dormant_workspace(workspace.clone()).unwrap();

        let error = open_remote_profile_with_paths(
            &state,
            &paths,
            RemoteOpenRequest {
                request_id: None,
                profile_id: uuid::Uuid::new_v4(),
                remote_path: "/srv/project".into(),
                ssh_password: None,
                agent_cli: AgentCliId::CodexAcp,
            },
            None,
        )
        .unwrap_err();

        assert!(error.contains("Remote machine profile not found"));
        let open = state.list_open_workspaces().unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].workspace.root, workspace);
        state.shutdown_all();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn remote_profile_bootstrap_failure_keeps_existing_workspace_state() {
        let dir = std::env::temp_dir().join(format!(
            "kodex-remote-bootstrap-open-failure-{}",
            uuid::Uuid::new_v4()
        ));
        let workspace = dir.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let paths = app_core::AppPaths::from_root(dir.join(".kodex"));
        let snapshot = app_core::remote_profiles::save_remote_machine_profile(
            &paths,
            workspace_model::RemoteMachineProfileInput {
                id: None,
                display_name: "Devbox".into(),
                ssh_target: "root@devbox".into(),
                ssh_port: Some(36000),
            },
        )
        .unwrap();
        let state = AppState::new();
        state.restore_dormant_workspace(workspace.clone()).unwrap();
        let runner = FakeRunner::new(vec![
            ssh_ok("kodex-ssh-ok"),
            ssh_ok("os=Linux\narch=x86_64\nhome=/root\nmissing=node\n"),
        ]);

        let error = open_remote_profile_with_runner(
            &state,
            &paths,
            RemoteOpenRequest {
                request_id: Some(uuid::Uuid::new_v4()),
                profile_id: snapshot.profiles[0].id,
                remote_path: "/srv/project".into(),
                ssh_password: Some("ssh-secret".into()),
                agent_cli: AgentCliId::CodexAcp,
            },
            None,
            &runner,
        )
        .unwrap_err();

        assert!(error.contains("缺少 bootstrap 依赖"));
        let open = state.list_open_workspaces().unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].workspace.root, workspace);
        state.shutdown_all();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn remote_profile_bootstrap_failure_emits_progress_and_does_not_persist_password() {
        let dir = std::env::temp_dir().join(format!(
            "kodex-remote-bootstrap-progress-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = app_core::AppPaths::from_root(dir.join(".kodex"));
        let snapshot = app_core::remote_profiles::save_remote_machine_profile(
            &paths,
            workspace_model::RemoteMachineProfileInput {
                id: None,
                display_name: "Devbox".into(),
                ssh_target: "root@devbox".into(),
                ssh_port: Some(36000),
            },
        )
        .unwrap();
        let state = AppState::new();
        let runner = FakeRunner::new(vec![
            ssh_ok("kodex-ssh-ok"),
            ssh_ok("os=Linux\narch=x86_64\nhome=/root\nmissing=node\n"),
        ]);
        let request_id = uuid::Uuid::new_v4();
        let mut progress = Vec::new();

        let error = open_remote_profile_with_progress(
            &state,
            &paths,
            RemoteOpenRequest {
                request_id: Some(request_id),
                profile_id: snapshot.profiles[0].id,
                remote_path: "/srv/project".into(),
                ssh_password: Some("ssh-secret".into()),
                agent_cli: AgentCliId::CodexAcp,
            },
            &runner,
            |event| progress.push(event),
        )
        .unwrap_err();

        assert!(error.contains("缺少 bootstrap 依赖"));
        assert!(progress.iter().all(|event| event.request_id == request_id));
        assert!(
            progress
                .iter()
                .any(|event| event.phase == RemoteOpenPhaseKind::Ssh
                    && event.status == RemoteOpenPhaseStatus::Running)
        );
        assert!(
            progress
                .iter()
                .any(|event| event.phase == RemoteOpenPhaseKind::Platform
                    && event.status == RemoteOpenPhaseStatus::Failed)
        );
        assert!(!format!("{progress:?}").contains("ssh-secret"));
        let persisted =
            std::fs::read_to_string(paths.config_dir().join("remote-machines.json")).unwrap();
        assert!(!persisted.contains("ssh-secret"));
        state.shutdown_all();
        let _ = std::fs::remove_dir_all(dir);
    }
}
