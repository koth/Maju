//! Live turn-end probe against a REAL `dsh web` host.
//!
//! Diagnosis tool for "turn completion puts the UI into its 会话已断开 state":
//! it sends one tiny prompt to a live harness and prints every `ClientEvent`
//! in arrival order with millisecond timestamps, so the order of the turn's
//! `TurnFinished` (durable `turn/end`) and any `Interrupted` (the
//! `api-session/status(<id>, false)` idle transition, `api-session/error`, or
//! a stream error) is directly observable. The `Interrupted` reason string
//! names the exact source.
//!
//! Run against a live host (endpoint + token from `~/.kodex/logs/app.log`):
//!
//! ```sh
//! DSH_HARNESS_ENDPOINT='http://127.0.0.1:PORT/?token=…' \
//! DSH_PROBE_PROMPT='只回复:ok' \
//!   cargo test -p dsh-bridge --test live_turn_probe -- --ignored --nocapture
//! ```
//!
//! The test creates its own throwaway session on the host and is `#[ignore]`d
//! so it never runs in CI. It panics (failing loudly) when a turn that ended
//! cleanly also produced an `Interrupted` — the exact regression reported for
//! the composer's disconnected state.

use acp_core::{ClientEvent, PermissionBroker, RuntimeCommand, SessionConfig, ShutdownSignal};
use dsh_bridge::HarnessHostRegistry;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use workspace_model::UserPromptContent;

fn short(event: &ClientEvent) -> String {
    match event {
        ClientEvent::MessageChunk { role, content } => {
            format!("MessageChunk({role:?}, {:?})", content.chars().take(40).collect::<String>())
        }
        ClientEvent::ToolMessageChunk { id, content } => {
            format!("ToolMessageChunk({id}, {:?})", content.chars().take(30).collect::<String>())
        }
        ClientEvent::TurnFinished { stop_reason, detail } => {
            format!("TurnFinished(stop_reason={stop_reason:?}, detail={detail:?})")
        }
        ClientEvent::Interrupted { reason } => format!("Interrupted({reason:?})"),
        ClientEvent::ToolStarted { id, name, .. } => format!("ToolStarted({id}, {name})"),
        ClientEvent::ToolCompleted { id, .. } => format!("ToolCompleted({id})"),
        ClientEvent::ToolFailed { id, error, .. } => format!("ToolFailed({id}, {error:?})"),
        ClientEvent::SessionStarted { session_id } => format!("SessionStarted({session_id})"),
        other => format!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a live dsh web host; set DSH_HARNESS_ENDPOINT"]
async fn live_turn_end_must_not_interrupt() {
    let endpoint = std::env::var("DSH_HARNESS_ENDPOINT")
        .expect("set DSH_HARNESS_ENDPOINT to the live harness endpoint (with token)");
    let prompt_text =
        std::env::var("DSH_PROBE_PROMPT").unwrap_or_else(|_| "只回复:ok".to_string());

    let config = SessionConfig {
        workspace_root: std::env::temp_dir().to_string_lossy().into_owned(),
        app_data_root: std::env::temp_dir().to_string_lossy().into_owned(),
        model: String::new(),
        agent_command: "dsh".into(),
        agent_env: Vec::new(),
        resume_session_id: None,
        log_id: "live-turn-probe".into(),
        acp_port: 0,
        remote_ssh: None,
        mcp_servers: Vec::new(),
        harness_endpoint: Some(endpoint),
        agent_preset: None,
    };

    let registry = Arc::new(HarnessHostRegistry::new());
    let (tx_events, rx_events) = mpsc::channel::<ClientEvent>();
    let (tx_commands, rx_commands) = mpsc::channel();
    let shutdown = ShutdownSignal::default();

    let worker_registry = registry.clone();
    let worker_shutdown = shutdown.clone();
    let worker = std::thread::spawn(move || {
        dsh_bridge::run_harness_session(
            worker_registry,
            config,
            tx_events,
            rx_commands,
            PermissionBroker::default(),
            worker_shutdown,
        )
    });

    // Give the session boot a moment, then send the probe prompt. A fresh
    // prompt carries no `accepted_tx` (that slot marks a steer).
    std::thread::sleep(Duration::from_millis(1500));
    tx_commands
        .send(RuntimeCommand::SendPrompt {
            prompt: vec![UserPromptContent::text(prompt_text)],
            accepted_tx: None,
        })
        .expect("command channel open");
    println!(">>> probe prompt sent; draining events");

    let start = Instant::now();
    let mut turn_done_at: Option<Instant> = None;
    let mut interrupted: Vec<String> = Vec::new();
    let mut saw_turn_end = false;

    // Drain until turn end, then keep watching a few more seconds for the
    // late `Interrupted` the report is about.
    while start.elapsed() < Duration::from_secs(180) {
        if let Some(at) = turn_done_at
            && at.elapsed() > Duration::from_secs(5)
        {
            break;
        }
        match rx_events.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => {
                let at = start.elapsed();
                println!("[{:>8}ms] {}", at.as_millis(), short(&event));
                match &event {
                    ClientEvent::TurnFinished { .. } => {
                        saw_turn_end = true;
                        turn_done_at.get_or_insert(Instant::now());
                    }
                    ClientEvent::Interrupted { reason } => {
                        interrupted.push(reason.clone());
                        turn_done_at.get_or_insert(Instant::now());
                    }
                    _ => {}
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = tx_commands.send(RuntimeCommand::Shutdown);
    let _ = worker.join();

    println!(
        ">>> summary: saw_turn_end={saw_turn_end} interrupted={interrupted:?}"
    );
    assert!(
        interrupted.is_empty(),
        "a cleanly finished turn also produced Interrupted: {interrupted:?}"
    );
    assert!(saw_turn_end, "turn never ended within the probe window");
}
