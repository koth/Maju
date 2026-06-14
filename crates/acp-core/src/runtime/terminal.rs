use super::agent_process::build_remote_ssh_command_args;
use super::process::{
    agent_spawn_command, apply_process_cwd_and_pwd, build_terminal_command, hide_console_window,
    process_cwd,
};
use super::workspace_paths::validate_remote_client_file_path;
use crate::events::RemoteSshSessionConfig;
use agent_client_protocol::schema::{
    CreateTerminalRequest, CreateTerminalResponse, KillTerminalRequest, KillTerminalResponse,
    ReleaseTerminalRequest, ReleaseTerminalResponse, TerminalExitStatus, TerminalId,
    TerminalOutputRequest, TerminalOutputResponse, WaitForTerminalExitRequest,
    WaitForTerminalExitResponse,
};
use anyhow::{Context, anyhow};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

const KODEX_SSH_ASKPASS_ENV: &str = "KODEX_SSH_ASKPASS";
const KODEX_SSH_ASKPASS_PASSWORD_ENV: &str = "KODEX_SSH_ASKPASS_PASSWORD";

#[derive(Default)]
pub(super) struct TerminalManager {
    next_id: AtomicU64,
    terminals: Mutex<HashMap<String, Arc<ManagedTerminal>>>,
}

struct ManagedTerminal {
    child: Mutex<Option<Child>>,
    output: Mutex<String>,
    truncated: AtomicBool,
    output_byte_limit: Option<usize>,
    exit_status: Mutex<Option<TerminalExitStatus>>,
}

impl TerminalManager {
    pub(super) fn create_terminal(
        &self,
        workspace_root: &str,
        request: &CreateTerminalRequest,
    ) -> anyhow::Result<CreateTerminalResponse> {
        let terminal_id = format!(
            "terminal_{}",
            self.next_id.fetch_add(1, Ordering::Relaxed) + 1
        );

        let mut command = build_terminal_command(request);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let cwd = process_cwd(workspace_root, request.cwd.as_deref());
        apply_process_cwd_and_pwd(&mut command, &cwd);

        for env_var in &request.env {
            command.env(&env_var.name, &env_var.value);
        }
        command.env("PWD", cwd.as_os_str());

        let mut child = command.spawn().with_context(|| {
            format!(
                "failed to spawn terminal command '{}' with args {:?}",
                request.command, request.args
            )
        })?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let terminal = Arc::new(ManagedTerminal {
            child: Mutex::new(Some(child)),
            output: Mutex::new(String::new()),
            truncated: AtomicBool::new(false),
            output_byte_limit: request
                .output_byte_limit
                .map(|limit| limit.min(usize::MAX as u64) as usize),
            exit_status: Mutex::new(None),
        });

        if let Some(stdout) = stdout {
            spawn_terminal_reader(stdout, terminal.clone());
        }
        if let Some(stderr) = stderr {
            spawn_terminal_reader(stderr, terminal.clone());
        }

        self.terminals
            .lock()
            .map_err(|_| anyhow!("terminal registry poisoned"))?
            .insert(terminal_id.clone(), terminal);

        Ok(CreateTerminalResponse::new(TerminalId::new(terminal_id)))
    }

    pub(super) fn create_remote_terminal(
        &self,
        remote_ssh: &RemoteSshSessionConfig,
        request: &CreateTerminalRequest,
    ) -> anyhow::Result<CreateTerminalResponse> {
        let terminal_id = format!(
            "terminal_{}",
            self.next_id.fetch_add(1, Ordering::Relaxed) + 1
        );

        let mut command = build_remote_terminal_command(remote_ssh, request)?;
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(password) = remote_ssh
            .ssh_password
            .as_deref()
            .filter(|password| !password.is_empty())
        {
            configure_ssh_askpass(&mut command, password)?;
        }

        let mut child = command.spawn().with_context(|| {
            format!(
                "failed to spawn remote terminal command '{}' with args {:?}",
                request.command, request.args
            )
        })?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let terminal = Arc::new(ManagedTerminal {
            child: Mutex::new(Some(child)),
            output: Mutex::new(String::new()),
            truncated: AtomicBool::new(false),
            output_byte_limit: request
                .output_byte_limit
                .map(|limit| limit.min(usize::MAX as u64) as usize),
            exit_status: Mutex::new(None),
        });

        if let Some(stdout) = stdout {
            spawn_terminal_reader(stdout, terminal.clone());
        }
        if let Some(stderr) = stderr {
            spawn_terminal_reader(stderr, terminal.clone());
        }

        self.terminals
            .lock()
            .map_err(|_| anyhow!("terminal registry poisoned"))?
            .insert(terminal_id.clone(), terminal);

        Ok(CreateTerminalResponse::new(TerminalId::new(terminal_id)))
    }

