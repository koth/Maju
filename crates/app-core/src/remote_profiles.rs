use crate::AppPaths;
use crate::remote_ssh::{
    RemoteSshCommand, RemoteSshCommandRunner, SystemRemoteSshCommandRunner, first_nonempty,
    sanitize_ssh_diagnostic,
};
use anyhow::{Context, Result, anyhow};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use workspace_model::{
    AgentCliId, RemoteLinuxWorkspace, RemoteMachineProfile, RemoteMachineProfileInput,
    RemoteMachineProfilesSnapshot, RemoteMachineValidation, RemoteMachineValidationPhase,
    RemoteMachineValidationRequest, RemoteOpenRequest, RemoteValidationPhaseKind,
    RemoteValidationPhaseStatus,
};

const REMOTE_MACHINES_FILE: &str = "remote-machines.json";
const SSH_PROBE_TIMEOUT: Duration = Duration::from_secs(8);

pub fn load_remote_machine_profiles(paths: &AppPaths) -> RemoteMachineProfilesSnapshot {
    std::fs::read_to_string(remote_machines_path(paths))
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

pub fn save_remote_machine_profile(
    paths: &AppPaths,
    input: RemoteMachineProfileInput,
) -> Result<RemoteMachineProfilesSnapshot> {
    let normalized = normalize_profile_input(input)?;
    let mut snapshot = load_remote_machine_profiles(paths);
    let now = now_ms();
    let profile = match normalized.id {
        Some(id) => {
            let existing = snapshot
                .profiles
                .iter()
                .find(|profile| profile.id == id)
                .cloned();
            let last_validation = existing
                .as_ref()
                .filter(|profile| profile_identity_matches(profile, &normalized))
                .and_then(|profile| profile.last_validation.clone());
            RemoteMachineProfile {
                id,
                display_name: normalized.display_name,
                ssh_target: normalized.ssh_target,
                ssh_port: normalized.ssh_port,
                created_at_ms: existing
                    .as_ref()
                    .map(|profile| profile.created_at_ms)
                    .unwrap_or(now),
                updated_at_ms: now,
                last_validation,
            }
        }
        None => RemoteMachineProfile {
            id: uuid::Uuid::new_v4(),
            display_name: normalized.display_name,
            ssh_target: normalized.ssh_target,
            ssh_port: normalized.ssh_port,
            created_at_ms: now,
            updated_at_ms: now,
            last_validation: None,
        },
    };

    if let Some(existing) = snapshot
        .profiles
        .iter_mut()
        .find(|existing| existing.id == profile.id)
    {
        *existing = profile;
    } else {
        snapshot.profiles.push(profile);
    }
    snapshot
        .profiles
        .sort_by(|a, b| a.display_name.cmp(&b.display_name));
    write_remote_machine_profiles(paths, &snapshot)?;
    Ok(snapshot)
}

pub fn delete_remote_machine_profile(
    paths: &AppPaths,
    profile_id: uuid::Uuid,
) -> Result<RemoteMachineProfilesSnapshot> {
    let mut snapshot = load_remote_machine_profiles(paths);
    snapshot.profiles.retain(|profile| profile.id != profile_id);
    write_remote_machine_profiles(paths, &snapshot)?;
    Ok(snapshot)
}

pub fn get_remote_machine_profile(
    paths: &AppPaths,
    profile_id: uuid::Uuid,
) -> Result<RemoteMachineProfile> {
    load_remote_machine_profiles(paths)
        .profiles
        .into_iter()
        .find(|profile| profile.id == profile_id)
        .ok_or_else(|| anyhow!("Remote machine profile not found"))
}

pub fn validate_remote_machine_profile(
    paths: &AppPaths,
    request: RemoteMachineValidationRequest,
) -> Result<RemoteMachineProfilesSnapshot> {
    validate_remote_machine_profile_and_persist(paths, request, &SystemRemoteSshCommandRunner)
}

fn validate_remote_machine_profile_and_persist<R: RemoteSshCommandRunner>(
    paths: &AppPaths,
    request: RemoteMachineValidationRequest,
    runner: &R,
) -> Result<RemoteMachineProfilesSnapshot> {
    let mut snapshot = load_remote_machine_profiles(paths);
    let profile = snapshot
        .profiles
        .iter()
        .find(|profile| profile.id == request.profile_id)
        .cloned()
        .ok_or_else(|| anyhow!("Remote machine profile not found"))?;

    let validation = validate_remote_machine_profile_with_runner(
        &profile,
        request.remote_path.as_deref(),
        request.ssh_password.as_deref(),
        request.agent_cli,
        request.include_acp,
        runner,
    );
    let Some(existing) = snapshot
        .profiles
        .iter_mut()
        .find(|profile| profile.id == request.profile_id)
    else {
        return Err(anyhow!("Remote machine profile not found"));
    };
    existing.last_validation = Some(validation);
    existing.updated_at_ms = now_ms();
    write_remote_machine_profiles(paths, &snapshot)?;
    Ok(snapshot)
}

pub fn remote_workspace_from_profile(
    paths: &AppPaths,
    request: RemoteOpenRequest,
) -> Result<RemoteLinuxWorkspace> {
    let snapshot = load_remote_machine_profiles(paths);
    let profile = snapshot
        .profiles
        .iter()
        .find(|profile| profile.id == request.profile_id)
        .ok_or_else(|| anyhow!("Remote machine profile not found"))?;
    let remote_path = request.remote_path.trim().to_string();
    if !remote_path.starts_with('/') {
        anyhow::bail!("Remote workspace path must be absolute");
    }
    Ok(RemoteLinuxWorkspace {
        profile_id: Some(profile.id),
        ssh_target: profile.ssh_target.clone(),
        ssh_port: profile.ssh_port,
        remote_path,
        ssh_password: request.ssh_password.filter(|password| !password.is_empty()),
        agent_cli: Some(request.agent_cli),
        agent_command: remote_agent_command_for(request.agent_cli),
        local_port: None,
        remote_port: None,
    })
}

fn write_remote_machine_profiles(
    paths: &AppPaths,
    snapshot: &RemoteMachineProfilesSnapshot,
) -> Result<()> {
    let dir = paths.config_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create config directory {}", dir.display()))?;
    let path = remote_machines_path(paths);
    let content = serde_json::to_string_pretty(snapshot)?;
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write remote machines {}", path.display()))
}

fn remote_machines_path(paths: &AppPaths) -> std::path::PathBuf {
    paths.config_dir().join(REMOTE_MACHINES_FILE)
}

fn normalize_profile_input(input: RemoteMachineProfileInput) -> Result<RemoteMachineProfileInput> {
    let display_name = input.display_name.trim().to_string();
    let ssh_target = input.ssh_target.trim().to_string();

    if display_name.is_empty() {
        anyhow::bail!("Remote machine name cannot be empty");
    }
    if ssh_target.is_empty() {
        anyhow::bail!("SSH target cannot be empty");
    }
    if input.ssh_port == Some(0) {
        anyhow::bail!("SSH port must be between 1 and 65535");
    }

    Ok(RemoteMachineProfileInput {
        id: input.id,
        display_name,
        ssh_target,
        ssh_port: input.ssh_port,
    })
}

fn profile_identity_matches(
    profile: &RemoteMachineProfile,
    input: &RemoteMachineProfileInput,
) -> bool {
    profile.ssh_target == input.ssh_target && profile.ssh_port == input.ssh_port
}

fn remote_agent_command_for(agent_cli: AgentCliId) -> Option<String> {
    crate::settings::remote_linux_command_for_agent(agent_cli)
}

fn validate_remote_machine_profile_with_runner<R: RemoteSshCommandRunner>(
    profile: &RemoteMachineProfile,
    remote_path: Option<&str>,
    ssh_password: Option<&str>,
    _agent_cli: Option<AgentCliId>,
    include_acp: bool,
    runner: &R,
) -> RemoteMachineValidation {
    let mut phases = Vec::new();
    let checked_at_ms = now_ms();
    let validation_path = remote_path
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .unwrap_or("~");

    let ssh = run_ssh_phase(
        profile,
        RemoteValidationPhaseKind::Ssh,
        "printf kodex-ssh-ok".to_string(),
        ssh_password,
        runner,
    );
    let ssh_ok = ssh.status == RemoteValidationPhaseStatus::Succeeded;
    phases.push(ssh);
    if !ssh_ok {
        return validation(checked_at_ms, Some(validation_path), phases);
    }

    let path_phase = match crate::remote_bootstrap::remote_directory_test_command(validation_path) {
        Some(command) => run_ssh_phase(
            profile,
            RemoteValidationPhaseKind::RemotePath,
            command,
            ssh_password,
            runner,
        ),
        None => phase(
            RemoteValidationPhaseKind::RemotePath,
            RemoteValidationPhaseStatus::Failed,
            0,
            Some("Remote validation path must be an absolute path, ~, or ~/path".into()),
        ),
    };
    let path_ok = path_phase.status == RemoteValidationPhaseStatus::Succeeded;
    phases.push(path_phase);
    if !path_ok {
        return validation(checked_at_ms, Some(validation_path), phases);
    }

    phases.push(phase(
        RemoteValidationPhaseKind::AcpReady,
        RemoteValidationPhaseStatus::Skipped,
        0,
        Some(if include_acp {
            "ACP readiness is verified during remote workspace open".into()
        } else {
            "ACP readiness probe not requested".into()
        }),
    ));

    validation(checked_at_ms, Some(validation_path), phases)
}

fn validation(
    checked_at_ms: u64,
    remote_path: Option<&str>,
    phases: Vec<RemoteMachineValidationPhase>,
) -> RemoteMachineValidation {
    RemoteMachineValidation {
        ok: phases
            .iter()
            .all(|phase| phase.status != RemoteValidationPhaseStatus::Failed),
        checked_at_ms,
        remote_path: remote_path.map(|path| path.trim().to_string()),
        phases,
    }
}

fn run_ssh_phase<R: RemoteSshCommandRunner>(
    profile: &RemoteMachineProfile,
    kind: RemoteValidationPhaseKind,
    remote_command: String,
    ssh_password: Option<&str>,
    runner: &R,
) -> RemoteMachineValidationPhase {
    let output = runner.run_ssh_command(&RemoteSshCommand::new(
        profile.ssh_target.clone(),
        profile.ssh_port,
        remote_command,
        ssh_password,
        SSH_PROBE_TIMEOUT,
    ));
    if output.timed_out {
        return phase(
            kind,
            RemoteValidationPhaseStatus::Failed,
            output.elapsed_ms,
            Some("SSH probe timed out".into()),
        );
    }
    if output.success {
        return phase(
            kind,
            RemoteValidationPhaseStatus::Succeeded,
            output.elapsed_ms,
            None,
        );
    }
    let message = first_nonempty(&output.stderr, &output.stdout)
        .map(sanitize_ssh_diagnostic)
        .unwrap_or_else(|| "SSH probe failed without output".into());
    phase(
        kind,
        RemoteValidationPhaseStatus::Failed,
        output.elapsed_ms,
        Some(message),
    )
}

fn phase(
    phase: RemoteValidationPhaseKind,
    status: RemoteValidationPhaseStatus,
    elapsed_ms: u64,
    message: Option<String>,
) -> RemoteMachineValidationPhase {
    RemoteMachineValidationPhase {
        phase,
        status,
        elapsed_ms,
        message,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[derive(Clone)]
    struct FakeRunner {
        outputs: std::sync::Arc<std::sync::Mutex<Vec<crate::remote_ssh::RemoteSshOutput>>>,
        calls: std::sync::Arc<std::sync::Mutex<Vec<RemoteSshCommand>>>,
        passwords: std::sync::Arc<std::sync::Mutex<Vec<Option<String>>>>,
    }

    impl FakeRunner {
        fn new(outputs: Vec<crate::remote_ssh::RemoteSshOutput>) -> Self {
            Self {
                outputs: std::sync::Arc::new(std::sync::Mutex::new(outputs)),
                calls: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
                passwords: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }

        fn calls(&self) -> Vec<RemoteSshCommand> {
            self.calls.lock().unwrap().clone()
        }

        fn passwords(&self) -> Vec<Option<String>> {
            self.passwords.lock().unwrap().clone()
        }
    }

    impl RemoteSshCommandRunner for FakeRunner {
        fn run_ssh_command(
            &self,
            command: &RemoteSshCommand,
        ) -> crate::remote_ssh::RemoteSshOutput {
            self.calls.lock().unwrap().push(command.clone());
            self.passwords
                .lock()
                .unwrap()
                .push(command.ssh_password.clone());
            self.outputs.lock().unwrap().remove(0)
        }
    }

    fn ok_output() -> crate::remote_ssh::RemoteSshOutput {
        crate::remote_ssh::RemoteSshOutput {
            success: true,
            stdout: "ok".into(),
            stderr: String::new(),
            timed_out: false,
            elapsed_ms: 1,
        }
    }

    fn fail_output(message: &str) -> crate::remote_ssh::RemoteSshOutput {
        crate::remote_ssh::RemoteSshOutput {
            success: false,
            stdout: String::new(),
            stderr: message.into(),
            timed_out: false,
            elapsed_ms: 1,
        }
    }

    fn profile_input() -> RemoteMachineProfileInput {
        RemoteMachineProfileInput {
            id: None,
            display_name: "Devbox".into(),
            ssh_target: " root@devbox ".into(),
            ssh_port: Some(36000),
        }
    }

    fn profile() -> RemoteMachineProfile {
        let paths = AppPaths::from_root(tempdir().unwrap().path().join(".kodex"));
        let mut snapshot = save_remote_machine_profile(&paths, profile_input()).unwrap();
        snapshot.profiles.remove(0)
    }

    #[test]
    fn remote_profiles_default_to_empty_and_persist_without_secrets() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));

        assert!(load_remote_machine_profiles(&paths).profiles.is_empty());

        let snapshot = save_remote_machine_profile(&paths, profile_input()).unwrap();
        assert_eq!(snapshot.profiles.len(), 1);
        assert_eq!(snapshot.profiles[0].ssh_target, "root@devbox");
        assert_eq!(snapshot.profiles[0].display_name, "Devbox");

        let serialized = std::fs::read_to_string(remote_machines_path(&paths)).unwrap();
        assert!(serialized.contains("root@devbox"));
        assert!(!serialized.contains("agent_cli"));
        assert!(!serialized.contains("agent_command"));
        assert!(!serialized.contains("auth_hint"));
        assert!(!serialized.to_ascii_lowercase().contains("password"));
        assert!(!serialized.to_ascii_lowercase().contains("passphrase"));
        assert!(!serialized.contains("PRIVATE KEY"));
    }

    #[test]
    fn remote_profiles_update_and_delete() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));
        let created = save_remote_machine_profile(&paths, profile_input()).unwrap();
        let id = created.profiles[0].id;

        let updated = save_remote_machine_profile(
            &paths,
            RemoteMachineProfileInput {
                id: Some(id),
                display_name: "Prod".into(),
                ssh_target: "root@prod".into(),
                ssh_port: None,
            },
        )
        .unwrap();
        assert_eq!(updated.profiles.len(), 1);
        assert_eq!(updated.profiles[0].display_name, "Prod");
        assert_eq!(
            updated.profiles[0].created_at_ms,
            created.profiles[0].created_at_ms
        );

        let deleted = delete_remote_machine_profile(&paths, id).unwrap();
        assert!(deleted.profiles.is_empty());
    }

    #[test]
    fn remote_profiles_reject_invalid_or_secret_like_input() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));

        let mut missing_name = profile_input();
        missing_name.display_name = " ".into();
        assert!(save_remote_machine_profile(&paths, missing_name).is_err());

        let mut invalid_port = profile_input();
        invalid_port.ssh_port = Some(0);
        assert!(save_remote_machine_profile(&paths, invalid_port).is_err());
    }

    #[test]
    fn remote_validation_succeeds_with_path_checks_and_ignores_agent() {
        let runner = FakeRunner::new(vec![ok_output(), ok_output()]);
        let validation = validate_remote_machine_profile_with_runner(
            &profile(),
            Some("/srv/project"),
            None,
            Some(AgentCliId::CodexAcp),
            false,
            &runner,
        );

        assert!(validation.ok);
        assert_eq!(validation.phases.len(), 3);
        assert_eq!(validation.phases[0].phase, RemoteValidationPhaseKind::Ssh);
        assert_eq!(
            validation.phases[2].status,
            RemoteValidationPhaseStatus::Skipped
        );
        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls[1].remote_command.contains("test -d /srv/project"));
    }

    #[test]
    fn remote_machine_validation_does_not_include_agent_phase() {
        let runner = FakeRunner::new(vec![ok_output(), ok_output()]);
        let validation = validate_remote_machine_profile_with_runner(
            &profile(),
            Some("/srv/project"),
            None,
            None,
            false,
            &runner,
        );

        assert!(validation.ok);
        assert_eq!(validation.phases.len(), 3);
        assert_eq!(
            validation.phases[2].phase,
            RemoteValidationPhaseKind::AcpReady
        );
        assert_eq!(runner.calls().len(), 2);
    }

    #[test]
    fn remote_machine_validation_defaults_empty_path_to_remote_home() {
        let runner = FakeRunner::new(vec![ok_output(), ok_output()]);
        let validation = validate_remote_machine_profile_with_runner(
            &profile(),
            None,
            None,
            None,
            false,
            &runner,
        );

        assert!(validation.ok);
        assert_eq!(validation.remote_path.as_deref(), Some("~"));
        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls[1].remote_command.contains("test -d \"$HOME\""));
    }

    #[test]
    fn remote_machine_validation_uses_askpass_when_password_is_provided() {
        let runner = FakeRunner::new(vec![ok_output(), ok_output()]);
        let validation = validate_remote_machine_profile_with_runner(
            &profile(),
            Some("~/project"),
            Some("ssh-secret"),
            None,
            false,
            &runner,
        );

        assert!(validation.ok);
        let calls = runner.calls();
        assert_eq!(
            runner.passwords(),
            vec![Some("ssh-secret".into()), Some("ssh-secret".into())]
        );
        let args = crate::remote_ssh::build_ssh_args(&calls[0]);
        assert!(args.contains(&"NumberOfPasswordPrompts=1".to_string()));
        assert!(!args.contains(&"BatchMode=yes".to_string()));
        assert!(
            calls[1]
                .remote_command
                .contains("test -d \"$HOME\"/project")
        );
    }

    #[test]
    fn remote_validation_reports_unreachable_ssh_and_redacts_secret_output() {
        let runner = FakeRunner::new(vec![fail_output("password=secret rejected")]);
        let validation = validate_remote_machine_profile_with_runner(
            &profile(),
            Some("/srv/project"),
            None,
            None,
            false,
            &runner,
        );

        assert!(!validation.ok);
        assert_eq!(validation.phases.len(), 1);
        assert_eq!(
            validation.phases[0].message.as_deref(),
            Some("Credential details redacted")
        );
    }

    #[test]
    fn remote_validation_reports_auth_failure_without_redacting_password_method() {
        let runner = FakeRunner::new(vec![fail_output("Permission denied (publickey,password).")]);
        let validation = validate_remote_machine_profile_with_runner(
            &profile(),
            None,
            None,
            None,
            false,
            &runner,
        );

        assert!(!validation.ok);
        assert_eq!(validation.remote_path.as_deref(), Some("~"));
        assert!(
            validation.phases[0]
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("本次 SSH 密码")
        );
    }

    #[test]
    fn remote_validation_rejects_relative_path_before_remote_path_probe() {
        let runner = FakeRunner::new(vec![ok_output()]);
        let validation = validate_remote_machine_profile_with_runner(
            &profile(),
            Some("project"),
            None,
            None,
            false,
            &runner,
        );

        assert!(!validation.ok);
        assert_eq!(runner.calls().len(), 1);
        assert_eq!(
            validation.phases[1].phase,
            RemoteValidationPhaseKind::RemotePath
        );
    }

    #[test]
    fn remote_validation_reports_timeout() {
        let runner = FakeRunner::new(vec![crate::remote_ssh::RemoteSshOutput {
            success: false,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: true,
            elapsed_ms: 1,
        }]);

        let validation = validate_remote_machine_profile_with_runner(
            &profile(),
            Some("/srv/project"),
            None,
            None,
            false,
            &runner,
        );

        assert!(!validation.ok);
        assert_eq!(
            validation.phases[0].message.as_deref(),
            Some("SSH probe timed out")
        );
    }

    #[test]
    fn validation_result_is_persisted_on_profile() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));
        let snapshot = save_remote_machine_profile(&paths, profile_input()).unwrap();
        let id = snapshot.profiles[0].id;
        let request = RemoteMachineValidationRequest {
            profile_id: id,
            remote_path: Some("project".into()),
            ssh_password: None,
            agent_cli: None,
            include_acp: false,
        };

        let runner = FakeRunner::new(vec![ok_output()]);
        let updated =
            validate_remote_machine_profile_and_persist(&paths, request, &runner).unwrap();

        let validation = updated.profiles[0].last_validation.as_ref().unwrap();
        assert!(!validation.ok);
        assert_eq!(validation.remote_path.as_deref(), Some("project"));
    }

    #[test]
    fn remote_open_request_uses_profile_without_replacing_ports() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));
        let snapshot = save_remote_machine_profile(&paths, profile_input()).unwrap();
        let profile = &snapshot.profiles[0];

        let remote = remote_workspace_from_profile(
            &paths,
            RemoteOpenRequest {
                request_id: None,
                profile_id: profile.id,
                remote_path: "/srv/project".into(),
                ssh_password: Some("ssh-secret".into()),
                agent_cli: AgentCliId::CodexAcp,
            },
        )
        .unwrap();

        assert_eq!(remote.ssh_target, "root@devbox");
        assert_eq!(remote.ssh_port, Some(36000));
        assert_eq!(remote.remote_path, "/srv/project");
        assert_eq!(remote.ssh_password.as_deref(), Some("ssh-secret"));
        assert_eq!(remote.agent_cli, Some(AgentCliId::CodexAcp));
        assert_eq!(remote.agent_command.as_deref(), Some("codex-acp"));
        assert!(remote.local_port.is_none());
        assert!(remote.remote_port.is_none());
    }

    #[test]
    fn remote_open_request_does_not_serialize_one_time_password() {
        let request = RemoteOpenRequest {
            request_id: Some(uuid::Uuid::new_v4()),
            profile_id: uuid::Uuid::new_v4(),
            remote_path: "/srv/project".into(),
            ssh_password: Some("ssh-secret".into()),
            agent_cli: AgentCliId::CodexAcp,
        };

        let serialized = serde_json::to_string(&request).unwrap();

        assert!(serialized.contains("request_id"));
        assert!(!serialized.contains("ssh-secret"));
        assert!(!serialized.contains("ssh_password"));
    }
}
