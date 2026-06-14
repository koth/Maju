use super::process::hide_console_window;
use crate::events::RemoteSshSessionConfig;
use agent_client_protocol::schema::{ReadTextFileRequest, WriteTextFileRequest};
use anyhow::{Context, anyhow};
use serde::Deserialize;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const REMOTE_FS_READ_TIMEOUT: Duration = Duration::from_secs(45);
const REMOTE_FS_WRITE_TIMEOUT: Duration = Duration::from_secs(45);
const REMOTE_SSH_CONNECT_TIMEOUT_SECS: u64 = 5;
const REMOTE_SSH_SERVER_ALIVE_INTERVAL_SECS: u64 = 15;
const REMOTE_SSH_SERVER_ALIVE_COUNT_MAX: u64 = 4;
const KODEX_SSH_ASKPASS_ENV: &str = "KODEX_SSH_ASKPASS";
const KODEX_SSH_ASKPASS_PASSWORD_ENV: &str = "KODEX_SSH_ASKPASS_PASSWORD";

pub(super) fn read_workspace_text_file(
    workspace_root: &str,
    request: &ReadTextFileRequest,
) -> anyhow::Result<String> {
    let path = validate_client_file_path(workspace_root, &request.path)?;

    if path.is_dir() {
        return list_workspace_directory(&path);
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read text file {}", path.display()))?;

    let selected = select_lines(&content, request.line, request.limit);
    Ok(selected)
}

pub(super) fn write_workspace_text_file(
    workspace_root: &str,
    request: &WriteTextFileRequest,
) -> anyhow::Result<()> {
    let path = validate_client_file_path(workspace_root, &request.path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    fs::write(&path, &request.content)
        .with_context(|| format!("failed to write text file {}", path.display()))?;
    Ok(())
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(super) struct RemoteWriteTextFileOutcome {
    pub path: String,
    pub old_text: Option<String>,
}

pub(super) fn read_remote_workspace_text_file(
    config: &RemoteSshSessionConfig,
    request: &ReadTextFileRequest,
) -> anyhow::Result<String> {
    let path = validate_remote_client_file_path(&config.remote_workspace_root, &request.path)?;
    let line = request
        .line
        .map(|line| line.to_string())
        .unwrap_or_default();
    let limit = request
        .limit
        .map(|limit| limit.to_string())
        .unwrap_or_default();
    run_remote_node_command(
        config,
        REMOTE_READ_TEXT_FILE_SCRIPT,
        &[&path, &line, &limit],
        None,
        REMOTE_FS_READ_TIMEOUT,
    )
}

pub(super) fn write_remote_workspace_text_file(
    config: &RemoteSshSessionConfig,
    request: &WriteTextFileRequest,
) -> anyhow::Result<RemoteWriteTextFileOutcome> {
    let path = validate_remote_client_file_path(&config.remote_workspace_root, &request.path)?;
    let stdout = run_remote_node_command(
        config,
        REMOTE_WRITE_TEXT_FILE_SCRIPT,
        &[&path],
        Some(request.content.as_bytes().to_vec()),
        REMOTE_FS_WRITE_TIMEOUT,
    )?;
    serde_json::from_str(&stdout)
        .with_context(|| format!("remote write response is not valid JSON: {}", stdout.trim()))
}

pub(super) fn normalize_path(path: PathBuf) -> PathBuf {
    if path.exists() {
        return path.canonicalize().unwrap_or(path);
    }

    lexical_normalize(path)
}

pub(super) fn paths_are_inside_workspace(workspace_root: &str, paths: &[PathBuf]) -> bool {
    if paths.is_empty() {
        return false;
    }

    let Ok(root) = PathBuf::from(workspace_root).canonicalize() else {
        return false;
    };

    paths.iter().all(|path| {
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        let normalized = lexical_normalize(candidate);
        resolve_for_workspace_check(&normalized)
            .map(|resolved| resolved.starts_with(&root))
            .unwrap_or(false)
    })
}

fn list_workspace_directory(path: &PathBuf) -> anyhow::Result<String> {
    let mut entries = fs::read_dir(path)
        .with_context(|| format!("failed to read directory {}", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("failed to enumerate directory {}", path.display()))?;

    entries.sort_by_key(|entry| entry.file_name().to_string_lossy().to_ascii_lowercase());

    let listing = entries
        .into_iter()
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let suffix = match entry.file_type() {
                Ok(file_type) if file_type.is_dir() => "/",
                _ => "",
            };
            format!("{name}{suffix}")
        })
        .collect::<Vec<_>>()
        .join("\n");

    Ok(listing)
}

pub(super) fn validate_workspace_path(
    workspace_root: &str,
    requested_path: &Path,
) -> anyhow::Result<PathBuf> {
    let workspace_root = PathBuf::from(workspace_root)
        .canonicalize()
        .with_context(|| format!("failed to resolve workspace root {workspace_root}"))?;

    let candidate = if requested_path.is_absolute() {
        requested_path.to_path_buf()
    } else {
        workspace_root.join(requested_path)
    };

    let normalized = lexical_normalize(candidate);
    let resolved = resolve_for_workspace_check(&normalized)?;
    if !resolved.starts_with(&workspace_root) {
        return Err(anyhow!(
            "ACP file request is outside workspace: {}",
            normalized.display()
        ));
    }

    Ok(normalized)
}

pub(super) fn validate_client_file_path(
    workspace_root: &str,
    requested_path: &Path,
) -> anyhow::Result<PathBuf> {
    validate_workspace_path(workspace_root, requested_path)
        .or_else(|_| validate_codebuddy_plan_path(requested_path))
}

pub(super) fn validate_remote_client_file_path(
    remote_workspace_root: &str,
    requested_path: &Path,
) -> anyhow::Result<String> {
    let root = normalize_remote_posix_path(remote_workspace_root)?;
    let requested = requested_path.to_string_lossy().replace('\\', "/");
    let candidate = if requested.starts_with('/') {
        requested
    } else if root == "/" {
        format!("/{requested}")
    } else {
        format!("{}/{}", root.trim_end_matches('/'), requested)
    };
    let normalized = normalize_remote_posix_path(&candidate)?;
    if remote_path_is_inside(&root, &normalized) {
        return Ok(normalized);
    }

    Err(anyhow!(
        "ACP file request is outside remote workspace: {normalized}"
    ))
}

fn validate_codebuddy_plan_path(requested_path: &Path) -> anyhow::Result<PathBuf> {
    if !requested_path.is_absolute() {
        return Err(anyhow!(
            "ACP file request is outside workspace: {}",
            requested_path.display()
        ));
    }

    let normalized = lexical_normalize(requested_path.to_path_buf());
    if !matches!(
        normalized
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase()),
        Some(extension) if matches!(extension.as_str(), "md" | "mdx")
    ) {
        return Err(anyhow!(
            "ACP file request is outside workspace: {}",
            normalized.display()
        ));
    }

    let plan_root = resolve_for_workspace_check(&codebuddy_plan_root()?)?;
    let resolved = resolve_for_workspace_check(&normalized)?;
    if !resolved.starts_with(&plan_root) {
        return Err(anyhow!(
            "ACP file request is outside workspace: {}",
            normalized.display()
        ));
    }

    Ok(normalized)
}

fn codebuddy_plan_root() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| anyhow!("failed to resolve user home directory"))?;
    Ok(PathBuf::from(home).join(".codebuddy").join("plans"))
}

