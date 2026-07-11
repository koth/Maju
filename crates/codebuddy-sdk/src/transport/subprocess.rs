use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc, oneshot};
use crate::error::{SdkError, SdkResult};
use crate::options::SessionOptions;
/// Capacity of the stderr ring buffer used for crash diagnostics.
const STDERR_RING_CAP: usize = 200;
/// A spawned CodeBuddy CLI child process speaking stream-json over stdio.
pub struct SubprocessTransport {
    child: Mutex<Option<Child>>,
    stdin: Mutex<Option<tokio::process::ChildStdin>>,
    stderr_ring: Arc<Mutex<std::collections::VecDeque<String>>>,
    messages_rx: Mutex<Option<mpsc::Receiver<SdkResult<serde_json::Value>>>>,
    closed: Arc<std::sync::atomic::AtomicBool>,
    cli_path: PathBuf,
}
impl SubprocessTransport {
    pub fn spawn(opts: &SessionOptions, cli_path: PathBuf) -> SdkResult<(Self, oneshot::Receiver<()>)> {
        let mut cmd = Command::new(&cli_path);
        for arg in cli_args(opts) {
            cmd.arg(arg);
        }
        if let Some(cwd) = &opts.cwd {
            cmd.current_dir(cwd);
        }
        let env = crate::binary::build_child_env(&opts.env, &cli_path, env!("CARGO_PKG_VERSION"));
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            // hide the console window on Windows
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn().map_err(SdkError::Spawn)?;
        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");
        let stderr_ring = Arc::new(Mutex::new(std::collections::VecDeque::with_capacity(STDERR_RING_CAP)));
        let (msg_tx, msg_rx) = mpsc::channel::<SdkResult<serde_json::Value>>(1024);
        let (done_tx, done_rx) = oneshot::channel::<()>();
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stdout_closed = closed.clone();
        let stderr_ring_clone = stderr_ring.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            let mut got_any = false;
            loop {
                match reader.next_line().await {
                    Ok(Some(line)) => {
                        got_any = true;
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<serde_json::Value>(trimmed) {
                            Ok(v) => {
                                if msg_tx.send(Ok(v)).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                let _ = msg_tx
                                    .send(Err(SdkError::Json(e)))
                                    .await;
                            }
                        }
                    }
                    Ok(None) => {
                        let _ = done_tx.send(());
                        if !got_any && !stdout_closed.load(std::sync::atomic::Ordering::SeqCst) {
                        let stderr_snap = snapshot_ring(&stderr_ring_clone).await;
                            let _ = msg_tx
                                .send(Err(SdkError::CliNoOutput {
                                    exit_code: None,
                                    stderr: stderr_snap,
                                }))
                                .await;
                        }
                        break;
                    }
                    Err(e) => {
                        let _ = msg_tx.send(Err(SdkError::Io(e))).await;
                        break;
                    }
                }
            }
        });
        let stderr_ring2 = stderr_ring.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let mut ring = stderr_ring2.lock().await;
                if ring.len() >= STDERR_RING_CAP {
                    ring.pop_front();
                }
                ring.push_back(line);
            }
        });
        let transport = Self {
            child: Mutex::new(Some(child)),
            stdin: Mutex::new(Some(stdin)),
            stderr_ring,
            messages_rx: Mutex::new(Some(msg_rx)),
            closed: closed.clone(),
            cli_path,
        };
        Ok((transport, done_rx))
    }
    pub fn cli_path(&self) -> &std::path::Path {
        &self.cli_path
    }
    pub async fn write_line(&self, line: &str) -> SdkResult<()> {
        let mut guard = self.stdin.lock().await;
        let stdin = guard.as_mut().ok_or(SdkError::StdinClosed)?;
        stdin.write_all(line.as_bytes()).await.map_err(|_| SdkError::StdinClosed)?;
        stdin.write_all(b"\n").await.map_err(|_| SdkError::StdinClosed)?;
        stdin.flush().await.map_err(|_| SdkError::StdinClosed)?;
        Ok(())
    }
    pub async fn write_json(&self, value: &serde_json::Value) -> SdkResult<()> {
        let line = serde_json::to_string(value)?;
        self.write_line(&line).await
    }
    pub async fn take_messages(&self) -> Option<mpsc::Receiver<SdkResult<serde_json::Value>>> {
        self.messages_rx.lock().await.take()
    }
    pub async fn stderr_snapshot(&self) -> String {
        snapshot_ring(&self.stderr_ring).await
    }
    pub async fn close(&self) {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        {
            let mut guard = self.stdin.lock().await;
            guard.take();
        }
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), child.wait()).await;
        }
    }
}
async fn snapshot_ring(ring: &Arc<Mutex<std::collections::VecDeque<String>>>) -> String {
    let guard = ring.lock().await;
    guard.iter().cloned().collect::<Vec<_>>().join("\n")
}

