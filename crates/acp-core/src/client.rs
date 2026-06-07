use crate::events::{ClientEvent, SessionConfig};
use crate::runtime::{PermissionBroker, RuntimeCommand, ShutdownSignal, run_session};
use anyhow::anyhow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use workspace_model::{PermissionInputResponse, UserPromptContent};

const MAX_READY_EVENTS_PER_COLLECT: usize = 32;

pub struct PromptTask {
    events: Vec<ClientEvent>,
    finished: bool,
}

#[derive(Debug)]
pub struct SessionHandle {
    pub id: String,
    event_rx: Receiver<ClientEvent>,
    command_tx: mpsc::Sender<RuntimeCommand>,
    worker: Option<thread::JoinHandle<anyhow::Result<()>>>,
    is_alive: Arc<AtomicBool>,
    last_error: Arc<Mutex<Option<String>>>,
    permission_broker: PermissionBroker,
    shutdown_signal: ShutdownSignal,
}

impl SessionHandle {
    pub fn start(config: SessionConfig) -> anyhow::Result<Self> {
        let (event_tx, event_rx) = mpsc::channel();
        let (command_tx, command_rx) = mpsc::channel();

        let is_alive = Arc::new(AtomicBool::new(true));
        let last_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let permission_broker = PermissionBroker::default();
        let shutdown_signal = ShutdownSignal::default();

        let worker_alive = is_alive.clone();
        let worker_error = last_error.clone();
        let worker_permission_broker = permission_broker.clone();
        let worker_shutdown_signal = shutdown_signal.clone();
        let worker = thread::spawn(move || {
            let result = run_session(
                config,
                event_tx,
                command_rx,
                worker_permission_broker,
                worker_shutdown_signal,
            );
            worker_alive.store(false, Ordering::Release);
            if let Err(ref err) = result {
                if let Ok(mut guard) = worker_error.lock() {
                    *guard = Some(err.to_string());
                }
            }
            result
        });

        Ok(Self {
            id: String::new(),
            event_rx,
            command_tx,
            worker: Some(worker),
            is_alive,
            last_error,
            permission_broker,
            shutdown_signal,
        })
    }

    pub fn shutdown(&mut self) {
        let _ = self.command_tx.send(RuntimeCommand::Shutdown);
        self.permission_broker.cancel_all().ok();
        self.shutdown_signal.request_shutdown();
        let _ = self.worker.take();
    }

    pub fn is_alive(&self) -> bool {
        self.is_alive.load(Ordering::Acquire)
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|g| g.clone())
    }

    pub fn send_prompt(&mut self, prompt: impl Into<String>) -> anyhow::Result<Vec<ClientEvent>> {
        let mut task =
            self.send_prompt_content_async(vec![UserPromptContent::text(prompt.into())])?;
        let mut events = Vec::new();
        while !task.is_finished() {
            events.extend(task.wait_for_events(self)?);
        }
        events.extend(task.into_events());
        self.update_session_id(&events);
        Ok(events)
    }

    pub fn send_prompt_async(&mut self, prompt: impl Into<String>) -> anyhow::Result<PromptTask> {
        self.send_prompt_content_async(vec![UserPromptContent::text(prompt.into())])
    }

    pub fn send_prompt_content_async(
        &mut self,
        prompt: Vec<UserPromptContent>,
    ) -> anyhow::Result<PromptTask> {
        self.command_tx
            .send(RuntimeCommand::SendPrompt(prompt))
            .map_err(|_| anyhow!("ACP command channel closed"))?;

        Ok(PromptTask {
            events: Vec::new(),
            finished: false,
        })
    }

    pub fn set_config_option(
        &mut self,
        config_id: impl Into<String>,
        value_id: impl Into<String>,
        provider: Option<String>,
    ) -> anyhow::Result<Vec<ClientEvent>> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.command_tx
            .send(RuntimeCommand::SetConfigOption {
                config_id: config_id.into(),
                value_id: value_id.into(),
                provider,
                reply_tx,
            })
            .map_err(|_| anyhow!("ACP command channel closed"))?;
        reply_rx
            .recv()
            .map_err(|_| anyhow!("ACP command reply channel closed"))?
    }

    pub fn set_mode(&mut self, mode_id: impl Into<String>) -> anyhow::Result<Vec<ClientEvent>> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.command_tx
            .send(RuntimeCommand::SetMode {
                mode_id: mode_id.into(),
                reply_tx,
            })
            .map_err(|_| anyhow!("ACP command channel closed"))?;
        reply_rx
            .recv()
            .map_err(|_| anyhow!("ACP command reply channel closed"))?
    }

    pub fn set_model(
        &mut self,
        model_id: impl Into<String>,
        provider: Option<String>,
    ) -> anyhow::Result<Vec<ClientEvent>> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.command_tx
            .send(RuntimeCommand::SetModel {
                model_id: model_id.into(),
                provider,
                reply_tx,
            })
            .map_err(|_| anyhow!("ACP command channel closed"))?;
        reply_rx
            .recv()
            .map_err(|_| anyhow!("ACP command reply channel closed"))?
    }

    pub fn resolve_permission(
        &self,
        request_id: &str,
        option_id: Option<String>,
        guidance: Option<String>,
        input_response: Option<PermissionInputResponse>,
    ) -> anyhow::Result<bool> {
        self.permission_broker
            .resolve(request_id, option_id, guidance, input_response)
    }

    pub fn resolve_codebuddy_interruption(
        &self,
        tool_call_id: &str,
        decision: &str,
    ) -> anyhow::Result<()> {
        if self.id.is_empty() {
            return Err(anyhow!("ACP session id is not available yet"));
        }

        let (reply_tx, reply_rx) = mpsc::channel();
        self.command_tx
            .send(RuntimeCommand::ResolveCodeBuddyInterruption {
                session_id: self.id.clone(),
                tool_call_id: tool_call_id.to_string(),
                decision: decision.to_string(),
                reply_tx,
            })
            .map_err(|_| anyhow!("ACP command channel closed"))?;
        reply_rx
            .recv()
            .map_err(|_| anyhow!("ACP command reply channel closed"))?
    }

    pub fn cancel_prompt(&self) -> anyhow::Result<()> {
        self.permission_broker.cancel_all()?;
        let (reply_tx, reply_rx) = mpsc::channel();
        self.command_tx
            .send(RuntimeCommand::CancelPrompt { reply_tx })
            .map_err(|_| anyhow!("ACP command channel closed"))?;
        reply_rx
            .recv()
            .map_err(|_| anyhow!("ACP command reply channel closed"))?
    }

    pub fn set_permission_mode(&self, mode_id: &str) -> anyhow::Result<()> {
        self.permission_broker.set_mode(mode_id)
    }

    pub fn update_session_id(&mut self, events: &[ClientEvent]) {
        if let Some(session_id) = events.iter().find_map(|event| match event {
            ClientEvent::SessionStarted { session_id } => Some(session_id.clone()),
            _ => None,
        }) {
            self.id = session_id;
        }
    }

    fn recv_event(&mut self) -> anyhow::Result<ClientEvent> {
        self.event_rx
            .recv()
            .map_err(|_| anyhow!("ACP event channel closed unexpectedly"))
    }

    fn try_recv_event(&mut self) -> Result<ClientEvent, TryRecvError> {
        self.event_rx.try_recv()
    }

    /// Drain all buffered events from the channel without processing them.
    /// Used to discard session/load replay events before sending a real prompt.
    pub fn drain_events(&mut self) {
        while self.event_rx.try_recv().is_ok() {}
    }

    pub fn collect_pending_events(&mut self) -> Vec<ClientEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.try_recv_event() {
            events.push(event);
        }
        self.update_session_id(&events);
        events
    }
}

