use anyhow::{Context, anyhow, bail};
use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;
use uuid::Uuid;
use workspace_model::{
    TerminalExitEvent, TerminalOutputEvent, TerminalSession, TerminalSessionStatus,
    TerminalStatusEvent,
};

pub type TerminalEventSink = Arc<dyn Fn(TerminalServiceEvent) + Send + Sync + 'static>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalServiceEvent {
    Output(TerminalOutputEvent),
    Status(TerminalStatusEvent),
    Exit(TerminalExitEvent),
}

#[derive(Clone, Default)]
pub struct TerminalService {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    sessions: HashMap<String, TerminalEntry>,
    event_sink: Option<TerminalEventSink>,
}

struct TerminalEntry {
    session: TerminalSession,
    workspace_key: String,
    master: Box<dyn MasterPty + Send>,
    input_tx: mpsc::Sender<String>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellProfile {
    pub command: String,
    pub args: Vec<String>,
    pub display_name: String,
}

impl TerminalService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_event_sink(&self, sink: TerminalEventSink) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.event_sink = Some(sink);
        }
    }

    pub fn clear_event_sink(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.event_sink = None;
        }
    }

    pub fn open_workspace(
        &self,
        workspace_root: impl AsRef<Path>,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<TerminalSession> {
        let workspace_root = canonical_workspace_root(workspace_root.as_ref())?;
        let workspace_key = normalize_path_key(&workspace_root);
        let cols = sanitize_cols(cols);
        let rows = sanitize_rows(rows);

        if let Some(existing) = self.live_terminal_for_workspace(&workspace_key)? {
            return Ok(existing);
        }

        self.remove_exited_for_workspace(&workspace_key)?;

        self.spawn_workspace_terminal(workspace_root, workspace_key, cols, rows)
    }

    pub fn open_workspace_new(
        &self,
        workspace_root: impl AsRef<Path>,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<TerminalSession> {
        let workspace_root = canonical_workspace_root(workspace_root.as_ref())?;
        let workspace_key = normalize_path_key(&workspace_root);
        let cols = sanitize_cols(cols);
        let rows = sanitize_rows(rows);

        self.spawn_workspace_terminal(workspace_root, workspace_key, cols, rows)
    }

    fn spawn_workspace_terminal(
        &self,
        workspace_root: PathBuf,
        workspace_key: String,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<TerminalSession> {
        let shell = default_shell_profile();
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to open PTY")?;

        let mut command = CommandBuilder::new(&shell.command);
        for arg in &shell.args {
            command.arg(arg);
        }
        configure_workspace_command(&mut command, &workspace_root);

        let child = pair
            .slave
            .spawn_command(command)
            .with_context(|| format!("failed to start shell {}", shell.command))?;
        let killer = child.clone_killer();
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .context("failed to clone PTY reader")?;
        let mut writer = pair
            .master
            .take_writer()
            .context("failed to take PTY writer")?;

        let terminal_id = Uuid::new_v4().to_string();
        let session = TerminalSession {
            terminal_id: terminal_id.clone(),
            workspace_root: workspace_key.clone(),
            cwd: workspace_root.display().to_string(),
            shell: shell.display_name,
            status: TerminalSessionStatus::Running,
            exit_code: None,
            cols,
            rows,
        };

        let seq = Arc::new(AtomicU64::new(0));
        let (input_tx, input_rx) = mpsc::channel::<String>();
        thread::spawn(move || {
            for data in input_rx {
                if writer.write_all(data.as_bytes()).is_err() {
                    break;
                }
                if writer.flush().is_err() {
                    break;
                }
            }
        });

        let output_sink = self.event_sink();
        let output_terminal_id = terminal_id.clone();
        let output_workspace_root = workspace_key.clone();
        let output_seq = seq.clone();
        let output_input_tx = input_tx.clone();
        thread::spawn(move || {
            let mut buffer = [0_u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        let data = String::from_utf8_lossy(&buffer[..n]).to_string();
                        let data = answer_device_status_reports(&data, &output_input_tx);
                        if data.is_empty() {
                            continue;
                        }
                        emit_event(
                            &output_sink,
                            TerminalServiceEvent::Output(TerminalOutputEvent {
                                terminal_id: output_terminal_id.clone(),
                                workspace_root: output_workspace_root.clone(),
                                seq: output_seq.fetch_add(1, Ordering::Relaxed),
                                data,
                            }),
                        );
                    }
                    Err(_) => break,
                }
            }
        });

        self.spawn_exit_watcher(terminal_id.clone(), &session, child);

        let entry = TerminalEntry {
            session: session.clone(),
            workspace_key,
            master: pair.master,
            input_tx,
            killer,
        };
        self.inner
            .lock()
            .map_err(|e| anyhow!(e.to_string()))?
            .sessions
            .insert(terminal_id.clone(), entry);

        self.emit_status(&session);
        Ok(session)
    }

    pub fn write(&self, terminal_id: &str, data: &str) -> anyhow::Result<()> {
        let inner = self.inner.lock().map_err(|e| anyhow!(e.to_string()))?;
        let entry = inner
            .sessions
            .get(terminal_id)
            .ok_or_else(|| anyhow!("terminal not found"))?;
        ensure_running(&entry.session)?;
        entry
            .input_tx
            .send(data.to_string())
            .context("failed to queue terminal input")?;
        Ok(())
    }

    pub fn resize(
        &self,
        terminal_id: &str,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<TerminalSession> {
        let mut inner = self.inner.lock().map_err(|e| anyhow!(e.to_string()))?;
        let entry = inner
            .sessions
            .get_mut(terminal_id)
            .ok_or_else(|| anyhow!("terminal not found"))?;
        ensure_running(&entry.session)?;
        let cols = sanitize_cols(cols);
        let rows = sanitize_rows(rows);
        entry
            .master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to resize terminal")?;
        entry.session.cols = cols;
        entry.session.rows = rows;
        Ok(entry.session.clone())
    }

    pub fn terminate(&self, terminal_id: &str) -> anyhow::Result<()> {
        let entry = {
            let mut inner = self.inner.lock().map_err(|e| anyhow!(e.to_string()))?;
            inner
                .sessions
                .remove(terminal_id)
                .ok_or_else(|| anyhow!("terminal not found"))?
        };
        if entry.session.status == TerminalSessionStatus::Exited {
            return Ok(());
        }
        emit_closed_async(self.event_sink(), entry.session.clone(), None);
        kill_entries_async(vec![entry]);
        Ok(())
    }

    pub fn restart(
        &self,
        terminal_id: &str,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<TerminalSession> {
        let entry = {
            let mut inner = self.inner.lock().map_err(|e| anyhow!(e.to_string()))?;
            inner
                .sessions
                .remove(terminal_id)
                .ok_or_else(|| anyhow!("terminal not found"))?
        };
        let workspace_root = PathBuf::from(entry.session.workspace_root.clone());
        emit_closed_async(self.event_sink(), entry.session.clone(), None);
        kill_entries_async(vec![entry]);
        self.open_workspace_new(workspace_root, cols, rows)
    }

    pub fn list_workspace(
        &self,
        workspace_root: impl AsRef<Path>,
    ) -> anyhow::Result<Vec<TerminalSession>> {
        let workspace_key = normalize_path_key(&canonical_workspace_root(workspace_root.as_ref())?);
        let inner = self.inner.lock().map_err(|e| anyhow!(e.to_string()))?;
        Ok(inner
            .sessions
            .values()
            .filter(|entry| entry.workspace_key == workspace_key)
            .map(|entry| entry.session.clone())
            .collect())
    }

    pub fn shutdown_workspace(&self, workspace_root: impl AsRef<Path>) {
        let Ok(workspace_root) = canonical_workspace_root(workspace_root.as_ref()) else {
            return;
        };
        let workspace_key = normalize_path_key(&workspace_root);
        let entries = {
            let Ok(mut inner) = self.inner.lock() else {
                return;
            };
            let ids = inner
                .sessions
                .iter()
                .filter_map(|(id, entry)| {
                    if entry.workspace_key == workspace_key {
                        Some(id.clone())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|id| inner.sessions.remove(&id))
                .collect::<Vec<_>>()
        };
        kill_entries_async(entries);
    }

    pub fn shutdown_all(&self) {
        let entries = {
            let Ok(mut inner) = self.inner.lock() else {
                return;
            };
            inner
                .sessions
                .drain()
                .map(|(_, entry)| entry)
                .collect::<Vec<_>>()
        };
        kill_entries_async(entries);
    }

    fn live_terminal_for_workspace(
        &self,
        workspace_key: &str,
    ) -> anyhow::Result<Option<TerminalSession>> {
        let inner = self.inner.lock().map_err(|e| anyhow!(e.to_string()))?;
        Ok(inner
            .sessions
            .values()
            .find(|entry| {
                entry.workspace_key == workspace_key
                    && entry.session.status == TerminalSessionStatus::Running
            })
            .map(|entry| entry.session.clone()))
    }

    fn remove_exited_for_workspace(&self, workspace_key: &str) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().map_err(|e| anyhow!(e.to_string()))?;
        inner.sessions.retain(|_, entry| {
            !(entry.workspace_key == workspace_key
                && entry.session.status == TerminalSessionStatus::Exited)
        });
        Ok(())
    }

    fn spawn_exit_watcher(
        &self,
        terminal_id: String,
        session: &TerminalSession,
        mut child: Box<dyn Child + Send>,
    ) {
        let inner = self.inner.clone();
        let sink = self.event_sink();
        let cwd = session.cwd.clone();
        let workspace_root = session.workspace_root.clone();
        let shell = session.shell.clone();
        thread::spawn(move || {
            let status = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break Some(status),
                    Ok(None) => thread::sleep(Duration::from_millis(120)),
                    Err(_) => break None,
                }
            };
            let exit_code = status.map(|status| status.exit_code() as i32);
            let status_event = TerminalStatusEvent {
                terminal_id: terminal_id.clone(),
                workspace_root: workspace_root.clone(),
                status: TerminalSessionStatus::Exited,
                cwd,
                shell,
                exit_code,
            };
            if let Ok(mut inner) = inner.lock() {
                if let Some(entry) = inner.sessions.get_mut(&terminal_id) {
                    entry.session.status = TerminalSessionStatus::Exited;
                    entry.session.exit_code = exit_code;
                }
            }
            emit_event(&sink, TerminalServiceEvent::Status(status_event));
            emit_event(
                &sink,
                TerminalServiceEvent::Exit(TerminalExitEvent {
                    terminal_id,
                    workspace_root,
                    exit_code,
                }),
            );
        });
    }

    fn event_sink(&self) -> Option<TerminalEventSink> {
        self.inner
            .lock()
            .ok()
            .and_then(|inner| inner.event_sink.clone())
    }

    fn emit_status(&self, session: &TerminalSession) {
        emit_event(
            &self.event_sink(),
            TerminalServiceEvent::Status(TerminalStatusEvent {
                terminal_id: session.terminal_id.clone(),
                workspace_root: session.workspace_root.clone(),
                status: session.status.clone(),
                cwd: session.cwd.clone(),
                shell: session.shell.clone(),
                exit_code: session.exit_code,
            }),
        );
    }
}

impl Drop for TerminalService {
    fn drop(&mut self) {
        if Arc::strong_count(&self.inner) == 1 {
            self.shutdown_all();
        }
    }
}

fn ensure_running(session: &TerminalSession) -> anyhow::Result<()> {
    if session.status == TerminalSessionStatus::Running {
        Ok(())
    } else {
        bail!("terminal has exited")
    }
}

fn emit_event(sink: &Option<TerminalEventSink>, event: TerminalServiceEvent) {
    if let Some(sink) = sink {
        sink(event);
    }
}

fn emit_closed_async(
    sink: Option<TerminalEventSink>,
    session: TerminalSession,
    exit_code: Option<i32>,
) {
    thread::spawn(move || {
        emit_event(
            &sink,
            TerminalServiceEvent::Status(TerminalStatusEvent {
                terminal_id: session.terminal_id.clone(),
                workspace_root: session.workspace_root.clone(),
                status: TerminalSessionStatus::Exited,
                cwd: session.cwd.clone(),
                shell: session.shell.clone(),
                exit_code,
            }),
        );
        emit_event(
            &sink,
            TerminalServiceEvent::Exit(TerminalExitEvent {
                terminal_id: session.terminal_id.clone(),
                workspace_root: session.workspace_root.clone(),
                exit_code,
            }),
        );
    });
}

fn answer_device_status_reports(data: &str, input_tx: &mpsc::Sender<String>) -> String {
    const DSR_CURSOR_POSITION_REQUEST: &str = "\x1b[6n";
    if !data.contains(DSR_CURSOR_POSITION_REQUEST) {
        return data.to_string();
    }

    let mut remaining = data;
    let mut filtered = String::with_capacity(data.len());
    while let Some(index) = remaining.find(DSR_CURSOR_POSITION_REQUEST) {
        filtered.push_str(&remaining[..index]);
        let _ = input_tx.send("\x1b[1;1R".to_string());
        remaining = &remaining[index + DSR_CURSOR_POSITION_REQUEST.len()..];
    }
    filtered.push_str(remaining);
    filtered
}

fn kill_entries(entries: Vec<TerminalEntry>) {
    for mut entry in entries {
        let _ = entry.killer.kill();
    }
}

fn kill_entries_async(entries: Vec<TerminalEntry>) {
    if entries.is_empty() {
        return;
    }
    thread::spawn(move || kill_entries(entries));
}

fn sanitize_cols(cols: u16) -> u16 {
    cols.clamp(20, 500)
}

fn sanitize_rows(rows: u16) -> u16 {
    rows.clamp(4, 200)
}

fn canonical_workspace_root(path: &Path) -> anyhow::Result<PathBuf> {
    let root = path
        .canonicalize()
        .with_context(|| format!("workspace does not exist: {}", path.display()))?;
    if !root.is_dir() {
        bail!("workspace is not a directory: {}", root.display());
    }
    Ok(clean_canonical_path(root))
}

fn normalize_path_key(path: &Path) -> String {
    let normalized = path.display().to_string().replace('\\', "/");
    if normalized.len() >= 2 && normalized.as_bytes()[1] == b':' {
        let mut chars = normalized.chars().collect::<Vec<_>>();
        chars[0] = chars[0].to_ascii_lowercase();
        chars.into_iter().collect()
    } else {
        normalized
    }
}

fn clean_canonical_path(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let text = path.display().to_string();
        if let Some(stripped) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{stripped}"));
        }
        if let Some(stripped) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(stripped);
        }
    }
    path
}