    pub(super) fn create_denied_terminal(
        &self,
        request: &CreateTerminalRequest,
        reason: &str,
    ) -> anyhow::Result<CreateTerminalResponse> {
        let terminal_id = format!(
            "terminal_{}",
            self.next_id.fetch_add(1, Ordering::Relaxed) + 1
        );
        let output_byte_limit = request
            .output_byte_limit
            .map(|limit| limit.min(usize::MAX as u64) as usize);
        let mut output = reason.trim().to_string();
        if !output.ends_with('\n') {
            output.push('\n');
        }
        let mut truncated = false;
        if let Some(limit) = output_byte_limit
            && output.len() > limit
        {
            let mut trim_to = output.len() - limit;
            while trim_to < output.len() && !output.is_char_boundary(trim_to) {
                trim_to += 1;
            }
            output.drain(..trim_to);
            truncated = true;
        }

        let terminal = Arc::new(ManagedTerminal {
            child: Mutex::new(None),
            output: Mutex::new(output),
            truncated: AtomicBool::new(truncated),
            output_byte_limit,
            exit_status: Mutex::new(Some(TerminalExitStatus::new().exit_code(Some(1)))),
        });

        self.terminals
            .lock()
            .map_err(|_| anyhow!("terminal registry poisoned"))?
            .insert(terminal_id.clone(), terminal);

        Ok(CreateTerminalResponse::new(TerminalId::new(terminal_id)))
    }

    pub(super) fn terminal_output(
        &self,
        request: &TerminalOutputRequest,
    ) -> anyhow::Result<TerminalOutputResponse> {
        let terminal = self.get_terminal(request.terminal_id.0.as_ref())?;
        let exit_status = terminal.try_update_exit_status()?;
        let output = terminal
            .output
            .lock()
            .map_err(|_| anyhow!("terminal output poisoned"))?
            .clone();

        Ok(
            TerminalOutputResponse::new(output, terminal.truncated.load(Ordering::Relaxed))
                .exit_status(exit_status),
        )
    }

    pub(super) fn wait_for_terminal_exit(
        &self,
        request: &WaitForTerminalExitRequest,
    ) -> anyhow::Result<WaitForTerminalExitResponse> {
        let terminal = self.get_terminal(request.terminal_id.0.as_ref())?;
        let exit_status = terminal.wait_for_exit()?;
        Ok(WaitForTerminalExitResponse::new(exit_status))
    }

    pub(super) fn kill_terminal(
        &self,
        request: &KillTerminalRequest,
    ) -> anyhow::Result<KillTerminalResponse> {
        let terminal = self.get_terminal(request.terminal_id.0.as_ref())?;
        terminal.kill()?;
        Ok(KillTerminalResponse::new())
    }

    pub(super) fn release_terminal(
        &self,
        request: &ReleaseTerminalRequest,
    ) -> anyhow::Result<ReleaseTerminalResponse> {
        let terminal = self
            .terminals
            .lock()
            .map_err(|_| anyhow!("terminal registry poisoned"))?
            .remove(request.terminal_id.0.as_ref())
            .ok_or_else(|| anyhow!("unknown terminal id {}", request.terminal_id.0))?;

        let _ = terminal.try_update_exit_status()?;
        if terminal.current_exit_status()?.is_none() {
            terminal.kill()?;
        }

        Ok(ReleaseTerminalResponse::new())
    }

    fn get_terminal(&self, terminal_id: &str) -> anyhow::Result<Arc<ManagedTerminal>> {
        self.terminals
            .lock()
            .map_err(|_| anyhow!("terminal registry poisoned"))?
            .get(terminal_id)
            .cloned()
            .ok_or_else(|| anyhow!("unknown terminal id {terminal_id}"))
    }
}

impl Drop for TerminalManager {
    fn drop(&mut self) {
        if let Ok(terminals) = self.terminals.lock() {
            for terminal in terminals.values() {
                let _ = terminal.kill();
            }
        }
    }
}

impl ManagedTerminal {
    fn current_exit_status(&self) -> anyhow::Result<Option<TerminalExitStatus>> {
        Ok(self
            .exit_status
            .lock()
            .map_err(|_| anyhow!("terminal exit status poisoned"))?
            .clone())
    }

