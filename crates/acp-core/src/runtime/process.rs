use super::workspace_paths::normalize_path;
use agent_client_protocol::schema::CreateTerminalRequest;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

pub(super) fn build_terminal_command(request: &CreateTerminalRequest) -> Command {
    if request.args.is_empty() {
        return build_shell_command(&request.command);
    }

    let mut command = Command::new(&request.command);
    command.args(&request.args);
    hide_console_window(&mut command);
    command
}

pub(super) fn build_shell_command(command_text: &str) -> Command {
    #[cfg(windows)]
    {
        if is_probably_powershell_command(command_text) {
            let mut command = Command::new("powershell.exe");
            command.args(["-NoProfile", "-Command", command_text]);
            hide_console_window(&mut command);
            return command;
        }

        if let Some(git_bash) = find_git_bash() {
            let mut command = Command::new(git_bash);
            command.args(["-lc", command_text]);
            hide_console_window(&mut command);
            return command;
        }

        let mut command = Command::new("bash.exe");
        command.args(["-lc", command_text]);
        hide_console_window(&mut command);
        return command;
    }

    #[cfg(not(windows))]
    {
        let mut command = Command::new("sh");
        command.args(["-lc", command_text]);
        command
    }
}

pub(super) fn process_cwd(workspace_root: &str, requested_cwd: Option<&Path>) -> PathBuf {
    let workspace_root = PathBuf::from(workspace_root);
    let cwd = match requested_cwd {
        Some(cwd) if cwd.is_absolute() => cwd.to_path_buf(),
        Some(cwd) => workspace_root.join(cwd),
        None => workspace_root,
    };
    normalize_path(cwd)
}

pub(super) fn apply_process_cwd_and_pwd(command: &mut Command, cwd: &Path) {
    command.current_dir(cwd);
    command.env("PWD", cwd.as_os_str());
}

#[cfg(windows)]
pub(super) fn hide_console_window(command: &mut Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub(super) fn hide_console_window(_command: &mut Command) {}

pub(super) fn agent_spawn_command(command_path: &Path, args: &[String]) -> Command {
    #[cfg(windows)]
    {
        let mut command = if is_windows_batch_script(command_path) {
            let mut wrapper = Command::new("cmd.exe");
            wrapper.arg("/C").arg(command_path);
            wrapper
        } else {
            Command::new(command_path)
        };
        command.args(args);
        command
    }

    #[cfg(not(windows))]
    {
        // On Unix, if the command is a script (e.g. node script with shebang),
        // std::process::Command won't interpret the shebang. Use /bin/sh -c
        // to ensure scripts are properly executed.
        if is_script_file(command_path) {
            let mut command = Command::new("/bin/sh");
            let mut cmd_str = command_path.to_string_lossy().to_string();
            for arg in args {
                cmd_str.push(' ');
                cmd_str.push_str(&shell_words::quote(arg));
            }
            command.arg("-c").arg(cmd_str);
            command
        } else {
            let mut command = Command::new(command_path);
            command.args(args);
            command
        }
    }
}

#[cfg(windows)]
pub(super) fn is_windows_batch_script(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        })
}

#[cfg(not(windows))]
pub(super) fn is_script_file(path: &Path) -> bool {
    use std::io::Read;

    if let Ok(mut file) = std::fs::File::open(path) {
        let mut buf = [0u8; 2];
        if file.read_exact(&mut buf).is_ok() {
            // Check for shebang (#!)
            return buf == [0x23, 0x21];
        }
    }
    false
}

pub(super) fn parse_env_assignment(value: &str) -> Option<(String, String)> {
    let (name, value) = value.split_once('=')?;
    if name.is_empty() {
        return None;
    }
    let mut chars = name.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() && first != '_' {
        return None;
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return None;
    }
    Some((name.to_string(), value.to_string()))
}

#[cfg(windows)]
pub(super) fn is_probably_powershell_command(command_text: &str) -> bool {
    let lower = command_text.to_ascii_lowercase();
    [
        "get-childitem",
        "select-object",
        "where-object",
        "foreach-object",
        "set-content",
        "get-content",
        "out-file",
        "new-item",
        "remove-item",
        "copy-item",
        "move-item",
        "$env:",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

#[cfg(windows)]
pub(super) fn find_git_bash() -> Option<PathBuf> {
    let git_path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|entry| {
            let git_cmd = entry.join("git.exe");
            git_cmd.exists().then_some(git_cmd)
        })
    })?;

    let root = git_path.parent()?.parent()?;
    let candidates = [
        root.join("bin").join("bash.exe"),
        root.join("usr").join("bin").join("bash.exe"),
    ];
    candidates.into_iter().find(|candidate| candidate.exists())
}
