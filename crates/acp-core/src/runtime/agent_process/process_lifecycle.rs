use super::super::ShutdownSignal;
use crate::events::SessionConfig;
use crate::mapping::append_runtime_event_log;
use futures::channel::mpsc as futures_mpsc;
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
use std::process::Child;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

pub(in crate::runtime::agent_process) fn io_other(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, error.to_string())
}

pub(in crate::runtime::agent_process) fn read_agent_stdout(
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

pub(in crate::runtime::agent_process) fn read_agent_stderr(
    stderr: std::process::ChildStderr,
    tx: mpsc::Sender<String>,
) {
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

pub(in crate::runtime::agent_process) fn write_agent_stdin(
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

pub(in crate::runtime::agent_process) fn trim_line_ending(line: &mut String) {
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
}

pub(in crate::runtime::agent_process) struct AgentChildGuard {
    child: Arc<Mutex<Option<Child>>>,
    #[cfg(windows)]
    _job: Option<WindowsKillOnDropJob>,
}

impl AgentChildGuard {
    pub(in crate::runtime::agent_process) fn new(
        child: Child,
        shutdown_signal: ShutdownSignal,
    ) -> Self {
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

    pub(in crate::runtime::agent_process) fn handle(&self) -> Arc<Mutex<Option<Child>>> {
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

pub(in crate::runtime) fn kill_child_handle(
    child: &Arc<Mutex<Option<Child>>>,
) -> anyhow::Result<()> {
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

pub(in crate::runtime::agent_process) async fn monitor_hidden_agent_child(
    child: Arc<Mutex<Option<Child>>>,
    stderr_rx: mpsc::Receiver<String>,
    log_config: Option<SessionConfig>,
    label: &'static str,
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
        stderr_rx
            .recv_timeout(Duration::from_millis(200))
            .unwrap_or_default()
    };
    let payload = if stderr.is_empty() {
        json!({
            "label": label,
            "success": success,
            "status": status.to_string(),
            "exitCode": status.code()
        })
    } else {
        json!({
            "label": label,
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