    fn try_update_exit_status(&self) -> anyhow::Result<Option<TerminalExitStatus>> {
        if let Some(status) = self.current_exit_status()? {
            return Ok(Some(status));
        }

        let exit = {
            let mut child = self
                .child
                .lock()
                .map_err(|_| anyhow!("terminal child poisoned"))?;
            match child.as_mut() {
                Some(child) => child.try_wait()?,
                None => None,
            }
        };

        if let Some(exit) = exit {
            let status = to_terminal_exit_status(exit);
            *self
                .exit_status
                .lock()
                .map_err(|_| anyhow!("terminal exit status poisoned"))? = Some(status.clone());
            return Ok(Some(status));
        }

        Ok(None)
    }

    fn wait_for_exit(&self) -> anyhow::Result<TerminalExitStatus> {
        if let Some(status) = self.current_exit_status()? {
            return Ok(status);
        }

        let exit = {
            let mut child = self
                .child
                .lock()
                .map_err(|_| anyhow!("terminal child poisoned"))?;
            match child.as_mut() {
                Some(child) => child.wait()?,
                None => {
                    return self
                        .current_exit_status()?
                        .ok_or_else(|| anyhow!("terminal already released"));
                }
            }
        };

        let status = to_terminal_exit_status(exit);
        *self
            .exit_status
            .lock()
            .map_err(|_| anyhow!("terminal exit status poisoned"))? = Some(status.clone());
        Ok(status)
    }

    fn kill(&self) -> anyhow::Result<()> {
        if self.current_exit_status()?.is_some() {
            return Ok(());
        }

        let exit = {
            let mut child = self
                .child
                .lock()
                .map_err(|_| anyhow!("terminal child poisoned"))?;
            let Some(child) = child.as_mut() else {
                return Ok(());
            };

            match child.try_wait()? {
                Some(exit) => exit,
                None => {
                    child.kill()?;
                    child.wait()?
                }
            }
        };

        let status = to_terminal_exit_status(exit);
        *self
            .exit_status
            .lock()
            .map_err(|_| anyhow!("terminal exit status poisoned"))? = Some(status);
        Ok(())
    }

    fn push_output(&self, chunk: &str) -> anyhow::Result<()> {
        let mut output = self
            .output
            .lock()
            .map_err(|_| anyhow!("terminal output poisoned"))?;
        output.push_str(chunk);

        if let Some(limit) = self.output_byte_limit {
            if output.len() > limit {
                let mut trim_to = output.len() - limit;
                while trim_to < output.len() && !output.is_char_boundary(trim_to) {
                    trim_to += 1;
                }
                output.drain(..trim_to);
                self.truncated.store(true, Ordering::Relaxed);
            }
        }

        Ok(())
    }
}

impl Drop for ManagedTerminal {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

fn spawn_terminal_reader<R>(reader: R, terminal: Arc<ManagedTerminal>)
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut reader = reader;
        let mut buffer = [0_u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    let chunk = String::from_utf8_lossy(&buffer[..count]);
                    let _ = terminal.push_output(&chunk);
                }
                Err(_) => break,
            }
        }
    });
}

fn to_terminal_exit_status(status: ExitStatus) -> TerminalExitStatus {
    TerminalExitStatus::new().exit_code(status.code().map(|code| code.max(0) as u32))
}

fn build_remote_terminal_command(
    remote_ssh: &RemoteSshSessionConfig,
    request: &CreateTerminalRequest,
) -> anyhow::Result<Command> {
    let remote_command = remote_terminal_shell_command(remote_ssh, request)?;
    let has_password = remote_ssh
        .ssh_password
        .as_deref()
        .is_some_and(|password| !password.is_empty());
    let args = build_remote_ssh_command_args(
        &remote_ssh.ssh_target,
        remote_ssh.ssh_port,
        remote_command,
        has_password,
    );
    let ssh_command = remote_ssh
        .ssh_command
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("ssh"));
    let mut command = agent_spawn_command(&ssh_command, &args);
    hide_console_window(&mut command);
    Ok(command)
}

