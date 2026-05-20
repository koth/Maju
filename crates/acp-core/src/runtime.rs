use crate::codex_api_proxy::ensure_codex_api_proxy;
use crate::events::{ClientEvent, SessionConfig};
use crate::mapping::{
    append_notification_log, append_runtime_event_log, append_typed_notification_log,
    emit_notification, format_permission_options, format_stop_reason, session_config_from_options,
    session_config_from_parts,
};
use agent_client_protocol::schema::{
    AgentCapabilities, BlobResourceContents, CancelNotification, ClientCapabilities, ClientRequest,
    ContentBlock, CreateTerminalRequest, CreateTerminalResponse, EmbeddedResource,
    EmbeddedResourceResource, ExtRequest, FileSystemCapabilities, ImageContent, Implementation,
    InitializeRequest, KillTerminalRequest, KillTerminalResponse, ListSessionsRequest,
    LoadSessionRequest, NewSessionRequest, NewSessionResponse, PermissionOptionKind, PromptRequest,
    PromptResponse, ProtocolVersion, ReadTextFileRequest, ReadTextFileResponse,
    ReleaseTerminalRequest, ReleaseTerminalResponse, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome, SessionId,
    SessionNotification, SessionUpdate, SetSessionConfigOptionRequest, SetSessionModeRequest,
    SetSessionModelRequest, StopReason, TerminalExitStatus, TerminalId, TerminalOutputRequest,
    TerminalOutputResponse, TextContent, TextResourceContents, ToolKind,
    WaitForTerminalExitRequest, WaitForTerminalExitResponse, WriteTextFileRequest,
    WriteTextFileResponse,
};
use agent_client_protocol::{Agent, Client, ConnectTo, ConnectionTo, Dispatch, Lines, Role};
use anyhow::{Context, anyhow};
use futures::channel::mpsc as futures_mpsc;
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::thread;
use workspace_model::{PermissionOption, PromptInputCapabilities, UserPromptContent};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;
const TITLE_SYNC_RETRY_DELAYS_MS: [u64; 3] = [120, 400, 900];
const TITLE_SYNC_TIMEOUT_MS: u64 = 2_000;

#[derive(Clone, Debug, Default)]
pub(crate) struct PermissionBroker {
    state: Arc<Mutex<PermissionBrokerState>>,
    mode: Arc<Mutex<PermissionPolicyMode>>,
}

#[derive(Debug, Default)]
struct PermissionBrokerState {
    pending: HashMap<String, mpsc::Sender<Option<String>>>,
    early_resolutions: HashMap<String, Option<String>>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ShutdownSignal {
    inner: Arc<ShutdownState>,
}

#[derive(Debug, Default)]
struct ShutdownState {
    requested: AtomicBool,
    agent_children: Mutex<Vec<Weak<Mutex<Option<Child>>>>>,
}

impl ShutdownSignal {
    pub(crate) fn request_shutdown(&self) {
        self.inner.requested.store(true, Ordering::Release);
        self.kill_registered_agent_children();
    }

    fn is_requested(&self) -> bool {
        self.inner.requested.load(Ordering::Acquire)
    }

    fn register_agent_child(&self, child: &Arc<Mutex<Option<Child>>>) {
        let Ok(mut guard) = self.inner.agent_children.lock() else {
            return;
        };
        guard.retain(|entry| entry.strong_count() > 0);
        guard.push(Arc::downgrade(child));
    }

    fn kill_registered_agent_children(&self) {
        let children = {
            let Ok(mut guard) = self.inner.agent_children.lock() else {
                return;
            };
            let children = guard.iter().filter_map(Weak::upgrade).collect::<Vec<_>>();
            guard.retain(|entry| entry.strong_count() > 0);
            children
        };

        for child in children {
            let _ = kill_child_handle(&child);
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PermissionPolicyMode {
    Plan,
    #[default]
    Build,
}

impl PermissionBroker {
    pub(crate) fn register(
        &self,
        request_id: String,
    ) -> anyhow::Result<mpsc::Receiver<Option<String>>> {
        let (tx, rx) = mpsc::channel();

        let early_resolution = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("permission broker lock poisoned"))?;
            if let Some(option_id) = state.early_resolutions.remove(&request_id) {
                Some(option_id)
            } else {
                state.pending.insert(request_id, tx.clone());
                None
            }
        };

        if let Some(option_id) = early_resolution {
            tx.send(option_id)
                .map_err(|_| anyhow!("permission request already closed"))?;
        }

        Ok(rx)
    }

    pub(crate) fn resolve(
        &self,
        request_id: &str,
        option_id: Option<String>,
    ) -> anyhow::Result<bool> {
        let sender = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("permission broker lock poisoned"))?;
            if let Some(sender) = state.pending.remove(request_id) {
                Some(sender)
            } else {
                state
                    .early_resolutions
                    .insert(request_id.to_string(), option_id.clone());
                None
            }
        };

        let Some(sender) = sender else {
            return Ok(false);
        };

        sender
            .send(option_id)
            .map_err(|_| anyhow!("permission request already closed"))?;
        Ok(true)
    }

    pub(crate) fn clear_early_resolution(&self, request_id: &str) -> anyhow::Result<()> {
        self.state
            .lock()
            .map_err(|_| anyhow!("permission broker lock poisoned"))?
            .early_resolutions
            .remove(request_id);
        Ok(())
    }

    pub(crate) fn set_mode(&self, mode_id: &str) -> anyhow::Result<()> {
        let mode = if mode_id.eq_ignore_ascii_case("build") {
            PermissionPolicyMode::Build
        } else {
            PermissionPolicyMode::Plan
        };
        *self
            .mode
            .lock()
            .map_err(|_| anyhow!("permission broker lock poisoned"))? = mode;
        Ok(())
    }

    fn mode(&self) -> PermissionPolicyMode {
        self.mode.lock().map(|mode| *mode).unwrap_or_default()
    }

    pub(crate) fn cancel_all(&self) -> anyhow::Result<()> {
        let pending = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("permission broker lock poisoned"))?;
            state.early_resolutions.clear();
            std::mem::take(&mut state.pending)
        };
        for (_, sender) in pending {
            let _ = sender.send(None);
        }
        Ok(())
    }
}

enum PermissionDecision {
    Select(String),
    Cancel,
    Ask,
}

fn decide_permission(
    mode: PermissionPolicyMode,
    workspace_root: &str,
    request: &RequestPermissionRequest,
) -> PermissionDecision {
    match mode {
        PermissionPolicyMode::Plan => decide_plan_permission(workspace_root, request),
        PermissionPolicyMode::Build => decide_build_permission(workspace_root, request),
    }
}

fn decide_plan_permission(
    workspace_root: &str,
    request: &RequestPermissionRequest,
) -> PermissionDecision {
    match request.tool_call.fields.kind.unwrap_or(ToolKind::Other) {
        ToolKind::Read | ToolKind::Search => {
            if paths_are_inside_workspace(workspace_root, &permission_paths(request)) {
                select_permission_option(request, true)
            } else {
                select_permission_option(request, false)
            }
        }
        ToolKind::Edit => {
            let paths = permission_paths(request);
            if !paths.is_empty()
                && paths_are_inside_workspace(workspace_root, &paths)
                && paths.iter().all(is_markdown_path)
            {
                select_permission_option(request, true)
            } else {
                select_permission_option(request, false)
            }
        }
        ToolKind::Execute | ToolKind::Delete | ToolKind::Move => {
            select_permission_option(request, false)
        }
        _ => select_permission_option(request, false),
    }
}

fn decide_build_permission(
    workspace_root: &str,
    request: &RequestPermissionRequest,
) -> PermissionDecision {
    match request.tool_call.fields.kind.unwrap_or(ToolKind::Other) {
        ToolKind::Read | ToolKind::Edit | ToolKind::Delete | ToolKind::Move => {
            let paths = permission_paths(request);
            if paths_are_inside_workspace(workspace_root, &paths) {
                select_permission_option(request, true)
            } else {
                PermissionDecision::Ask
            }
        }
        _ => select_permission_option(request, true),
    }
}

fn select_permission_option(request: &RequestPermissionRequest, allow: bool) -> PermissionDecision {
    let option = request.options.iter().find(|option| {
        matches!(
            (allow, option.kind),
            (
                true,
                PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways
            ) | (
                false,
                PermissionOptionKind::RejectOnce | PermissionOptionKind::RejectAlways
            )
        )
    });

    option
        .map(|option| PermissionDecision::Select(option.option_id.0.to_string()))
        .unwrap_or(PermissionDecision::Cancel)
}