/// Build the CLI argv (excluding the binary path) from session options.
///
/// Kept pure so unit tests can lock the critical `--tools` contract without
/// spawning a real CodeBuddy process.
fn cli_args(opts: &SessionOptions) -> Vec<String> {
    // `--print` is mandatory: per `codebuddy --help`, `--input-format`/
    // `--output-format` "only work with --print", and without it the
    // CLI starts an *interactive* TUI session that ignores stream-json
    // framing. That made our `initialize` control_request sit unread
    // on stdin → 60s control timeout with empty stderr. The TS SDK
    // sidesteps this by spawning the headless `dist/codebuddy-headless.js`
    // variant; the `.exe` we resolve is the interactive binary, so we
    // must opt into non-interactive stream-json mode explicitly.
    let mut args = vec![
        "--print".to_string(),
        "--input-format=stream-json".to_string(),
        "--output-format=stream-json".to_string(),
        "--verbose".to_string(),
    ];
    if let Some(model) = &opts.model {
        args.push("--model".to_string());
        args.push(model.clone());
    }
    if let Some(mode) = &opts.permission_mode {
        args.push("--permission-mode".to_string());
        args.push(mode.clone());
    }
    // Built-in tool filter. Empty list → `--tools ""` disables ALL built-ins
    // (CLI help: `Use "" to disable all`). The reverse proxy always sets
    // `tools: Some(vec![])` so CodeBuddy cannot execute Bash/Edit/Read
    // itself; only the in-process MCP proxy tools remain available.
    if let Some(tools) = &opts.tools {
        args.push("--tools".to_string());
        args.push(tools.join(","));
    }
    if let Some(max_turns) = opts.max_turns {
        args.push("--max-turns".to_string());
        args.push(max_turns.to_string());
    }
    if let Some(sid) = &opts.session_id {
        args.push("--session-id".to_string());
        args.push(sid.clone());
    }
    if let Some(sp) = &opts.system_prompt {
        args.push("--system-prompt".to_string());
        args.push(sp.clone());
    }
    args.push("--setting-sources".to_string());
    args.push("none".to_string());
    args.push("--include-partial-messages".to_string());
    args
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn spawn_missing_binary_returns_cli_not_found() {
        let opts = SessionOptions::default();
        let err = SubprocessTransport::spawn(&opts, PathBuf::from("/nonexistent/codebuddy"))
            .err();
        assert!(matches!(err, Some(SdkError::Spawn(_))), "got {err:?}");
    }

    #[test]
    fn empty_tools_list_disables_all_builtins() {
        // Contract parity with TS/Python: `tools: []` → `--tools ""`.
        let mut opts = SessionOptions::default();
        opts.tools = Some(Vec::new());
        let args = cli_args(&opts);
        let tools_idx = args.iter().position(|a| a == "--tools").expect("--tools present");
        assert_eq!(args.get(tools_idx + 1).map(String::as_str), Some(""));
    }

    #[test]
    fn tools_none_omits_flag() {
        let opts = SessionOptions::default();
        let args = cli_args(&opts);
        assert!(!args.iter().any(|a| a == "--tools"));
    }

    #[test]
    fn tools_list_is_comma_joined() {
        let mut opts = SessionOptions::default();
        opts.tools = Some(vec!["Bash".into(), "Read".into()]);
        let args = cli_args(&opts);
        let tools_idx = args.iter().position(|a| a == "--tools").expect("--tools present");
        assert_eq!(args.get(tools_idx + 1).map(String::as_str), Some("Bash,Read"));
    }
}
