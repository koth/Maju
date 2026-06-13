use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(unix)]
use std::path::PathBuf;

pub const DEFAULT_SSH_CONNECT_TIMEOUT_SECS: u64 = 5;
const KODEX_SSH_ASKPASS_ENV: &str = "KODEX_SSH_ASKPASS";
const KODEX_SSH_ASKPASS_PASSWORD_ENV: &str = "KODEX_SSH_ASKPASS_PASSWORD";

#[derive(Debug, Clone)]
pub struct RemoteSshCommand {
    pub ssh_target: String,
    pub ssh_port: Option<u16>,
    pub remote_command: String,
    pub ssh_password: Option<String>,
    pub stdin: Option<Vec<u8>>,
    pub timeout: Duration,
    pub connect_timeout_secs: u64,
}

impl RemoteSshCommand {
    pub fn new(
        ssh_target: impl Into<String>,
        ssh_port: Option<u16>,
        remote_command: impl Into<String>,
        ssh_password: Option<&str>,
        timeout: Duration,
    ) -> Self {
        Self {
            ssh_target: ssh_target.into(),
            ssh_port,
            remote_command: remote_command.into(),
            ssh_password: ssh_password
                .map(str::to_string)
                .filter(|password| !password.is_empty()),
            stdin: None,
            timeout,
            connect_timeout_secs: DEFAULT_SSH_CONNECT_TIMEOUT_SECS,
        }
    }

    pub fn with_stdin(mut self, stdin: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(stdin.into());
        self
    }
}

#[derive(Debug, Clone)]
pub struct RemoteSshOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub elapsed_ms: u64,
}

pub trait RemoteSshCommandRunner {
    fn run_ssh_command(&self, command: &RemoteSshCommand) -> RemoteSshOutput;
}

pub struct SystemRemoteSshCommandRunner;

impl RemoteSshCommandRunner for SystemRemoteSshCommandRunner {
    fn run_ssh_command(&self, request: &RemoteSshCommand) -> RemoteSshOutput {
        let started = Instant::now();
        let mut command = Command::new("ssh");
        command
            .args(build_ssh_args(request))
            .stdin(if request.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(password) = request.ssh_password.as_deref() {
            if let Err(error) = configure_ssh_askpass(&mut command, password) {
                return RemoteSshOutput {
                    success: false,
                    stdout: String::new(),
                    stderr: format!("Failed to configure SSH password prompt: {error}"),
                    timed_out: false,
                    elapsed_ms: elapsed_ms(started),
                };
            }
        }
        #[cfg(windows)]
        command.creation_flags(0x08000000);

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                return RemoteSshOutput {
                    success: false,
                    stdout: String::new(),
                    stderr: format!("Failed to start SSH command: {error}"),
                    timed_out: false,
                    elapsed_ms: elapsed_ms(started),
                };
            }
        };
        let mut stdin_writer = if let Some(stdin) = request.stdin.clone() {
            child.stdin.take().map(|mut child_stdin| {
                thread::spawn(move || {
                    let _ = child_stdin.write_all(&stdin);
                })
            })
        } else {
            None
        };

        loop {
            match child.try_wait() {
                Ok(Some(_)) => {
                    if let Some(writer) = stdin_writer.take() {
                        let _ = writer.join();
                    }
                    let output = match child.wait_with_output() {
                        Ok(output) => output,
                        Err(error) => {
                            return RemoteSshOutput {
                                success: false,
                                stdout: String::new(),
                                stderr: format!("Failed to read SSH command output: {error}"),
                                timed_out: false,
                                elapsed_ms: elapsed_ms(started),
                            };
                        }
                    };
                    return RemoteSshOutput {
                        success: output.status.success(),
                        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                        timed_out: false,
                        elapsed_ms: elapsed_ms(started),
                    };
                }
                Ok(None) if started.elapsed() >= request.timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    if let Some(writer) = stdin_writer.take() {
                        let _ = writer.join();
                    }
                    return RemoteSshOutput {
                        success: false,
                        stdout: String::new(),
                        stderr: String::new(),
                        timed_out: true,
                        elapsed_ms: elapsed_ms(started),
                    };
                }
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Err(error) => {
                    let _ = child.kill();
                    if let Some(writer) = stdin_writer.take() {
                        let _ = writer.join();
                    }
                    return RemoteSshOutput {
                        success: false,
                        stdout: String::new(),
                        stderr: format!("Failed to wait for SSH command: {error}"),
                        timed_out: false,
                        elapsed_ms: elapsed_ms(started),
                    };
                }
            }
        }
    }
}

pub fn build_ssh_args(request: &RemoteSshCommand) -> Vec<String> {
    let mut args = Vec::new();
    if request.ssh_password.is_some() {
        args.extend(["-o".to_string(), "NumberOfPasswordPrompts=1".to_string()]);
    } else {
        args.extend(["-o".to_string(), "BatchMode=yes".to_string()]);
    }
    args.extend([
        "-o".to_string(),
        format!("ConnectTimeout={}", request.connect_timeout_secs),
    ]);
    args.extend(ssh_multiplex_args());
    if let Some(port) = request.ssh_port {
        args.push("-p".to_string());
        args.push(port.to_string());
    }
    args.push(request.ssh_target.clone());
    args.push(request.remote_command.clone());
    args
}