pub fn default_shell_profile() -> ShellProfile {
    #[cfg(windows)]
    {
        for candidate in ["pwsh.exe", "powershell.exe", "cmd.exe"] {
            if let Some(path) = resolve_command_on_path(candidate) {
                return ShellProfile {
                    display_name: shell_display_name(candidate),
                    command: path.display().to_string(),
                    args: shell_startup_args(candidate),
                };
            }
        }
        let command = env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into());
        return ShellProfile {
            display_name: shell_display_name(&command),
            args: shell_startup_args(&command),
            command,
        };
    }

    #[cfg(not(windows))]
    {
        let command = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        ShellProfile {
            display_name: shell_display_name(&command),
            command,
            args: Vec::new(),
        }
    }
}

#[cfg(windows)]
fn shell_startup_args(command: &str) -> Vec<String> {
    let display_name = shell_display_name(command).to_ascii_lowercase();
    if display_name == "pwsh" || display_name == "powershell" {
        vec!["-NoLogo".into(), "-NoProfile".into()]
    } else {
        Vec::new()
    }
}

fn shell_display_name(command: &str) -> String {
    Path::new(command)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(command)
        .to_string()
}

fn configure_workspace_command(command: &mut CommandBuilder, workspace_root: &Path) {
    command.cwd(workspace_root.as_os_str());
    command.env("PWD", workspace_root.as_os_str());
}