impl PromptTask {
    pub fn wait_for_events(
        &mut self,
        session: &mut SessionHandle,
    ) -> anyhow::Result<Vec<ClientEvent>> {
        if !self.finished && self.events.is_empty() {
            let event = session.recv_event()?;
            self.finished = matches!(
                event,
                ClientEvent::TurnFinished { .. } | ClientEvent::Interrupted { .. }
            );
            self.events.push(event);
        }

        self.collect_ready_events(session)
    }

    pub fn collect_ready_events(
        &mut self,
        session: &mut SessionHandle,
    ) -> anyhow::Result<Vec<ClientEvent>> {
        if !self.finished {
            match session.try_recv_event() {
                Ok(event) => {
                    self.finished = matches!(
                        event,
                        ClientEvent::TurnFinished { .. } | ClientEvent::Interrupted { .. }
                    );
                    self.events.push(event);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    // Worker thread died — synthesize Interrupted so the UI can react
                    let reason = session
                        .last_error()
                        .unwrap_or_else(|| "ACP subprocess exited unexpectedly".to_string());
                    self.events.push(ClientEvent::Interrupted { reason });
                    self.finished = true;
                }
            }
        }

        while self.events.len() < MAX_READY_EVENTS_PER_COLLECT {
            let Ok(event) = session.try_recv_event() else {
                break;
            };
            self.finished = self.finished
                || matches!(
                    event,
                    ClientEvent::TurnFinished { .. } | ClientEvent::Interrupted { .. }
                );
            self.events.push(event);
        }

        session.update_session_id(&self.events);

        let mut ready = Vec::new();
        std::mem::swap(&mut ready, &mut self.events);
        Ok(coalesce_ready_events(ready))
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub fn into_events(self) -> Vec<ClientEvent> {
        self.events
    }
}

fn coalesce_ready_events(events: Vec<ClientEvent>) -> Vec<ClientEvent> {
    let mut coalesced: Vec<ClientEvent> = Vec::with_capacity(events.len());

    for event in events {
        match event {
            ClientEvent::MessageChunk { role, content } => {
                if let Some(ClientEvent::MessageChunk {
                    role: previous_role,
                    content: previous_content,
                }) = coalesced.last_mut()
                    && *previous_role == role
                {
                    previous_content.push_str(&content);
                    continue;
                }
                coalesced.push(ClientEvent::MessageChunk { role, content });
            }
            ClientEvent::ToolMessageChunk { id, content } => {
                if let Some(ClientEvent::ToolMessageChunk {
                    id: previous_id,
                    content: previous_content,
                }) = coalesced.last_mut()
                    && *previous_id == id
                {
                    previous_content.push_str(&content);
                    continue;
                }
                coalesced.push(ClientEvent::ToolMessageChunk { id, content });
            }
            ClientEvent::ToolProgress { id, content } => {
                if let Some(ClientEvent::ToolProgress {
                    id: previous_id,
                    content: previous_content,
                }) = coalesced.last_mut()
                    && *previous_id == id
                {
                    previous_content.push_str(&content);
                    continue;
                }
                coalesced.push(ClientEvent::ToolProgress { id, content });
            }
            other => coalesced.push(other),
        }
    }

    coalesced
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}