fn permission_paths(request: &RequestPermissionRequest) -> Vec<PathBuf> {
    let mut paths = request
        .tool_call
        .fields
        .locations
        .as_ref()
        .map(|locations| {
            locations
                .iter()
                .map(|location| location.path.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if let Some(raw_input) = &request.tool_call.fields.raw_input {
        collect_path_like_values(raw_input, &mut paths);
    }

    paths
}

fn collect_path_like_values(value: &serde_json::Value, paths: &mut Vec<PathBuf>) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                let key = key.to_ascii_lowercase();
                if key.contains("path") || key == "file" || key == "cwd" || key.ends_with("file") {
                    if let Some(path) = value.as_str() {
                        paths.push(PathBuf::from(path));
                        continue;
                    }
                }
                collect_path_like_values(value, paths);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_path_like_values(item, paths);
            }
        }
        _ => {}
    }
}

fn paths_are_inside_workspace(workspace_root: &str, paths: &[PathBuf]) -> bool {
    let root = normalize_path(PathBuf::from(workspace_root));
    paths.iter().all(|path| {
        let candidate = if path.is_absolute() {
            normalize_path(path.clone())
        } else {
            normalize_path(root.join(path))
        };
        candidate.starts_with(&root)
    })
}

fn is_markdown_path(path: &PathBuf) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| matches!(extension.to_ascii_lowercase().as_str(), "md" | "mdx"))
        .unwrap_or(false)
}

pub(crate) enum RuntimeCommand {
    SendPrompt(Vec<UserPromptContent>),
    SetConfigOption {
        config_id: String,
        value_id: String,
        reply_tx: mpsc::Sender<anyhow::Result<Vec<ClientEvent>>>,
    },
    SetMode {
        mode_id: String,
        reply_tx: mpsc::Sender<anyhow::Result<Vec<ClientEvent>>>,
    },
    SetModel {
        model_id: String,
        reply_tx: mpsc::Sender<anyhow::Result<Vec<ClientEvent>>>,
    },
    ResolveCodeBuddyInterruption {
        session_id: String,
        tool_call_id: String,
        decision: String,
        reply_tx: mpsc::Sender<anyhow::Result<()>>,
    },
    CancelPrompt {
        reply_tx: mpsc::Sender<anyhow::Result<()>>,
    },
    Shutdown,
}

fn prompt_content_to_acp(content: UserPromptContent) -> Option<ContentBlock> {
    match content {
        UserPromptContent::Text { text } => {
            let text = text.trim().to_string();
            if text.is_empty() {
                None
            } else {
                Some(ContentBlock::Text(TextContent::new(text)))
            }
        }
        UserPromptContent::Image {
            data, mime_type, ..
        } => Some(ContentBlock::Image(ImageContent::new(data, mime_type))),
        UserPromptContent::File {
            data,
            text,
            mime_type,
            name,
            uri,
        } => {
            let uri = uri.unwrap_or_else(|| attachment_uri(&name));
            if let Some(text) = text {
                Some(ContentBlock::Resource(EmbeddedResource::new(
                    EmbeddedResourceResource::TextResourceContents(
                        TextResourceContents::new(text, uri).mime_type(mime_type),
                    ),
                )))
            } else if let Some(data) = data {
                Some(ContentBlock::Resource(EmbeddedResource::new(
                    EmbeddedResourceResource::BlobResourceContents(
                        BlobResourceContents::new(data, uri).mime_type(mime_type),
                    ),
                )))
            } else {
                None
            }
        }
    }
}

fn attachment_uri(name: &str) -> String {
    let safe_name = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let safe_name = safe_name.trim_matches('_');
    format!(
        "attachment://{}",
        if safe_name.is_empty() {
            "file"
        } else {
            safe_name
        }
    )
}

fn prompt_contains_image(prompt: &[UserPromptContent]) -> bool {
    prompt
        .iter()
        .any(|content| matches!(content, UserPromptContent::Image { .. }))
}

fn prompt_contains_file(prompt: &[UserPromptContent]) -> bool {
    prompt
        .iter()
        .any(|content| matches!(content, UserPromptContent::File { .. }))
}

fn prompt_capabilities_from_acp(
    capabilities: &agent_client_protocol::schema::PromptCapabilities,
) -> PromptInputCapabilities {
    PromptInputCapabilities {
        image: capabilities.image,
        embedded_context: capabilities.embedded_context,
    }
}