#[cfg(windows)]
fn resolve_command_on_path(command: &str) -> Option<PathBuf> {
    let command_path = Path::new(command);
    if command_path.is_absolute() && command_path.is_file() {
        return Some(command_path.to_path_buf());
    }
    let path_var = env::var_os("PATH")?;
    let pathext = env::var_os("PATHEXT").unwrap_or_else(|| OsString::from(".EXE;.CMD;.BAT"));
    let extensions = pathext
        .to_string_lossy()
        .split(';')
        .map(|ext| ext.to_string())
        .collect::<Vec<_>>();

    for dir in env::split_paths(&path_var) {
        let direct = dir.join(command);
        if direct.is_file() {
            return Some(direct);
        }
        if Path::new(command).extension().is_some() {
            continue;
        }
        for ext in &extensions {
            let candidate = dir.join(format!("{command}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    use tempfile::tempdir;

    #[test]
    fn default_shell_profile_has_command_and_label() {
        let profile = default_shell_profile();
        assert!(!profile.command.trim().is_empty());
        assert!(!profile.display_name.trim().is_empty());
    }

    #[test]
    fn device_status_report_is_answered_and_stripped() {
        let (tx, rx) = mpsc::channel();
        let filtered = answer_device_status_reports("before\x1b[6nafter", &tx);
        assert_eq!(filtered, "beforeafter");
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(100)).unwrap(),
            "\x1b[1;1R"
        );
    }

    #[test]
    fn workspace_terminal_command_sets_cwd_and_pwd() {
        let dir = tempdir().unwrap();
        let mut command = CommandBuilder::new("pwsh");
        let expected_cwd = dir.path().as_os_str().to_os_string();

        configure_workspace_command(&mut command, dir.path());

        assert_eq!(command.get_cwd(), Some(&expected_cwd));
        assert_eq!(command.get_env("PWD"), Some(dir.path().as_os_str()));
    }

    #[test]
    #[ignore = "real PTY smoke test can hang under Windows test harnesses"]
    fn terminal_lifecycle_emits_output_and_exit() {
        let dir = tempdir().unwrap();
        let service = TerminalService::new();
        let (tx, rx) = mpsc::channel();
        service.set_event_sink(Arc::new(move |event| {
            let _ = tx.send(event);
        }));

        let session = service.open_workspace(dir.path(), 80, 24).unwrap();
        assert_eq!(session.status, TerminalSessionStatus::Running);

        #[cfg(windows)]
        let command = "echo kodex-terminal-test\r\nexit\r\n";
        #[cfg(not(windows))]
        let command = "printf 'kodex-terminal-test\\n'\nexit\n";

        service.write(&session.terminal_id, command).unwrap();

        let deadline = Instant::now() + Duration::from_secs(8);
        let mut saw_output = false;
        let mut saw_exit = false;
        while Instant::now() < deadline {
            if let Ok(event) = rx.recv_timeout(Duration::from_millis(200)) {
                match event {
                    TerminalServiceEvent::Output(output) => {
                        if output.data.contains("kodex-terminal-test") {
                            saw_output = true;
                        }
                    }
                    TerminalServiceEvent::Exit(exit) => {
                        if exit.terminal_id == session.terminal_id {
                            saw_exit = true;
                        }
                    }
                    TerminalServiceEvent::Status(_) => {}
                }
            }
            if saw_output && saw_exit {
                break;
            }
        }

        assert!(saw_output, "expected terminal output event");
        assert!(saw_exit, "expected terminal exit event");
    }

    #[test]
    fn open_workspace_reuses_live_terminal() {
        let dir = tempdir().unwrap();
        let service = TerminalService::new();
        let first = service.open_workspace(dir.path(), 80, 24).unwrap();
        let second = service.open_workspace(dir.path(), 100, 30).unwrap();
        assert_eq!(first.terminal_id, second.terminal_id);
        service.shutdown_all();
    }

    #[test]
    fn open_workspace_new_creates_second_live_terminal() {
        let dir = tempdir().unwrap();
        let service = TerminalService::new();
        let first = service.open_workspace(dir.path(), 80, 24).unwrap();
        let second = service.open_workspace_new(dir.path(), 80, 24).unwrap();
        assert_ne!(first.terminal_id, second.terminal_id);
        assert_eq!(service.list_workspace(dir.path()).unwrap().len(), 2);
        service.shutdown_all();
    }

    #[test]
    fn shutdown_workspace_removes_sessions() {
        let dir = tempdir().unwrap();
        let service = TerminalService::new();
        let session = service.open_workspace(dir.path(), 80, 24).unwrap();
        assert_eq!(service.list_workspace(dir.path()).unwrap().len(), 1);
        service.shutdown_workspace(dir.path());
        assert!(service.list_workspace(dir.path()).unwrap().is_empty());
        let _ = service.terminate(&session.terminal_id);
    }
}