fn resolve_for_workspace_check(path: &Path) -> anyhow::Result<PathBuf> {
    if path.exists() {
        return path
            .canonicalize()
            .with_context(|| format!("failed to resolve path {}", path.display()));
    }

    let mut ancestor = path;
    let mut missing_components = Vec::<OsString>::new();
    while !ancestor.exists() {
        let Some(name) = ancestor.file_name() else {
            return Err(anyhow!("failed to resolve path {}", path.display()));
        };
        missing_components.push(name.to_os_string());
        ancestor = ancestor
            .parent()
            .ok_or_else(|| anyhow!("failed to resolve path {}", path.display()))?;
    }

    let mut resolved = ancestor
        .canonicalize()
        .with_context(|| format!("failed to resolve path {}", ancestor.display()))?;
    for component in missing_components.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn lexical_normalize(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }

    normalized
}

fn select_lines(content: &str, start_line: Option<u32>, limit: Option<u32>) -> String {
    if start_line.is_none() && limit.is_none() {
        return content.to_string();
    }

    let start_index = start_line.unwrap_or(1).saturating_sub(1) as usize;
    let max_lines = limit.unwrap_or(u32::MAX) as usize;

    content
        .lines()
        .skip(start_index)
        .take(max_lines)
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_remote_posix_path(path: &str) -> anyhow::Result<String> {
    let normalized = path.trim().replace('\\', "/");
    if !normalized.starts_with('/') {
        return Err(anyhow!(
            "remote workspace path must be absolute: {normalized}"
        ));
    }

    let mut parts = Vec::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }

    if parts.is_empty() {
        Ok("/".into())
    } else {
        Ok(format!("/{}", parts.join("/")))
    }
}

