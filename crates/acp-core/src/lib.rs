mod client;
mod events;
mod mapping;
mod runtime;

pub use client::{PromptTask, SessionHandle};
pub use events::{ClientEvent, SessionConfig};
pub use mapping::diff_to_hunks;

pub const DEFAULT_AGENT_COMMAND: &str = "codebuddy --acp";

pub fn platform_default_agent_command() -> String {
    if cfg!(windows) {
        if let Some(path) = find_windows_agent_binary("codebuddy") {
            return format!("{} --acp", shell_words::quote(&path.to_string_lossy()));
        }

        "codebuddy.exe --acp".to_string()
    } else {
        DEFAULT_AGENT_COMMAND.to_string()
    }
}

pub fn resolve_agent_command() -> String {
    std::env::var("ACP_AGENT_COMMAND").unwrap_or_else(|_| platform_default_agent_command())
}

#[cfg(windows)]
fn find_windows_agent_binary(binary: &str) -> Option<std::path::PathBuf> {
    let paths = std::env::var_os("PATH")?;
    let names = [
        format!("{binary}.exe"),
        format!("{binary}.cmd"),
        format!("{binary}.bat"),
    ];
    std::env::split_paths(&paths)
        .flat_map(|path| names.iter().map(move |name| path.join(name)))
        .find(|path| path.is_file())
}