#[derive(Default)]
struct TerminalManager {
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
    fn create_terminal(
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

    fn terminal_output(
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

    fn wait_for_terminal_exit(
        &self,
        request: &WaitForTerminalExitRequest,
    ) -> anyhow::Result<WaitForTerminalExitResponse> {
        let terminal = self.get_terminal(request.terminal_id.0.as_ref())?;
        let exit_status = terminal.wait_for_exit()?;
        Ok(WaitForTerminalExitResponse::new(exit_status))
    }

    fn kill_terminal(&self, request: &KillTerminalRequest) -> anyhow::Result<KillTerminalResponse> {
        let terminal = self.get_terminal(request.terminal_id.0.as_ref())?;
        terminal.kill()?;
        Ok(KillTerminalResponse::new())
    }

    fn release_terminal(
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

fn build_terminal_command(request: &CreateTerminalRequest) -> Command {
    if request.args.is_empty() {
        return build_shell_command(&request.command);
    }

    let mut command = Command::new(&request.command);
    command.args(&request.args);
    hide_console_window(&mut command);
    command
}

fn build_shell_command(command_text: &str) -> Command {
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

fn process_cwd(workspace_root: &str, requested_cwd: Option<&Path>) -> PathBuf {
    let workspace_root = PathBuf::from(workspace_root);
    let cwd = match requested_cwd {
        Some(cwd) if cwd.is_absolute() => cwd.to_path_buf(),
        Some(cwd) => workspace_root.join(cwd),
        None => workspace_root,
    };
    normalize_path(cwd)
}

fn apply_process_cwd_and_pwd(command: &mut Command, cwd: &Path) {
    command.current_dir(cwd);
    command.env("PWD", cwd.as_os_str());
}

#[cfg(windows)]
fn hide_console_window(command: &mut Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console_window(_command: &mut Command) {}

fn agent_spawn_command(command_path: &Path, args: &[String]) -> Command {
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
fn is_windows_batch_script(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        })
}

#[cfg(not(windows))]
fn is_script_file(path: &Path) -> bool {
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

struct HiddenAgentProcess {
    command: PathBuf,
    args: Vec<String>,
    env: Vec<(String, String)>,
    current_dir: PathBuf,
    log_config: Option<SessionConfig>,
    shutdown_signal: ShutdownSignal,
}

impl HiddenAgentProcess {
    fn from_config(config: &SessionConfig) -> anyhow::Result<Self> {
        let mut process = Self::from_command(&config.agent_command, &config.workspace_root)?;
        process.env.extend(config.agent_env.clone());
        if config
            .agent_command
            .to_ascii_lowercase()
            .contains("codex-acp")
        {
            if let Some((_, api_key)) = process
                .env
                .iter()
                .find(|(name, _)| name == "DEEPSEEK_API_KEY")
            {
                ensure_codex_api_proxy("deepseek", api_key);
            } else if let Some((_, api_key)) =
                process.env.iter().find(|(name, _)| name == "VENUS_API_KEY")
            {
                ensure_codex_api_proxy("venus", api_key);
            }
        }
        process.log_config = Some(config.clone());
        Ok(process)
    }

    fn from_command(command: &str, current_dir: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let args = shell_words::split(command).map_err(|err| anyhow!(err.to_string()))?;
        if args.is_empty() {
            return Err(anyhow!("agent command cannot be empty"));
        }

        let mut env = Vec::new();
        let mut command_index = 0;
        for (index, arg) in args.iter().enumerate() {
            if let Some((name, value)) = parse_env_assignment(arg) {
                env.push((name, value));
                command_index = index + 1;
            } else {
                break;
            }
        }

        let Some(command) = args.get(command_index) else {
            return Err(anyhow!("agent command missing executable"));
        };

        Ok(Self {
            command: PathBuf::from(command),
            args: args[command_index + 1..].to_vec(),
            env,
            current_dir: current_dir.into(),
            log_config: None,
            shutdown_signal: ShutdownSignal::default(),
        })
    }

    fn shutdown_signal(mut self, shutdown_signal: ShutdownSignal) -> Self {
        self.shutdown_signal = shutdown_signal;
        self
    }
}

impl ConnectTo<Client> for HiddenAgentProcess {
    async fn connect_to(
        self,
        client: impl ConnectTo<<Client as Role>::Counterpart>,
    ) -> agent_client_protocol::Result<()> {
        let mut command = agent_spawn_command(&self.command, &self.args);
        apply_process_cwd_and_pwd(&mut command, &self.current_dir);
        for (name, value) in &self.env {
            command.env(name, value);
        }
        command.env("PWD", self.current_dir.as_os_str());
        // Ensure PATH includes common directories for CLI tools (e.g. node, python)
        // GUI apps on macOS may not inherit the shell's PATH.
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = std::env::split_paths(&path).collect::<Vec<_>>();
        for extra in ["/usr/local/bin", "/opt/homebrew/bin", "/usr/bin", "/bin"] {
            let p = std::path::PathBuf::from(extra);
            if !paths.contains(&p) {
                paths.push(p);
            }
        }
        let new_path = std::env::join_paths(paths).unwrap_or_default();
        command.env("PATH", new_path);
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        hide_console_window(&mut command);

        let mut child = command
            .spawn()
            .map_err(agent_client_protocol::Error::into_internal_error)?;

        let child_stdin = child.stdin.take().ok_or_else(|| {
            agent_client_protocol::util::internal_error("failed to open agent stdin")
        })?;
        let child_stdout = child.stdout.take().ok_or_else(|| {
            agent_client_protocol::util::internal_error("failed to open agent stdout")
        })?;
        let child_stderr = child.stderr.take().ok_or_else(|| {
            agent_client_protocol::util::internal_error("failed to open agent stderr")
        })?;

        let (stdout_tx, incoming_lines) = futures_mpsc::unbounded::<std::io::Result<String>>();
        thread::spawn(move || read_agent_stdout(child_stdout, stdout_tx));

        let (stderr_tx, stderr_rx) = std::sync::mpsc::channel::<String>();
        thread::spawn(move || read_agent_stderr(child_stderr, stderr_tx));

        let (stdin_tx, stdin_rx) = mpsc::channel::<String>();

        // Some ACP agents shut down their ACP handler when
        // stdin reaches EOF.  We must keep the stdin handle alive for the entire
        // lifetime of the child process — not just until the write channel closes
        // or connect_to returns.  Spawn a dedicated thread that holds the handle
        // and only drops it when the child exits (detected via child_monitor).
        let shared_stdin: Arc<Mutex<Option<std::process::ChildStdin>>> =
            Arc::new(Mutex::new(Some(child_stdin)));
        let keepalive_stdin = shared_stdin.clone();
        // Use a channel so we can signal the keepalive thread to exit.
        let (stdin_keepalive_tx, stdin_keepalive_rx) = mpsc::channel::<()>();
        thread::spawn(move || {
            // Block until signaled or the sender is dropped (process exit).
            let _ = stdin_keepalive_rx.recv();
            if let Ok(mut guard) = keepalive_stdin.lock() {
                guard.take();
            }
        });
        thread::spawn(move || write_agent_stdin(shared_stdin, stdin_rx));

        let log_config = self.log_config.clone();
        let child_guard = AgentChildGuard::new(child, self.shutdown_signal.clone());
        let child_monitor =
            monitor_hidden_agent_child(child_guard.handle(), stderr_rx, log_config.clone());
        let outgoing_sink = futures::sink::unfold(stdin_tx, |tx, line: String| async move {
            tx.send(line).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "agent stdin closed")
            })?;
            Ok::<_, std::io::Error>(tx)
        });

        let protocol = agent_client_protocol::ConnectTo::<Client>::connect_to(
            Lines::new(outgoing_sink, incoming_lines),
            client,
        );

        // Hold stdin_keepalive_tx for the entire duration of the select! so the
        // keepalive thread doesn't drop the stdin handle prematurely.
        let _keepalive = stdin_keepalive_tx;
        let _child_guard = child_guard;

        tokio::pin!(protocol);
        tokio::pin!(child_monitor);
        tokio::select! {
            result = &mut protocol => result,
            result = &mut child_monitor => match result {
                Ok(()) => {
                    if let Some(config) = log_config.as_ref() {
                        let _ = append_runtime_event_log(
                            config,
                            "agent/clean_exit_waiting_for_protocol",
                            &json!({ "reason": "agent process exited before protocol completed" }),
                        );
                    }
                    (&mut protocol).await
                }
                Err(error) => Err(error),
            },
        }
    }
}

/// ACP transport that spawns the agent process and connects via TCP.
///
/// Used for agents that listen on a TCP port instead of communicating over
/// stdio. The agent is spawned with `--port <port>` and the client connects to
/// `127.0.0.1:<port>`.
struct TcpAgentProcess {
    command: PathBuf,
    args: Vec<String>,
    env: Vec<(String, String)>,
    current_dir: PathBuf,
    port: u16,
    log_config: SessionConfig,
    shutdown_signal: ShutdownSignal,
}

impl TcpAgentProcess {
    fn from_config(config: &SessionConfig) -> anyhow::Result<Self> {
        let parsed =
            HiddenAgentProcess::from_command(&config.agent_command, &config.workspace_root)?;
        Ok(Self {
            command: parsed.command,
            args: parsed.args,
            env: parsed.env,
            current_dir: parsed.current_dir,
            port: config.acp_port,
            log_config: config.clone(),
            shutdown_signal: ShutdownSignal::default(),
        })
    }

    fn shutdown_signal(mut self, shutdown_signal: ShutdownSignal) -> Self {
        self.shutdown_signal = shutdown_signal;
        self
    }
}

/// Wrapper that dispatches to either stdio or TCP transport.
enum AgentTransport {
    Stdio(HiddenAgentProcess),
    Tcp(TcpAgentProcess),
}

impl ConnectTo<Client> for AgentTransport {
    async fn connect_to(
        self,
        client: impl ConnectTo<<Client as Role>::Counterpart>,
    ) -> agent_client_protocol::Result<()> {
        match self {
            AgentTransport::Stdio(agent) => agent.connect_to(client).await,
            AgentTransport::Tcp(agent) => agent.connect_to(client).await,
        }
    }
}

impl ConnectTo<Client> for TcpAgentProcess {
    async fn connect_to(
        self,
        client: impl ConnectTo<<Client as Role>::Counterpart>,
    ) -> agent_client_protocol::Result<()> {
        // Build the command, appending --port <port>
        let mut command = agent_spawn_command(&self.command, &self.args);
        command.args(["--port", &self.port.to_string()]);
        apply_process_cwd_and_pwd(&mut command, &self.current_dir);
        for (name, value) in &self.env {
            command.env(name, value);
        }
        command.env("PWD", self.current_dir.as_os_str());
        // Ensure PATH includes common directories
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = std::env::split_paths(&path).collect::<Vec<_>>();
        for extra in ["/usr/local/bin", "/opt/homebrew/bin", "/usr/bin", "/bin"] {
            let p = std::path::PathBuf::from(extra);
            if !paths.contains(&p) {
                paths.push(p);
            }
        }
        let new_path = std::env::join_paths(paths).unwrap_or_default();
        command.env("PATH", new_path);
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        hide_console_window(&mut command);

        let mut child = command
            .spawn()
            .map_err(agent_client_protocol::Error::into_internal_error)?;

        // Keep stdin handle alive for the entire session. Some TCP ACP agents exit
        // when stdin reaches EOF, regardless of whether it uses stdio or TCP.
        let _child_stdin = child.stdin.take().ok_or_else(|| {
            agent_client_protocol::util::internal_error("failed to open agent stdin")
        })?;

        let child_stderr = child.stderr.take().ok_or_else(|| {
            agent_client_protocol::util::internal_error("failed to open agent stderr")
        })?;

        let (stderr_tx, stderr_rx) = std::sync::mpsc::channel::<String>();
        thread::spawn(move || read_agent_stderr(child_stderr, stderr_tx));

        let log_config = self.log_config.clone();
        let child_guard = AgentChildGuard::new(child, self.shutdown_signal.clone());
        let child_monitor =
            monitor_hidden_agent_child(child_guard.handle(), stderr_rx, Some(log_config.clone()));

        // Wait for the agent to start listening on the port.
        let addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", self.port).parse().map_err(|e| {
                agent_client_protocol::util::internal_error(format!("invalid address: {e}"))
            })?;

        let stream = {
            let mut last_err = None;
            let mut connected = None;
            for attempt in 0..50 {
                match std::net::TcpStream::connect_timeout(
                    &addr,
                    std::time::Duration::from_millis(200),
                ) {
                    Ok(s) => {
                        connected = Some(s);
                        break;
                    }
                    Err(e) => {
                        last_err = Some(e);
                        if attempt < 49 {
                            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        }
                    }
                }
            }
            connected.ok_or_else(|| {
                agent_client_protocol::util::internal_error(format!(
                    "failed to connect to agent at 127.0.0.1:{}: {}",
                    self.port,
                    last_err.map(|e| e.to_string()).unwrap_or_default()
                ))
            })?
        };

        stream.set_nonblocking(true).map_err(|e| {
            agent_client_protocol::util::internal_error(format!("set_nonblocking: {e}"))
        })?;

        let tcp_stream = tokio::net::TcpStream::from_std(stream).map_err(|e| {
            agent_client_protocol::util::internal_error(format!("TcpStream::from_std: {e}"))
        })?;

        let (read_half, write_half) = tcp_stream.into_split();
        let reader = tokio::io::BufReader::new(read_half);
        let incoming_lines = futures::stream::unfold(reader, |mut reader| async move {
            let mut line = String::new();
            match tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut line).await {
                Ok(0) => None,
                Ok(_) => {
                    trim_line_ending(&mut line);
                    Some((Ok(line), reader))
                }
                Err(e) => Some((Err(e), reader)),
            }
        });

        let writer = tokio::io::BufWriter::new(write_half);
        let outgoing_sink =
            futures::sink::unfold(writer, |mut writer, line: String| async move {
                use tokio::io::AsyncWriteExt;
                writer.write_all(line.as_bytes()).await.map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::BrokenPipe, e.to_string())
                })?;
                writer.write_all(b"\n").await.map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::BrokenPipe, e.to_string())
                })?;
                writer.flush().await.map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::BrokenPipe, e.to_string())
                })?;
                Ok::<_, std::io::Error>(writer)
            });

        let protocol = agent_client_protocol::ConnectTo::<Client>::connect_to(
            Lines::new(outgoing_sink, incoming_lines),
            client,
        );

        let _child_guard = child_guard;

        tokio::pin!(protocol);
        tokio::pin!(child_monitor);
        tokio::select! {
            result = &mut protocol => result,
            result = &mut child_monitor => match result {
                Ok(()) => {
                    let _ = append_runtime_event_log(
                        &log_config,
                        "agent/clean_exit_waiting_for_protocol",
                        &json!({ "reason": "agent process exited before protocol completed" }),
                    );
                    (&mut protocol).await
                }
                Err(error) => Err(error),
            },
        }
    }
}