fn remote_path_is_inside(root: &str, path: &str) -> bool {
    if root == "/" {
        return path.starts_with('/');
    }
    path == root || path.starts_with(&format!("{}/", root.trim_end_matches('/')))
}

fn run_remote_node_command(
    config: &RemoteSshSessionConfig,
    script: &str,
    args: &[&str],
    stdin: Option<Vec<u8>>,
    timeout: Duration,
) -> anyhow::Result<String> {
    let command = remote_node_command(script, &config.remote_workspace_root, args);
    let output = run_remote_ssh_command(config, command, stdin, timeout)?;
    if output.success {
        return Ok(output.stdout);
    }

    Err(anyhow!(remote_command_error(
        "remote file operation failed",
        &output
    )))
}

fn remote_node_command(script: &str, remote_root: &str, args: &[&str]) -> String {
    let mut parts = vec![
        "node".to_string(),
        "-e".to_string(),
        shell_quote(script),
        "--".to_string(),
        shell_quote(remote_root),
    ];
    parts.extend(args.iter().map(|arg| shell_quote(arg)));
    parts.join(" ")
}

fn run_remote_ssh_command(
    config: &RemoteSshSessionConfig,
    remote_command: String,
    stdin: Option<Vec<u8>>,
    timeout: Duration,
) -> anyhow::Result<RemoteCommandOutput> {
    let started = Instant::now();
    let (program, args) = remote_ssh_program_and_args(config, remote_command)?;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(password) = config
        .ssh_password
        .as_deref()
        .filter(|password| !password.is_empty())
    {
        configure_ssh_askpass(&mut command, password)?;
    }
    hide_console_window(&mut command);

    let mut child = command
        .spawn()
        .with_context(|| "failed to start SSH command for remote file operation")?;
    let mut stdin_writer = stdin.and_then(|stdin| {
        child.stdin.take().map(|mut child_stdin| {
            thread::spawn(move || {
                let _ = child_stdin.write_all(&stdin);
            })
        })
    });

    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                if let Some(writer) = stdin_writer.take() {
                    let _ = writer.join();
                }
                let output = child
                    .wait_with_output()
                    .with_context(|| "failed to read SSH command output")?;
                return Ok(RemoteCommandOutput {
                    success: output.status.success(),
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                    timed_out: false,
                });
            }
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                if let Some(writer) = stdin_writer.take() {
                    let _ = writer.join();
                }
                return Ok(RemoteCommandOutput {
                    success: false,
                    stdout: String::new(),
                    stderr: String::new(),
                    timed_out: true,
                });
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(error) => {
                let _ = child.kill();
                if let Some(writer) = stdin_writer.take() {
                    let _ = writer.join();
                }
                return Err(anyhow!("failed to wait for SSH command: {error}"));
            }
        }
    }
}