fn remote_terminal_shell_command(
    remote_ssh: &RemoteSshSessionConfig,
    request: &CreateTerminalRequest,
) -> anyhow::Result<String> {
    let cwd = remote_terminal_cwd(remote_ssh, request.cwd.as_deref())?;
    let mut lines = vec![format!("cd {} || exit $?", shell_quote(&cwd))];
    let mut env = Vec::new();
    for env_var in &request.env {
        if !is_valid_env_name(&env_var.name) {
            return Err(anyhow!("invalid remote terminal environment name"));
        }
        env.push(format!("{}={}", env_var.name, shell_quote(&env_var.value)));
    }
    env.push(format!("PWD={}", shell_quote(&cwd)));

    let command = if request.args.is_empty() {
        ["sh".to_string(), "-lc".to_string(), request.command.clone()]
            .into_iter()
            .map(|part| shell_quote(&part))
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        std::iter::once(request.command.as_str())
            .chain(request.args.iter().map(String::as_str))
            .map(shell_quote)
            .collect::<Vec<_>>()
            .join(" ")
    };
    lines.push(format!("exec env {} {}", env.join(" "), command));
    Ok(lines.join("; "))
}

fn remote_terminal_cwd(
    remote_ssh: &RemoteSshSessionConfig,
    requested_cwd: Option<&Path>,
) -> anyhow::Result<String> {
    match requested_cwd {
        Some(cwd) => validate_remote_client_file_path(&remote_ssh.remote_workspace_root, cwd),
        None => validate_remote_client_file_path(
            &remote_ssh.remote_workspace_root,
            Path::new(&remote_ssh.remote_workspace_root),
        ),
    }
}

fn is_valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn configure_ssh_askpass(command: &mut Command, password: &str) -> anyhow::Result<()> {
    let askpass = std::env::current_exe()?;
    command
        .env("SSH_ASKPASS", askpass)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("DISPLAY", "kodex")
        .env(KODEX_SSH_ASKPASS_ENV, "1")
        .env(KODEX_SSH_ASKPASS_PASSWORD_ENV, password);
    Ok(())
}

fn shell_quote(value: &str) -> String {
    shell_words::quote(value).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denied_terminal_returns_failure_without_spawning_process() {
        let manager = TerminalManager::default();
        let request =
            CreateTerminalRequest::new("session-1", "pnpm".to_string()).args(vec!["build".into()]);

        let created = manager
            .create_denied_terminal(
                &request,
                "Permission rejected by user. Command was not executed.",
            )
            .expect("denied terminal should be created");
        let output = manager
            .terminal_output(&TerminalOutputRequest::new(
                "session-1",
                created.terminal_id.clone(),
            ))
            .expect("denied terminal should provide output");
        let exit = manager
            .wait_for_terminal_exit(&WaitForTerminalExitRequest::new(
                "session-1",
                created.terminal_id,
            ))
            .expect("denied terminal should be exited");

        assert!(output.output.contains("Permission rejected by user"));
        assert_eq!(output.exit_status.unwrap().exit_code, Some(1));
        assert_eq!(exit.exit_status.exit_code, Some(1));
    }

    #[test]
    fn remote_terminal_shell_command_runs_shell_in_remote_cwd() {
        let config = remote_config();
        let request = CreateTerminalRequest::new("session-1", "pwd && rg TODO".to_string())
            .cwd(PathBuf::from("src"));

        let command = remote_terminal_shell_command(&config, &request).unwrap();

        assert!(command.contains("cd /srv/project/src || exit $?"));
        assert!(command.contains("exec env "));
        assert!(command.contains("PWD=/srv/project/src"));
        assert!(command.contains("sh -lc 'pwd && rg TODO'"));
    }

    #[test]
    fn remote_terminal_shell_command_quotes_argv_commands() {
        let config = remote_config();
        let request = CreateTerminalRequest::new("session-1", "python".to_string())
            .args(vec!["-c".into(), "print('ok')".into()]);

        let command = remote_terminal_shell_command(&config, &request).unwrap();

        assert!(command.contains("cd /srv/project || exit $?"));
        assert!(command.contains("exec env PWD=/srv/project python -c"));
        assert!(command.contains("print"));
        assert!(command.contains("ok"));
    }

    #[test]
    fn remote_terminal_shell_command_rejects_cwd_escape() {
        let config = remote_config();
        let request = CreateTerminalRequest::new("session-1", "pwd".to_string())
            .cwd(PathBuf::from("../outside"));

        assert!(remote_terminal_shell_command(&config, &request).is_err());
    }

    fn remote_config() -> RemoteSshSessionConfig {
        RemoteSshSessionConfig {
            ssh_target: "devbox".into(),
            ssh_port: Some(2222),
            remote_workspace_root: "/srv/project".into(),
            local_port: 0,
            remote_port: 0,
            reverse_forwards: Vec::new(),
            ssh_command: None,
            ssh_password: None,
        }
    }
}