fn read_agent_stdout(
    stdout: std::process::ChildStdout,
    tx: futures_mpsc::UnboundedSender<std::io::Result<String>>,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                trim_line_ending(&mut line);
                if tx.unbounded_send(Ok(line)).is_err() {
                    break;
                }
            }
            Err(err) => {
                let _ = tx.unbounded_send(Err(err));
                break;
            }
        }
    }
}

fn read_agent_stderr(stderr: std::process::ChildStderr, tx: mpsc::Sender<String>) {
    let mut reader = BufReader::new(stderr);
    let mut collected = String::new();
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                trim_line_ending(&mut line);
                if !collected.is_empty() {
                    collected.push('\n');
                }
                collected.push_str(&line);
            }
            Err(_) => break,
        }
    }
    let _ = tx.send(collected);
}

fn write_agent_stdin(
    shared_stdin: Arc<Mutex<Option<std::process::ChildStdin>>>,
    rx: mpsc::Receiver<String>,
) {
    for line in rx {
        let Ok(mut guard) = shared_stdin.lock() else {
            break;
        };
        let Some(ref mut stdin) = *guard else {
            break;
        };
        if stdin.write_all(line.as_bytes()).is_err() {
            break;
        }
        if stdin.write_all(b"\n").is_err() {
            break;
        }
        if stdin.flush().is_err() {
            break;
        }
    }
}

fn trim_line_ending(line: &mut String) {
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
}

struct AgentChildGuard {
    child: Arc<Mutex<Option<Child>>>,
    #[cfg(windows)]
    _job: Option<WindowsKillOnDropJob>,
}

impl AgentChildGuard {
    fn new(child: Child, shutdown_signal: ShutdownSignal) -> Self {
        #[cfg(windows)]
        let job = WindowsKillOnDropJob::for_child(&child).ok();
        let guard = Self {
            child: Arc::new(Mutex::new(Some(child))),
            #[cfg(windows)]
            _job: job,
        };
        shutdown_signal.register_agent_child(&guard.child);
        if shutdown_signal.is_requested() {
            let _ = kill_child_handle(&guard.child);
        }
        guard
    }

    fn handle(&self) -> Arc<Mutex<Option<Child>>> {
        self.child.clone()
    }
}

#[cfg(windows)]
struct WindowsKillOnDropJob(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
unsafe impl Send for WindowsKillOnDropJob {}

#[cfg(windows)]
impl WindowsKillOnDropJob {
    fn for_child(child: &Child) -> anyhow::Result<Self> {
        use std::mem::{size_of, zeroed};
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                anyhow::bail!("CreateJobObjectW failed");
            }

            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set_ok = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &mut info as *mut _ as *mut _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if set_ok == 0 {
                CloseHandle(job);
                anyhow::bail!("SetInformationJobObject failed");
            }

            let process = child.as_raw_handle() as HANDLE;
            let assign_ok = AssignProcessToJobObject(job, process);
            if assign_ok == 0 {
                CloseHandle(job);
                anyhow::bail!("AssignProcessToJobObject failed");
            }

            Ok(Self(job))
        }
    }
}