fn remote_ssh_program_and_args(
    config: &RemoteSshSessionConfig,
    remote_command: String,
) -> anyhow::Result<(PathBuf, Vec<String>)> {
    let ssh_command = config.ssh_command.as_deref().unwrap_or("ssh");
    let mut command_parts =
        shell_words::split(ssh_command).map_err(|err| anyhow!(err.to_string()))?;
    if command_parts.is_empty() {
        command_parts.push("ssh".into());
    }
    let program = PathBuf::from(command_parts.remove(0));
    let mut args = command_parts;
    let has_password = config
        .ssh_password
        .as_deref()
        .is_some_and(|password| !password.is_empty());
    if has_password {
        args.extend(["-o".to_string(), "NumberOfPasswordPrompts=1".to_string()]);
    } else {
        args.extend(["-o".to_string(), "BatchMode=yes".to_string()]);
    }
    args.extend([
        "-o".to_string(),
        format!("ConnectTimeout={REMOTE_SSH_CONNECT_TIMEOUT_SECS}"),
        "-o".to_string(),
        format!("ServerAliveInterval={REMOTE_SSH_SERVER_ALIVE_INTERVAL_SECS}"),
        "-o".to_string(),
        format!("ServerAliveCountMax={REMOTE_SSH_SERVER_ALIVE_COUNT_MAX}"),
    ]);
    args.extend(ssh_multiplex_args());
    if let Some(port) = config.ssh_port {
        args.push("-p".to_string());
        args.push(port.to_string());
    }
    args.push(config.ssh_target.clone());
    args.push(remote_command);
    Ok((program, args))
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

#[derive(Debug)]
struct RemoteCommandOutput {
    success: bool,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

fn remote_command_error(prefix: &str, output: &RemoteCommandOutput) -> String {
    if output.timed_out {
        return format!("{prefix}: SSH command timed out");
    }
    let message = [&output.stderr, &output.stdout]
        .into_iter()
        .map(|value| value.trim())
        .find(|value| !value.is_empty())
        .map(sanitize_remote_diagnostic)
        .unwrap_or_else(|| "SSH command failed without output".into());
    format!("{prefix}: {message}")
}

fn sanitize_remote_diagnostic(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if lower.contains("permission denied")
        && (lower.contains("publickey") || lower.contains("password"))
    {
        return "SSH authentication failed. Configure an SSH key/ssh-agent or provide the SSH password and retry.".into();
    }
    if value
        .lines()
        .any(|line| contains_secret_material(line) || line.contains("-----BEGIN "))
    {
        return "Credential details redacted".into();
    }
    const MAX_DIAGNOSTIC_LEN: usize = 600;
    if value.len() <= MAX_DIAGNOSTIC_LEN {
        value.to_string()
    } else {
        let mut end = MAX_DIAGNOSTIC_LEN;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &value[..end])
    }
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

fn shell_quote(value: &str) -> String {
    shell_words::quote(value).into_owned()
}

const REMOTE_READ_TEXT_FILE_SCRIPT: &str = r#"
const fs = require('fs');
const path = require('path');

function die(message) {
  console.error(message);
  process.exit(1);
}

function ensureInside(root, target) {
  const rel = path.relative(root, target);
  if (rel === '' || (!rel.startsWith('..') && !path.isAbsolute(rel))) return;
  die('path escapes workspace');
}

const root = fs.realpathSync(process.argv[1]);
const requested = process.argv[2] || '';
const lineArg = process.argv[3] || '';
const limitArg = process.argv[4] || '';
const target = path.isAbsolute(requested) ? path.resolve(requested) : path.resolve(root, requested);
if (!fs.existsSync(target)) die(`file does not exist: ${requested}`);
const real = fs.realpathSync(target);
ensureInside(root, real);
const stat = fs.statSync(real);
if (stat.isDirectory()) {
  const entries = fs.readdirSync(real, { withFileTypes: true })
    .map((entry) => `${entry.name}${entry.isDirectory() ? '/' : ''}`);
  entries.sort((a, b) => a.toLowerCase().localeCompare(b.toLowerCase()));
  process.stdout.write(entries.join('\n'));
} else {
  const content = fs.readFileSync(real, 'utf8');
  if (!lineArg && !limitArg) {
    process.stdout.write(content);
  } else {
    const line = Math.max((Number.parseInt(lineArg || '1', 10) || 1) - 1, 0);
    const limit = Number.parseInt(limitArg || '', 10);
    const end = Number.isFinite(limit) && limit > 0 ? line + limit : undefined;
    process.stdout.write(content.split(/\r?\n/).slice(line, end).join('\n'));
  }
}
"#;

const REMOTE_WRITE_TEXT_FILE_SCRIPT: &str = r#"
const fs = require('fs');
const path = require('path');

function die(message) {
  console.error(message);
  process.exit(1);
}

function ensureInside(root, target) {
  const rel = path.relative(root, target);
  if (rel === '' || (!rel.startsWith('..') && !path.isAbsolute(rel))) return;
  die('path escapes workspace');
}

function existingAncestor(target) {
  let current = path.dirname(target);
  while (!fs.existsSync(current)) {
    const parent = path.dirname(current);
    if (parent === current) die('failed to find existing parent');
    current = parent;
  }
  return current;
}

const root = fs.realpathSync(process.argv[1]);
const requested = process.argv[2] || '';
const target = path.isAbsolute(requested) ? path.resolve(requested) : path.resolve(root, requested);
const ancestor = fs.realpathSync(existingAncestor(target));
ensureInside(root, ancestor);

let oldText = null;
if (fs.existsSync(target)) {
  const real = fs.realpathSync(target);
  ensureInside(root, real);
  if (fs.statSync(real).isDirectory()) die('cannot write directory');
  oldText = fs.readFileSync(real, 'utf8');
}

const parent = path.dirname(target);
fs.mkdirSync(parent, { recursive: true });
const realParent = fs.realpathSync(parent);
ensureInside(root, realParent);

const chunks = [];
process.stdin.on('data', (chunk) => chunks.push(chunk));
process.stdin.on('end', () => {
  fs.writeFileSync(target, Buffer.concat(chunks));
  process.stdout.write(JSON.stringify({ path: requested, old_text: oldText }));
});
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn path_permission_check_rejects_empty_path_sets() {
        assert!(!paths_are_inside_workspace("workspace-root", &[]));
    }

    #[test]
    fn path_permission_check_handles_nonexistent_children_inside_workspace() {
        let root = temp_workspace("inside");

        assert!(paths_are_inside_workspace(
            root.to_str().unwrap(),
            &[PathBuf::from("new/nested/file.txt")]
        ));

        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn path_permission_check_rejects_parent_escape() {
        let root = temp_workspace("escape");

        assert!(!paths_are_inside_workspace(
            root.to_str().unwrap(),
            &[PathBuf::from("../outside.txt")]
        ));

        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn client_file_path_allows_codebuddy_plan_markdown() {
        let root = temp_workspace("codebuddy-plan");
        let plan = codebuddy_plan_root().unwrap().join("draft.md");

        assert_eq!(
            validate_client_file_path(root.to_str().unwrap(), &plan).unwrap(),
            lexical_normalize(plan),
        );

        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn client_file_path_rejects_non_plan_home_file() {
        let root = temp_workspace("codebuddy-non-plan");
        let path = codebuddy_plan_root()
            .unwrap()
            .parent()
            .unwrap()
            .join("secret.md");

        assert!(validate_client_file_path(root.to_str().unwrap(), &path).is_err());

        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn client_file_path_rejects_non_markdown_codebuddy_plan_file() {
        let root = temp_workspace("codebuddy-non-markdown");
        let path = codebuddy_plan_root().unwrap().join("draft.txt");

        assert!(validate_client_file_path(root.to_str().unwrap(), &path).is_err());

        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn select_lines_applies_limit_from_first_line_when_start_missing() {
        assert_eq!(
            select_lines("one\ntwo\nthree", None, Some(2)),
            "one\ntwo".to_string()
        );
    }

    #[test]
    fn remote_client_file_path_allows_absolute_workspace_path() {
        assert_eq!(
            validate_remote_client_file_path("/g/kothbot/", Path::new("/g/kothbot/tmp/report.txt"))
                .unwrap(),
            "/g/kothbot/tmp/report.txt"
        );
    }

    #[test]
    fn remote_client_file_path_allows_relative_workspace_path() {
        assert_eq!(
            validate_remote_client_file_path("/g/kothbot", Path::new("tmp/report.txt")).unwrap(),
            "/g/kothbot/tmp/report.txt"
        );
    }

    #[test]
    fn remote_client_file_path_rejects_absolute_escape() {
        assert!(validate_remote_client_file_path("/g/kothbot", Path::new("/etc/passwd")).is_err());
    }

    #[test]
    fn remote_client_file_path_rejects_relative_escape() {
        assert!(validate_remote_client_file_path("/g/kothbot", Path::new("../secret")).is_err());
    }

    #[test]
    fn remote_ssh_program_preserves_custom_command_args() {
        let config = RemoteSshSessionConfig {
            ssh_target: "devbox".into(),
            ssh_port: Some(2200),
            remote_workspace_root: "/g/kothbot".into(),
            local_port: 0,
            remote_port: 0,
            reverse_forwards: Vec::new(),
            ssh_command: Some("ssh -F ~/.ssh/config".into()),
            ssh_password: None,
        };

        let (program, args) = remote_ssh_program_and_args(&config, "printf ok".into()).unwrap();

        assert_eq!(program, PathBuf::from("ssh"));
        assert!(args.starts_with(&["-F".to_string(), "~/.ssh/config".to_string()]));
        assert!(args.contains(&"BatchMode=yes".to_string()));
        assert!(args.contains(&"ConnectTimeout=5".to_string()));
        #[cfg(unix)]
        {
            assert!(args.contains(&"ControlMaster=auto".to_string()));
            assert!(args.contains(&"ControlPersist=300".to_string()));
            assert!(args.iter().any(|arg| arg.starts_with("ControlPath=")));
        }
        assert!(args.contains(&"2200".to_string()));
        assert_eq!(args.last().unwrap(), "printf ok");
    }

    #[cfg(unix)]
    #[test]
    fn path_permission_check_rejects_symlink_parent_escape() {
        let root = temp_workspace("symlink");
        let outside = root.parent().unwrap().join("outside");
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("linked-out")).unwrap();

        assert!(!paths_are_inside_workspace(
            root.to_str().unwrap(),
            &[PathBuf::from("linked-out/file.txt")]
        ));

        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    fn temp_workspace(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir()
            .join(format!("kodex-acp-paths-{label}-{unique}"))
            .join("workspace");
        fs::create_dir_all(&root).unwrap();
        root
    }
}