#[cfg(unix)]
fn ssh_multiplex_args() -> Vec<String> {
    let Some(control_path) = ssh_control_path_template() else {
        return Vec::new();
    };
    vec![
        "-o".to_string(),
        "ControlMaster=auto".to_string(),
        "-o".to_string(),
        "ControlPersist=300".to_string(),
        "-o".to_string(),
        format!("ControlPath={control_path}"),
    ]
}

#[cfg(not(unix))]
fn ssh_multiplex_args() -> Vec<String> {
    Vec::new()
}

#[cfg(unix)]
fn ssh_control_path_template() -> Option<String> {
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let dir = PathBuf::from(home).join(".kodex").join("ssh-control");
        if fs::create_dir_all(&dir).is_ok() {
            let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
            let path = dir.join("%C");
            let path = path.to_string_lossy().into_owned();
            if path.len() < 95 {
                return Some(path);
            }
        }
    }

    let user = std::env::var("USER")
        .ok()
        .map(|value| sanitize_control_path_part(&value))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "user".into());
    Some(format!("/tmp/kodex-ssh-{user}-%C"))
}

#[cfg(unix)]
fn sanitize_control_path_part(value: &str) -> String {
    value
        .chars()
        .filter_map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                Some(ch)
            } else {
                None
            }
        })
        .take(32)
        .collect()
}

pub fn first_nonempty<'a>(a: &'a str, b: &'a str) -> Option<&'a str> {
    [a, b]
        .into_iter()
        .map(str::trim)
        .find(|value| !value.is_empty())
}

pub fn sanitize_ssh_diagnostic(value: &str) -> String {
    let trimmed = value.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("permission denied")
        && (lower.contains("publickey") || lower.contains("password"))
    {
        return "SSH 认证失败。需要密码登录时，请填写本次 SSH 密码；也可以配置 SSH key、ssh-agent 或 ~/.ssh/config 后重试。"
            .into();
    }
    if trimmed
        .lines()
        .any(|line| contains_secret_material(line) || line.contains("-----BEGIN "))
    {
        return "Credential details redacted".into();
    }
    const MAX_DIAGNOSTIC_LEN: usize = 600;
    if trimmed.len() <= MAX_DIAGNOSTIC_LEN {
        return trimmed.to_string();
    }
    let mut end = MAX_DIAGNOSTIC_LEN;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &trimmed[..end])
}

fn contains_secret_material(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "password=",
        "password:",
        "passphrase",
        "private key",
        "api_key",
        "apikey",
        "secret=",
        "token=",
        "auth_token",
        "authorization:",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
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

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_args_use_batch_mode_without_password() {
        let request = RemoteSshCommand::new(
            "root@devbox",
            Some(36000),
            "printf ok",
            None,
            Duration::from_secs(1),
        );

        let args = build_ssh_args(&request);

        assert!(args.contains(&"BatchMode=yes".to_string()));
        assert!(!args.contains(&"NumberOfPasswordPrompts=1".to_string()));
        assert!(args.contains(&"-p".to_string()));
        assert!(args.contains(&"36000".to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn ssh_args_enable_connection_multiplexing() {
        let request = RemoteSshCommand::new(
            "root@devbox",
            None,
            "printf ok",
            None,
            Duration::from_secs(1),
        );

        let args = build_ssh_args(&request);

        assert!(args.contains(&"ControlMaster=auto".to_string()));
        assert!(args.contains(&"ControlPersist=300".to_string()));
        assert!(args.iter().any(|arg| arg.starts_with("ControlPath=")));
    }

    #[test]
    fn ssh_args_use_askpass_mode_with_password() {
        let request = RemoteSshCommand::new(
            "root@devbox",
            None,
            "printf ok",
            Some("secret"),
            Duration::from_secs(1),
        );

        let args = build_ssh_args(&request);

        assert!(args.contains(&"NumberOfPasswordPrompts=1".to_string()));
        assert!(!args.contains(&"BatchMode=yes".to_string()));
    }

    #[test]
    fn sanitizer_preserves_password_method_but_redacts_secret_material() {
        assert!(
            sanitize_ssh_diagnostic("Permission denied (publickey,password).")
                .contains("本次 SSH 密码")
        );
        assert_eq!(
            sanitize_ssh_diagnostic("password=secret rejected"),
            "Credential details redacted"
        );
    }

    #[test]
    fn sanitizer_truncates_utf8_diagnostics_on_char_boundary() {
        let diagnostic = "远程错误".repeat(200);
        let sanitized = sanitize_ssh_diagnostic(&diagnostic);

        assert!(sanitized.ends_with("..."));
        assert!(sanitized.len() <= 603);
    }
}