#[cfg(windows)]
impl Drop for WindowsKillOnDropJob {
    fn drop(&mut self) {
        unsafe {
            let _ = windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

impl Drop for AgentChildGuard {
    fn drop(&mut self) {
        let _ = kill_child_handle(&self.child);
    }
}

fn kill_child_handle(child: &Arc<Mutex<Option<Child>>>) -> anyhow::Result<()> {
    let Ok(mut guard) = child.lock() else {
        return Ok(());
    };
    let Some(mut child) = guard.take() else {
        return Ok(());
    };

    match child.try_wait() {
        Ok(Some(_)) => {}
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    Ok(())
}

async fn monitor_hidden_agent_child(
    child: Arc<Mutex<Option<Child>>>,
    stderr_rx: mpsc::Receiver<String>,
    log_config: Option<SessionConfig>,
) -> agent_client_protocol::Result<()> {
    let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
    thread::spawn(move || {
        loop {
            let result = {
                let mut guard = match child.lock() {
                    Ok(guard) => guard,
                    Err(_) => {
                        let _ =
                            exit_tx.send(Err(std::io::Error::other("agent child lock poisoned")));
                        return;
                    }
                };
                let Some(child) = guard.as_mut() else {
                    let _ = exit_tx.send(Err(std::io::Error::other("agent child handle closed")));
                    return;
                };
                match child.try_wait() {
                    Ok(Some(status)) => {
                        guard.take();
                        Some(Ok(status))
                    }
                    Ok(None) => None,
                    Err(error) => {
                        guard.take();
                        Some(Err(error))
                    }
                }
            };

            if let Some(result) = result {
                let _ = exit_tx.send(result);
                return;
            }
            thread::sleep(std::time::Duration::from_millis(50));
        }
    });

    let status = exit_rx
        .await
        .map_err(|_| agent_client_protocol::util::internal_error("agent wait thread closed"))?
        .map_err(agent_client_protocol::Error::into_internal_error)?;
    let success = status.success();
    let stderr = if success {
        String::new()
    } else {
        stderr_rx.try_recv().unwrap_or_default()
    };
    let payload = if stderr.is_empty() {
        json!({
            "success": success,
            "status": status.to_string(),
            "exitCode": status.code()
        })
    } else {
        json!({
            "success": success,
            "status": status.to_string(),
            "exitCode": status.code(),
            "stderr": stderr.clone()
        })
    };
    if let Some(config) = log_config.as_ref() {
        let _ = append_runtime_event_log(config, "agent/process_exit", &payload);
    }

    if success {
        return Ok(());
    }

    let message = if stderr.is_empty() {
        format!("agent process exited with {status}")
    } else {
        format!("agent process exited with {status}: {stderr}")
    };
    Err(agent_client_protocol::util::internal_error(message))
}

fn parse_env_assignment(value: &str) -> Option<(String, String)> {
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
fn is_probably_powershell_command(command_text: &str) -> bool {
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
fn find_git_bash() -> Option<PathBuf> {
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

pub(crate) fn run_session(
    config: SessionConfig,
    tx_events: mpsc::Sender<ClientEvent>,
    rx_commands: mpsc::Receiver<RuntimeCommand>,
    permission_broker: PermissionBroker,
    shutdown_signal: ShutdownSignal,
) -> anyhow::Result<()> {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            let _ = append_runtime_event_log(
                &config,
                "runtime/session_result",
                &json!({
                    "status": "error",
                    "error": format!("failed to create tokio runtime: {err}")
                }),
            );
            return Err(err).context("failed to create tokio runtime");
        }
    };

    let log_config = config.clone();
    let result: anyhow::Result<()> = runtime.block_on(async move {
        let agent = if config.acp_port > 0 {
            AgentTransport::Tcp(
                TcpAgentProcess::from_config(&config)?.shutdown_signal(shutdown_signal.clone()),
            )
        } else {
            AgentTransport::Stdio(
                HiddenAgentProcess::from_config(&config)?.shutdown_signal(shutdown_signal.clone()),
            )
        };
        let tx_permissions = tx_events.clone();
        let permission_log_config = config.clone();
        let permission_workspace_root = config.workspace_root.clone();
        let permission_request_broker = permission_broker.clone();
        let fs_log_config = config.clone();
        let fs_write_log_config = config.clone();
        let fs_write_tx = tx_events.clone();
        let fs_write_workspace_root = config.workspace_root.clone();
        let fs_read_workspace_root = config.workspace_root.clone();
        let terminal_manager = Arc::new(TerminalManager::default());
        let terminal_create_log_config = config.clone();
        let terminal_output_log_config = config.clone();
        let terminal_wait_log_config = config.clone();
        let terminal_kill_log_config = config.clone();
        let terminal_release_log_config = config.clone();
        let terminal_create_manager = terminal_manager.clone();
        let terminal_output_manager = terminal_manager.clone();
        let terminal_wait_manager = terminal_manager.clone();
        let terminal_kill_manager = terminal_manager.clone();
        let terminal_release_manager = terminal_manager.clone();

        Client
            .builder()
            .name("acp-editor")
            .on_receive_request(
                async move |request: RequestPermissionRequest, responder, _connection| {
                    let request_payload =
                        serde_json::to_value(&request).map_err(|err| anyhow!(err.to_string()))?;
                    append_runtime_event_log(
                        &permission_log_config,
                        "client/request_permission",
                        &request_payload,
                    )?;

                    let request_id = request.tool_call.tool_call_id.0.to_string();
                    let options: Vec<PermissionOption> = request
                        .options
                        .iter()
                        .map(|option| option.name.clone())
                        .enumerate()
                        .map(|(index, label)| PermissionOption {
                            id: request.options[index].option_id.0.to_string(),
                            label,
                            kind: format!("{:?}", request.options[index].kind),
                        })
                        .collect();
                    let option_labels = options
                        .iter()
                        .map(|option| option.label.clone())
                        .collect::<Vec<_>>();
                    let selected_option_id = match decide_permission(
                        permission_request_broker.mode(),
                        &permission_workspace_root,
                        &request,
                    ) {
                        PermissionDecision::Select(option_id) => Some(option_id),
                        PermissionDecision::Cancel => None,
                        PermissionDecision::Ask => {
                            let reply_rx =
                                permission_request_broker.register(request_id.clone())?;

                            let _ = tx_permissions.send(ClientEvent::ToolPermissionRequest {
                                id: request_id.clone(),
                                name: request
                                    .tool_call
                                    .fields
                                    .title
                                    .clone()
                                    .unwrap_or_else(|| "Permission request".into()),
                                options: options.clone(),
                            });
                            let _ = tx_permissions.send(ClientEvent::ToolProgress {
                                id: request_id.clone(),
                                content: format_permission_options(&option_labels),
                            });
                            reply_rx.recv().ok().flatten()
                        }
                    };
                    let selected_option = selected_option_id.as_deref().and_then(|option_id| {
                        request
                            .options
                            .iter()
                            .find(|option| option.option_id.0.as_ref() == option_id)
                    });

                    let response = match selected_option {
                        Some(option) => {
                            RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
                                SelectedPermissionOutcome::new(option.option_id.clone()),
                            ))
                        }
                        None => RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled),
                    };

                    let outcome = match &response.outcome {
                        RequestPermissionOutcome::Selected(selected) => {
                            let label = options
                                .iter()
                                .find(|option| option.id == selected.option_id.0.as_ref())
                                .map(|option| option.label.as_str())
                                .unwrap_or(selected.option_id.0.as_ref());
                            format!("Permission selected: {label}")
                        }
                        RequestPermissionOutcome::Cancelled => {
                            "Permission request cancelled".into()
                        }
                        _ => "Permission handled".into(),
                    };

                    let _ = tx_permissions.send(ClientEvent::ToolPermissionResolved {
                        id: request_id,
                        outcome,
                    });

                    let response_payload =
                        serde_json::to_value(&response).map_err(|err| anyhow!(err.to_string()))?;
                    append_runtime_event_log(
                        &permission_log_config,
                        "client/request_permission_response",
                        &response_payload,
                    )?;

                    responder.respond(response)
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: ReadTextFileRequest, responder, _connection| {
                    let request_payload =
                        serde_json::to_value(&request).map_err(|err| anyhow!(err.to_string()))?;
                    append_runtime_event_log(
                        &fs_log_config,
                        "client/fs_read_text_file",
                        &request_payload,
                    )?;

                    let content = read_workspace_text_file(&fs_read_workspace_root, &request)?;
                    let response = ReadTextFileResponse::new(content);
                    let response_payload =
                        serde_json::to_value(&response).map_err(|err| anyhow!(err.to_string()))?;
                    append_runtime_event_log(
                        &fs_log_config,
                        "client/fs_read_text_file_response",
                        &response_payload,
                    )?;

                    responder.respond(response)
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: WriteTextFileRequest, responder, _connection| {
                    let request_payload =
                        serde_json::to_value(&request).map_err(|err| anyhow!(err.to_string()))?;
                    append_runtime_event_log(
                        &fs_write_log_config,
                        "client/fs_write_text_file",
                        &request_payload,
                    )?;

                    // Read old content before writing for diff tracking
                    let path_for_diff =
                        validate_workspace_path(&fs_write_workspace_root, &request.path).ok();
                    let old_text = path_for_diff
                        .as_ref()
                        .and_then(|p| fs::read_to_string(p).ok());

                    write_workspace_text_file(&fs_write_log_config.workspace_root, &request)?;

                    // Emit ToolDiff event so session changes can be tracked
                    let path_display = path_for_diff
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| request.path.display().to_string());
                    let _ = fs_write_tx.send(ClientEvent::ToolDiff {
                        id: format!("fs_write:{}", path_display),
                        path: path_display,
                        old_text,
                        new_text: request.content.clone(),
                    });

                    let response = WriteTextFileResponse::new();
                    let response_payload =
                        serde_json::to_value(&response).map_err(|err| anyhow!(err.to_string()))?;
                    append_runtime_event_log(
                        &fs_write_log_config,
                        "client/fs_write_text_file_response",
                        &response_payload,
                    )?;

                    responder.respond(response)
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: CreateTerminalRequest, responder, _connection| {
                    let request_payload =
                        serde_json::to_value(&request).map_err(|err| anyhow!(err.to_string()))?;
                    append_runtime_event_log(
                        &terminal_create_log_config,
                        "client/terminal_create",
                        &request_payload,
                    )?;

                    let response = terminal_create_manager
                        .create_terminal(&terminal_create_log_config.workspace_root, &request)?;
                    let response_payload =
                        serde_json::to_value(&response).map_err(|err| anyhow!(err.to_string()))?;
                    append_runtime_event_log(
                        &terminal_create_log_config,
                        "client/terminal_create_response",
                        &response_payload,
                    )?;

                    responder.respond(response)
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: TerminalOutputRequest, responder, _connection| {
                    let request_payload =
                        serde_json::to_value(&request).map_err(|err| anyhow!(err.to_string()))?;
                    append_runtime_event_log(
                        &terminal_output_log_config,
                        "client/terminal_output",
                        &request_payload,
                    )?;

                    let response = terminal_output_manager.terminal_output(&request)?;
                    let response_payload =
                        serde_json::to_value(&response).map_err(|err| anyhow!(err.to_string()))?;
                    append_runtime_event_log(
                        &terminal_output_log_config,
                        "client/terminal_output_response",
                        &response_payload,
                    )?;

                    responder.respond(response)
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: WaitForTerminalExitRequest, responder, _connection| {
                    let request_payload =
                        serde_json::to_value(&request).map_err(|err| anyhow!(err.to_string()))?;
                    append_runtime_event_log(
                        &terminal_wait_log_config,
                        "client/terminal_wait_for_exit",
                        &request_payload,
                    )?;

                    let response = terminal_wait_manager.wait_for_terminal_exit(&request)?;
                    let response_payload =
                        serde_json::to_value(&response).map_err(|err| anyhow!(err.to_string()))?;
                    append_runtime_event_log(
                        &terminal_wait_log_config,
                        "client/terminal_wait_for_exit_response",
                        &response_payload,
                    )?;

                    responder.respond(response)
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: KillTerminalRequest, responder, _connection| {
                    let request_payload =
                        serde_json::to_value(&request).map_err(|err| anyhow!(err.to_string()))?;
                    append_runtime_event_log(
                        &terminal_kill_log_config,
                        "client/terminal_kill",
                        &request_payload,
                    )?;

                    let response = terminal_kill_manager.kill_terminal(&request)?;
                    let response_payload =
                        serde_json::to_value(&response).map_err(|err| anyhow!(err.to_string()))?;
                    append_runtime_event_log(
                        &terminal_kill_log_config,
                        "client/terminal_kill_response",
                        &response_payload,
                    )?;

                    responder.respond(response)
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: ReleaseTerminalRequest, responder, _connection| {
                    let request_payload =
                        serde_json::to_value(&request).map_err(|err| anyhow!(err.to_string()))?;
                    append_runtime_event_log(
                        &terminal_release_log_config,
                        "client/terminal_release",
                        &request_payload,
                    )?;

                    let response = terminal_release_manager.release_terminal(&request)?;
                    let response_payload =
                        serde_json::to_value(&response).map_err(|err| anyhow!(err.to_string()))?;
                    append_runtime_event_log(
                        &terminal_release_log_config,
                        "client/terminal_release_response",
                        &response_payload,
                    )?;

                    responder.respond(response)
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_with(agent, move |connection: ConnectionTo<Agent>| {
                let tx_events = tx_events.clone();
                let config = config.clone();
                async move {
                    let init = InitializeRequest::new(ProtocolVersion::V1)
                        .client_capabilities(
                            ClientCapabilities::new()
                                .fs(FileSystemCapabilities::new()
                                    .read_text_file(true)
                                    .write_text_file(true))
                                .terminal(true),
                        )
                        .client_info(
                            Implementation::new("acp-editor", "0.1.0")
                                .title("ACP Editor Prototype"),
                        );

                    let init_response = connection
                        .send_request(init)
                        .block_task()
                        .await
                        .map_err(|err| anyhow!(err.to_string()))?;

                    let prompt_capabilities = prompt_capabilities_from_acp(
                        &init_response.agent_capabilities.prompt_capabilities,
                    );
                    let supports_load_session = init_response.agent_capabilities.load_session;
                    let advertised_session_list =
                        advertised_session_list_capability(&init_response.agent_capabilities);
                    let codex_session_list_fallback =
                        !advertised_session_list && command_implies_codex_session_list(&config);
                    let supports_session_list =
                        advertised_session_list || codex_session_list_fallback;
                    append_runtime_event_log(
                        &config,
                        "session/capabilities",
                        &json!({
                            "loadSession": supports_load_session,
                            "sessionCapabilities": &init_response.agent_capabilities.session_capabilities,
                            "advertisedSessionList": advertised_session_list,
                            "codexAcpSessionListFallback": codex_session_list_fallback,
                            "supportsSessionList": supports_session_list,
                        }),
                    )?;
                    let has_resume_id = config.resume_session_id.is_some();

                    // Decide whether to load an existing session or create a new one
                    let (mut session, initial_session_config) =
                        if supports_load_session && config.resume_session_id.is_some() {
                            let session_id_str = config.resume_session_id.as_ref().unwrap();
                            let session_id: SessionId = session_id_str.clone().into();

                            // Send the load request first — agent will replay history via
                            // session/update then respond when done. We intentionally do NOT
                            // attach a handler yet, so the replayed events are discarded.
                            // Kodex already restored the conversation from SQLite.
                            let load_req = LoadSessionRequest::new(
                                session_id.clone(),
                                PathBuf::from(&config.workspace_root),
                            );
                            let load_response = connection
                                .send_request(load_req)
                                .block_task()
                                .await
                                .map_err(|err| anyhow!(err.to_string()))?;
                            let initial_session_config = session_config_from_parts(
                                load_response.config_options,
                                load_response.modes.as_ref(),
                                load_response.models.as_ref(),
                            );

                            // Now attach the session handler. The channel starts empty
                            // since all replay events were already delivered (and dropped).
                            let fake_response = NewSessionResponse::new(session_id);
                            let session = connection
                                .attach_session(fake_response, Default::default())
                                .map_err(|err| anyhow!(err.to_string()))?;
                            (session, initial_session_config)
                        } else {
                            let new_request =
                                NewSessionRequest::new(PathBuf::from(&config.workspace_root));
                            let new_response = connection
                                .send_request_to(Agent, new_request)
                                .block_task()
                                .await
                                .map_err(|err| anyhow!(err.to_string()))?;
                            let initial_session_config = session_config_from_parts(
                                new_response.config_options.clone(),
                                new_response.modes.as_ref(),
                                new_response.models.as_ref(),
                            );
                            let session = connection
                                .attach_session(new_response, Default::default())
                                .map_err(|err| anyhow!(err.to_string()))?;
                            (session, initial_session_config)
                        };

                    // If we loaded an existing session, drain any buffered replay
                    // events from the session channel. These are conversation history
                    // that the agent replayed during session/load — we already have
                    // this data in SQLite so we must NOT forward it to the UI.
                    // However, we DO forward session-level state like available_commands
                    // and config updates, since those represent current session state.
                    if supports_load_session && has_resume_id {
                        loop {
                            match tokio::time::timeout(
                                std::time::Duration::from_millis(100),
                                session.read_update(),
                            )
                            .await
                            {
                                Ok(Ok(update)) => {
                                    // Forward session-level state updates (commands, config)
                                    // but discard conversation replay events.
                                    if let agent_client_protocol::SessionMessage::SessionMessage(
                                        dispatch,
                                    ) = update
                                    {
                                        let _ = agent_client_protocol::util::MatchDispatch::new(
                                            dispatch,
                                        )
                                        .if_notification(
                                            async |notification: SessionNotification| {
                                                match &notification.update {
                                                    SessionUpdate::AvailableCommandsUpdate(_)
                                                    | SessionUpdate::ConfigOptionUpdate(_)
                                                    | SessionUpdate::CurrentModeUpdate(_) => {
                                                        let _ = emit_notification(
                                                            &tx_events,
                                                            &config.workspace_root,
                                                            notification,
                                                        );
                                                    }
                                                    _ => {
                                                        // Discard conversation replay
                                                    }
                                                }
                                                Ok(())
                                            },
                                        )
                                        .await
                                        .otherwise(|_dispatch: Dispatch| async { Ok(()) })
                                        .await;
                                    }
                                    continue;
                                }
                                Ok(Err(_)) => break, // channel error, stop draining
                                Err(_) => break,     // timeout = no more buffered events
                            }
                        }
                    }

                    let _ = tx_events.send(ClientEvent::SessionStarted {
                        session_id: session.session_id().0.to_string(),
                    });
                    if supports_session_list {
                        if let Err(error) = sync_session_title_from_list(
                            &config,
                            &tx_events,
                            &connection,
                            session.session_id(),
                        )
                        .await
                        {
                            let _ = append_runtime_event_log(
                                &config,
                                "session/list_title_sync_failed",
                                &json!({
                                    "phase": "startup",
                                    "error": error.to_string(),
                                }),
                            );
                        }
                    }
                    let _ = tx_events.send(ClientEvent::PromptCapabilitiesUpdated {
                        capabilities: prompt_capabilities.clone(),
                    });
                    if initial_session_config.hydrated {
                        let _ = tx_events.send(ClientEvent::SessionConfigUpdated {
                            state: initial_session_config,
                        });
                    }
                    loop {
                        let command = rx_commands
                            .recv()
                            .map_err(|_| anyhow!("ACP command channel closed"))?;

                        match command {
                            RuntimeCommand::SendPrompt(prompt) => {
                                if prompt_contains_image(&prompt) && !prompt_capabilities.image {
                                    let _ = tx_events.send(ClientEvent::Interrupted {
                                        reason: "Active agent does not support image prompts"
                                            .into(),
                                    });
                                    continue;
                                }
                                if prompt_contains_file(&prompt)
                                    && !prompt_capabilities.embedded_context
                                {
                                    let _ = tx_events.send(ClientEvent::Interrupted {
                                        reason: "Active agent does not support file attachments"
                                            .into(),
                                    });
                                    continue;
                                }

                                let content_blocks = prompt
                                    .into_iter()
                                    .filter_map(prompt_content_to_acp)
                                    .collect::<Vec<_>>();
                                if content_blocks.is_empty() {
                                    let _ = tx_events.send(ClientEvent::Interrupted {
                                        reason: "Prompt cannot be empty".into(),
                                    });
                                    continue;
                                }

                                let (stop_tx, stop_rx) = mpsc::channel();
                                session
                                    .connection()
                                    .send_request_to(
                                        Agent,
                                        PromptRequest::new(
                                            session.session_id().clone(),
                                            content_blocks,
                                        ),
                                    )
                                    .on_receiving_result(async move |result| {
                                        let PromptResponse { stop_reason, .. } = result?;
                                        stop_tx.send(stop_reason).map_err(|_| {
                                            agent_client_protocol::util::internal_error(
                                                "prompt stop channel closed",
                                            )
                                        })?;
                                        Ok(())
                                    })
                                    .map_err(|err| anyhow!(err.to_string()))?;

                                let mut cancel_sent = false;
                                loop {
                                    let next_message = tokio::time::timeout(
                                        std::time::Duration::from_millis(50),
                                        session.read_update(),
                                    )
                                    .await;

                                    let message = match next_message {
                                        Ok(Ok(message)) => Some(message),
                                        Ok(Err(err)) => {
                                            if let Some(reason) =
                                                recv_stop_reason_with_grace(&stop_rx).await
                                            {
                                                append_runtime_event_log(
                                                    &config,
                                                    "session/read_update_closed_after_stop",
                                                    &json!({ "error": err.to_string() }),
                                                )?;
                                                emit_turn_finished(
                                                    &config,
                                                    &tx_events,
                                                    &session.connection(),
                                                    session.session_id(),
                                                    supports_session_list,
                                                    reason,
                                                )
                                                .await?;
                                                return Ok(());
                                            }
                                            append_runtime_event_log(
                                                &config,
                                                "session/read_update_error",
                                                &json!({ "error": err.to_string() }),
                                            )?;
                                            return Err(err);
                                        }
                                        Err(_) => None,
                                    };

                                    while let Ok(command) = rx_commands.try_recv() {
                                        match command {
                                            RuntimeCommand::CancelPrompt { reply_tx } => {
                                                let result = (|| -> anyhow::Result<()> {
                                                    if cancel_sent {
                                                        return Ok(());
                                                    }
                                                    permission_broker.cancel_all()?;
                                                    session
                                                        .connection()
                                                        .send_notification_to(
                                                            Agent,
                                                            CancelNotification::new(
                                                                session.session_id().clone(),
                                                            ),
                                                        )
                                                        .map_err(|err| anyhow!(err.to_string()))?;
                                                    append_runtime_event_log(
                                                        &config,
                                                        "session/cancel",
                                                        &json!({
                                                            "sessionId": session.session_id().0
                                                        }),
                                                    )?;
                                                    cancel_sent = true;
                                                    Ok(())
                                                })(
                                                );
                                                let _ = reply_tx.send(result);
                                            }
                                            RuntimeCommand::Shutdown => {
                                                shutdown_signal.request_shutdown();
                                                return Ok(());
                                            }
                                            RuntimeCommand::ResolveCodeBuddyInterruption {
                                                session_id,
                                                tool_call_id,
                                                decision,
                                                reply_tx,
                                            } => {
                                                let connection = session.connection();
                                                let result =
                                                    send_codebuddy_interruption_resolution(
                                                        &config,
                                                        &connection,
                                                        &session_id,
                                                        &tool_call_id,
                                                        &decision,
                                                    )
                                                    .await;
                                                if result.is_ok() {
                                                    let _ = permission_broker
                                                        .clear_early_resolution(&tool_call_id);
                                                }
                                                let _ = reply_tx.send(result);
                                            }
                                            RuntimeCommand::SendPrompt(_)
                                            | RuntimeCommand::SetConfigOption { .. }
                                            | RuntimeCommand::SetMode { .. }
                                            | RuntimeCommand::SetModel { .. } => {}
                                        }
                                    }

                                    let Some(message) = message else {
                                        if let Ok(reason) = stop_rx.try_recv() {
                                            emit_turn_finished(
                                                &config,
                                                &tx_events,
                                                &session.connection(),
                                                session.session_id(),
                                                supports_session_list,
                                                reason,
                                            )
                                            .await?;
                                            break;
                                        }
                                        continue;
                                    };

                                    match message {
                                        agent_client_protocol::SessionMessage::SessionMessage(
                                            dispatch,
                                        ) => {
                                            agent_client_protocol::util::MatchDispatch::new(
                                                dispatch,
                                            )
                                            .if_notification(
                                                async |notification: SessionNotification| {
                                                    append_typed_notification_log(
                                                        &config,
                                                        &notification,
                                                    )?;
                                                    emit_notification(
                                                        &tx_events,
                                                        &config.workspace_root,
                                                        notification,
                                                    )?;
                                                    Ok(())
                                                },
                                            )
                                            .await
                                            .otherwise(|dispatch: Dispatch| async {
                                                if let Dispatch::Notification(untyped) = dispatch {
                                                    let (method, payload) = untyped.into_parts();
                                                    append_notification_log(
                                                        &config, &method, &payload,
                                                    )?;
                                                }
                                                Ok(())
                                            })
                                            .await
                                            .map_err(|err| anyhow!(err.to_string()))?;
                                        }
                                        agent_client_protocol::SessionMessage::StopReason(
                                            reason,
                                        ) => {
                                            emit_turn_finished(
                                                &config,
                                                &tx_events,
                                                &session.connection(),
                                                session.session_id(),
                                                supports_session_list,
                                                reason,
                                            )
                                            .await?;
                                            break;
                                        }
                                        other => {
                                            let payload = json!({
                                                "message": format!("{other:?}")
                                            });
                                            append_runtime_event_log(
                                                &config,
                                                "session/message_other",
                                                &payload,
                                            )?;
                                        }
                                    }
                                }
                            }
                            RuntimeCommand::SetConfigOption {
                                config_id,
                                value_id,
                                reply_tx,
                            } => {
                                let result = async {
                                    let response = session
                                        .connection()
                                        .send_request_to(
                                            Agent,
                                            SetSessionConfigOptionRequest::new(
                                                session.session_id().clone(),
                                                config_id,
                                                value_id,
                                            ),
                                        )
                                        .block_task()
                                        .await
                                        .map_err(|err| anyhow!(err.to_string()))?;
                                    Ok(vec![ClientEvent::SessionConfigUpdated {
                                        state: session_config_from_options(response.config_options),
                                    }])
                                }
                                .await;
                                let _ = reply_tx.send(result);
                            }
                            RuntimeCommand::SetMode { mode_id, reply_tx } => {
                                let result = async {
                                    session
                                        .connection()
                                        .send_request_to(
                                            Agent,
                                            SetSessionModeRequest::new(
                                                session.session_id().clone(),
                                                mode_id.clone(),
                                            ),
                                        )
                                        .block_task()
                                        .await
                                        .map_err(|err| anyhow!(err.to_string()))?;
                                    Ok(vec![ClientEvent::SessionConfigValueChanged {
                                        control_id: "mode".into(),
                                        value_id: mode_id,
                                        value_label: None,
                                    }])
                                }
                                .await;
                                let _ = reply_tx.send(result);
                            }
                            RuntimeCommand::SetModel { model_id, reply_tx } => {
                                let result = async {
                                    session
                                        .connection()
                                        .send_request_to(
                                            Agent,
                                            SetSessionModelRequest::new(
                                                session.session_id().clone(),
                                                model_id.clone(),
                                            ),
                                        )
                                        .block_task()
                                        .await
                                        .map_err(|err| anyhow!(err.to_string()))?;
                                    Ok(vec![ClientEvent::SessionConfigValueChanged {
                                        control_id: "model".into(),
                                        value_id: model_id,
                                        value_label: None,
                                    }])
                                }
                                .await;
                                let _ = reply_tx.send(result);
                            }
                            RuntimeCommand::ResolveCodeBuddyInterruption {
                                session_id,
                                tool_call_id,
                                decision,
                                reply_tx,
                            } => {
                                let connection = session.connection();
                                let result = send_codebuddy_interruption_resolution(
                                    &config,
                                    &connection,
                                    &session_id,
                                    &tool_call_id,
                                    &decision,
                                )
                                .await;
                                if result.is_ok() {
                                    let _ = permission_broker.clear_early_resolution(&tool_call_id);
                                }
                                let _ = reply_tx.send(result);
                            }
                            RuntimeCommand::CancelPrompt { reply_tx } => {
                                let _ = permission_broker.cancel_all();
                                let _ = reply_tx.send(Ok(()));
                            }
                            RuntimeCommand::Shutdown => {
                                shutdown_signal.request_shutdown();
                                break;
                            }
                        }
                    }

                    Ok(())
                }
            })
            .await
            .map_err(|err| anyhow!(err.to_string()))?;

        Ok(())
    });

    let payload = match &result {
        Ok(()) => json!({ "status": "ok" }),
        Err(error) => json!({ "status": "error", "error": error.to_string() }),
    };
    let _ = append_runtime_event_log(&log_config, "runtime/session_result", &payload);

    result
}

async fn send_codebuddy_interruption_resolution(
    config: &SessionConfig,
    connection: &ConnectionTo<Agent>,
    session_id: &str,
    tool_call_id: &str,
    decision: &str,
) -> anyhow::Result<()> {
    let payload = json!({
        "sessionId": session_id,
        "toolCallId": tool_call_id,
        "decision": decision,
    });
    append_runtime_event_log(config, "codebuddy/resolve_interruption", &payload)?;

    let params: Arc<serde_json::value::RawValue> =
        serde_json::value::RawValue::from_string(payload.to_string())?.into();
    let request = ClientRequest::ExtMethodRequest(ExtRequest::new(
        "_codebuddy.ai/resolveInterruption",
        params,
    ));

    connection
        .send_request_to(Agent, request)
        .block_task()
        .await
        .map_err(|err| anyhow!(err.to_string()))?;

    Ok(())
}

async fn emit_turn_finished(
    config: &SessionConfig,
    tx_events: &mpsc::Sender<ClientEvent>,
    connection: &ConnectionTo<Agent>,
    session_id: &SessionId,
    supports_session_list: bool,
    reason: StopReason,
) -> anyhow::Result<()> {
    let stop_reason = format_stop_reason(reason);
    append_runtime_event_log(
        config,
        "session/stop_reason",
        &json!({ "stopReason": stop_reason.clone() }),
    )?;

    let _ = tx_events.send(ClientEvent::TurnFinished { stop_reason });
    if supports_session_list {
        sync_session_title_from_list_after_turn(config, tx_events, connection, session_id).await;
    }
    Ok(())
}

fn advertised_session_list_capability(agent_capabilities: &AgentCapabilities) -> bool {
    agent_capabilities.session_capabilities.list.is_some()
}

fn command_implies_codex_session_list(config: &SessionConfig) -> bool {
    config
        .agent_command
        .to_ascii_lowercase()
        .contains("codex-acp")
}

async fn sync_session_title_from_list(
    config: &SessionConfig,
    tx_events: &mpsc::Sender<ClientEvent>,
    connection: &ConnectionTo<Agent>,
    session_id: &SessionId,
) -> anyhow::Result<bool> {
    append_runtime_event_log(
        config,
        "session/list_title_sync_start",
        &json!({
            "sessionId": session_id.0.as_ref(),
            "cwd": config.workspace_root,
            "timeoutMs": TITLE_SYNC_TIMEOUT_MS,
        }),
    )?;
    let request = ListSessionsRequest::new().cwd(PathBuf::from(&config.workspace_root));
    let response = tokio::time::timeout(
        std::time::Duration::from_millis(TITLE_SYNC_TIMEOUT_MS),
        connection.send_request_to(Agent, request).block_task(),
    )
    .await
    .map_err(|_| anyhow!("session/list timed out after {TITLE_SYNC_TIMEOUT_MS}ms"))?
    .map_err(|err| anyhow!(err.to_string()))?;
    let session_count = response.sessions.len();

    let matched = response
        .sessions
        .into_iter()
        .find(|session| session.session_id == *session_id);

    if let Some(title) = matched
        .as_ref()
        .and_then(|session| session.title.as_deref())
    {
        let trimmed = title.trim();
        if !trimmed.is_empty() {
            append_runtime_event_log(
                config,
                "session/list_title_sync",
                &json!({
                    "sessionId": session_id.0.as_ref(),
                    "title": trimmed,
                }),
            )?;
            let _ = tx_events.send(ClientEvent::SessionTitleUpdated {
                title: trimmed.to_string(),
            });
            return Ok(true);
        }
    }

    append_runtime_event_log(
        config,
        "session/list_title_sync_empty",
        &json!({
            "sessionId": session_id.0.as_ref(),
            "sessionCount": session_count,
            "matched": matched.is_some(),
            "hasTitle": matched.as_ref().and_then(|session| session.title.as_ref()).is_some(),
        }),
    )?;

    Ok(false)
}

async fn sync_session_title_from_list_after_turn(
    config: &SessionConfig,
    tx_events: &mpsc::Sender<ClientEvent>,
    connection: &ConnectionTo<Agent>,
    session_id: &SessionId,
) {
    if let Err(error) =
        sync_session_title_from_list(config, tx_events, connection, session_id).await
    {
        let _ = append_runtime_event_log(
            config,
            "session/list_title_sync_failed",
            &json!({ "error": error.to_string() }),
        );
        return;
    }

    for delay_ms in TITLE_SYNC_RETRY_DELAYS_MS {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        if let Err(error) =
            sync_session_title_from_list(config, tx_events, connection, session_id).await
        {
            let _ = append_runtime_event_log(
                config,
                "session/list_title_sync_retry_failed",
                &json!({ "delayMs": delay_ms, "error": error.to_string() }),
            );
            return;
        }
    }
}

async fn recv_stop_reason_with_grace(stop_rx: &mpsc::Receiver<StopReason>) -> Option<StopReason> {
    for attempt in 0..5 {
        match stop_rx.try_recv() {
            Ok(reason) => return Some(reason),
            Err(mpsc::TryRecvError::Disconnected) => return None,
            Err(mpsc::TryRecvError::Empty) if attempt < 4 => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(mpsc::TryRecvError::Empty) => return None,
        }
    }
    None
}

fn read_workspace_text_file(
    workspace_root: &str,
    request: &ReadTextFileRequest,
) -> anyhow::Result<String> {
    let path = validate_workspace_path(workspace_root, &request.path)?;

    if path.is_dir() {
        return list_workspace_directory(&path);
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read text file {}", path.display()))?;

    let selected = select_lines(&content, request.line, request.limit);
    Ok(selected)
}

fn write_workspace_text_file(
    workspace_root: &str,
    request: &WriteTextFileRequest,
) -> anyhow::Result<()> {
    let path = validate_workspace_path(workspace_root, &request.path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    fs::write(&path, &request.content)
        .with_context(|| format!("failed to write text file {}", path.display()))?;
    Ok(())
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

fn validate_workspace_path(
    workspace_root: &str,
    requested_path: &std::path::Path,
) -> anyhow::Result<PathBuf> {
    let workspace_root = PathBuf::from(workspace_root)
        .canonicalize()
        .with_context(|| format!("failed to resolve workspace root {workspace_root}"))?;

    let candidate = if requested_path.is_absolute() {
        requested_path.to_path_buf()
    } else {
        workspace_root.join(requested_path)
    };

    let normalized = normalize_path(candidate);
    if !normalized.starts_with(&workspace_root) {
        return Err(anyhow!(
            "ACP file request is outside workspace: {}",
            normalized.display()
        ));
    }

    Ok(normalized)
}

fn normalize_path(path: PathBuf) -> PathBuf {
    if path.exists() {
        return path.canonicalize().unwrap_or(path);
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }

    normalized
}

fn select_lines(content: &str, start_line: Option<u32>, limit: Option<u32>) -> String {
    let Some(start_line) = start_line else {
        return content.to_string();
    };

    let start_index = start_line.saturating_sub(1) as usize;
    let max_lines = limit.unwrap_or(u32::MAX) as usize;

    content
        .lines()
        .skip(start_index)
        .take(max_lines)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::{SessionCapabilities, SessionListCapabilities};
    use std::time::Duration;

    #[test]
    fn hidden_agent_process_uses_workspace_as_current_dir() {
        let process = HiddenAgentProcess::from_command(
            "CODEBUDDY_TEST=1 codebuddy.exe --acp",
            "D:/work/kodex",
        )
        .unwrap();

        assert_eq!(process.command, PathBuf::from("codebuddy.exe"));
        assert_eq!(process.args, vec!["--acp"]);
        assert_eq!(process.env, vec![("CODEBUDDY_TEST".into(), "1".into())]);
        assert_eq!(process.current_dir, PathBuf::from("D:/work/kodex"));
    }

    #[test]
    fn process_cwd_defaults_to_workspace_root() {
        assert_eq!(
            process_cwd("workspace-root", None),
            PathBuf::from("workspace-root")
        );
    }

    #[test]
    fn process_cwd_resolves_relative_request_from_workspace_root() {
        assert_eq!(
            process_cwd("workspace-root", Some(Path::new("backend"))),
            PathBuf::from("workspace-root/backend")
        );
    }

    #[test]
    fn process_context_sets_current_dir_and_pwd() {
        let cwd = PathBuf::from("workspace-root");
        let mut command = Command::new("codebuddy.exe");

        apply_process_cwd_and_pwd(&mut command, &cwd);

        assert_eq!(command.get_current_dir(), Some(cwd.as_path()));
        let pwd = command
            .get_envs()
            .find(|(name, _)| name.to_string_lossy() == "PWD")
            .and_then(|(_, value)| value)
            .map(|value| value.to_string_lossy().to_string());
        assert_eq!(pwd.as_deref(), Some("workspace-root"));
    }

    #[test]
    fn permission_broker_delivers_pending_resolution() {
        let broker = PermissionBroker::default();
        let rx = broker.register("call-1".into()).unwrap();

        let delivered = broker.resolve("call-1", Some("allow".into())).unwrap();

        assert!(delivered);
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(50)).unwrap(),
            Some("allow".into())
        );
    }

    #[test]
    fn permission_broker_replays_early_resolution() {
        let broker = PermissionBroker::default();

        let delivered = broker.resolve("call-1", Some("allowAll".into())).unwrap();
        let rx = broker.register("call-1".into()).unwrap();

        assert!(!delivered);
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(50)).unwrap(),
            Some("allowAll".into())
        );
    }

    #[test]
    fn permission_broker_cancel_clears_early_resolutions() {
        let broker = PermissionBroker::default();

        let delivered = broker.resolve("call-1", Some("allow".into())).unwrap();
        broker.cancel_all().unwrap();
        let rx = broker.register("call-1".into()).unwrap();

        assert!(!delivered);
        assert!(rx.recv_timeout(Duration::from_millis(10)).is_err());
    }

    #[test]
    fn advertised_session_list_capability_detects_initialize_capability() {
        let capabilities = AgentCapabilities::new()
            .session_capabilities(SessionCapabilities::new().list(SessionListCapabilities::new()));

        assert!(advertised_session_list_capability(&capabilities));
    }

    fn test_session_config(agent_command: &str) -> SessionConfig {
        SessionConfig {
            workspace_root: String::new(),
            app_data_root: String::new(),
            model: String::new(),
            agent_command: agent_command.into(),
            agent_env: Vec::new(),
            resume_session_id: None,
            log_id: String::new(),
            acp_port: 0,
        }
    }

    #[test]
    fn codex_agent_command_implies_session_list_support() {
        let config = test_session_config(r#"C:\Users\yvonchen\.kodex\bin\codex-acp.exe"#);

        assert!(command_implies_codex_session_list(&config));
    }

    #[test]
    fn non_codex_agent_command_does_not_imply_session_list_support() {
        let config = test_session_config("codebuddy.exe --acp");

        assert!(!command_implies_codex_session_list(&config));
    }
}
