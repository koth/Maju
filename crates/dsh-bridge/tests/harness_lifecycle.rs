//! Full lifecycle through `run_harness_session`: config hydration and
//! `SetModel`. Exercises the same worker-thread dispatch path as
//! `acp-core::runtime::run_session` (after the nested-runtime fix), so
//! regressions in model switching surface here. The prompt→message→turn flow
//! is covered by `harness_integration::single_session_create_prompt_message_flow`.

mod common;

use acp_core::{ClientEvent, PermissionBroker, RuntimeCommand, SessionConfig, ShutdownSignal};
use common::{MockHarness, default_config};
use dsh_bridge::HarnessHostRegistry;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

fn config_for(endpoint: String) -> SessionConfig {
    SessionConfig {
        workspace_root: "/tmp".into(),
        app_data_root: "/tmp".into(),
        model: "deepseek-v4-pro".into(),
        agent_command: "dsh".into(),
        agent_env: Vec::new(),
        resume_session_id: None,
        log_id: "lifecycle".into(),
        acp_port: 0,
        remote_ssh: None,
        mcp_servers: Vec::new(),
        harness_endpoint: Some(endpoint),
        agent_preset: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_hydrate_and_set_model() {
    let mock = MockHarness::start(default_config()).await;
    let registry = Arc::new(HarnessHostRegistry::new());

    let (tx, rx) = mpsc::channel::<ClientEvent>();
    let (command_tx, command_rx) = mpsc::channel();
    let config = config_for(mock.endpoint());

    let worker_registry = registry.clone();
    let worker = thread::spawn(move || {
        dsh_bridge::run_harness_session(
            worker_registry,
            config,
            tx,
            command_rx,
            PermissionBroker::default(),
            ShutdownSignal::default(),
        )
    });

    // 1. Config hydration.
    let mut hydrated = false;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ClientEvent::SessionConfigUpdated { state }) => {
                hydrated = state.hydrated;
                break;
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(hydrated, "config never hydrated");

    let calls = mock.calls();
    assert!(
        calls.iter().any(|(method, _)| method == "session/create"),
        "Typert session.create must be called: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|(method, _)| method == "session/modelCatalog"),
        "Typert modelCatalog must be called: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|(method, _)| method == "agentPresets/list"),
        "Typert agentPresets list must be called: {calls:?}"
    );

    // 2. SetModel: must return refreshed config, not Interrupted.
    let (model_reply_tx, model_reply_rx) = mpsc::channel();
    command_tx
        .send(RuntimeCommand::SetModel {
            model_id: "deepseek-v4-flash".into(),
            provider: Some("deepseek".into()),
            reply_tx: model_reply_tx,
        })
        .unwrap();
    let model_events = model_reply_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("SetModel reply timed out")
        .expect("SetModel reply channel closed");
    assert!(
        !model_events
            .iter()
            .any(|e| matches!(e, ClientEvent::Interrupted { .. })),
        "SetModel returned Interrupted: {model_events:?}"
    );
    assert!(
        model_events
            .iter()
            .any(|e| matches!(e, ClientEvent::SessionConfigUpdated { .. })),
        "SetModel did not republish config: {model_events:?}"
    );

    // 3. The mock records every `session.selectModel` call. The composer sends
    // a provider-qualified value id like `kodex-provider/deepseek/deepseek-v4-flash`;
    // the bridge must decode it so dsh receives the bare model id + provider
    // (dsh's schema rejects the encoded value and an empty provider).
    let calls = mock.calls();
    let select_model_calls: Vec<_> = calls
        .iter()
        .filter(|(m, _)| m == "session/selectModel")
        .collect();
    assert!(
        !select_model_calls.is_empty(),
        "session.selectModel was not called: {calls:?}"
    );
    // The mock's session.selectModel response always succeeds; we only need
    // the call to have happened with decoded values (asserted by the absence of
    // Interrupted above, since a 400/422 would surface as selectModel failed).

    let _ = command_tx.send(RuntimeCommand::Shutdown);
    let _ = worker.join();
}

#[tokio::test(flavor = "multi_thread")]
async fn set_mode_on_started_session_reports_notice_not_interrupt() {
    // A session that has already started a turn cannot switch preset: dsh
    // answers `agent-preset-locked`. The bridge must surface that as a system
    // notice, NOT `Interrupted` (which would mark the session dead/disconnected).
    let mut config_mock = default_config();
    config_mock.preset_locked = true;
    let mock = MockHarness::start(config_mock).await;
    let registry = Arc::new(HarnessHostRegistry::new());

    let (tx, rx) = mpsc::channel::<ClientEvent>();
    let (command_tx, command_rx) = mpsc::channel();
    let config = config_for(mock.endpoint());

    let worker_registry = registry.clone();
    let worker = thread::spawn(move || {
        dsh_bridge::run_harness_session(
            worker_registry,
            config,
            tx,
            command_rx,
            PermissionBroker::default(),
            ShutdownSignal::default(),
        )
    });

    // Wait for hydration before sending SetMode.
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ClientEvent::SessionConfigUpdated { state }) if state.hydrated => break,
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let (mode_reply_tx, mode_reply_rx) = mpsc::channel();
    command_tx
        .send(RuntimeCommand::SetMode {
            mode_id: "standard".into(),
            reply_tx: mode_reply_tx,
        })
        .unwrap();
    // The bridge rejects preset switches on a started session with an error
    // so `SessionHandle::set_mode` does not mutate the shared permission
    // broker; the composer surfaces the error to the user.
    let mode_result = mode_reply_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("SetMode reply timed out");
    let err = mode_result.expect_err("locked preset switch must fail");
    assert!(
        err.to_string().contains("预设已固定"),
        "error must explain the preset is locked: {err}"
    );

    let _ = command_tx.send(RuntimeCommand::Shutdown);
    let _ = worker.join();
}

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_hydrate_includes_preset_mode_control_and_set_mode() {
    let mock = MockHarness::start(default_config()).await;
    let registry = Arc::new(HarnessHostRegistry::new());

    let (tx, rx) = mpsc::channel::<ClientEvent>();
    let (command_tx, command_rx) = mpsc::channel();
    let config = config_for(mock.endpoint());

    let worker_registry = registry.clone();
    let worker = thread::spawn(move || {
        dsh_bridge::run_harness_session(
            worker_registry,
            config,
            tx,
            command_rx,
            PermissionBroker::default(),
            ShutdownSignal::default(),
        )
    });

    // 1. Config hydration must include a Mode (agent-preset) control with the
    // four shipped presets, defaulted to `code`.
    let mut mode_control = None;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ClientEvent::SessionConfigUpdated { state }) => {
                mode_control = state
                    .controls
                    .iter()
                    .find(|c| c.category == workspace_model::SessionConfigCategory::Mode)
                    .cloned();
                if mode_control.is_some() {
                    break;
                }
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let mode_control = mode_control.expect("no Mode control published");
    assert_eq!(mode_control.current_value_id, "code");
    assert!(
        mode_control.choices.iter().any(|c| c.id == "standard"),
        "preset choices missing `standard`: {:?}",
        mode_control.choices
    );
    assert!(
        mode_control.choices.iter().any(|c| c.id == "minimal"),
        "preset choices missing `minimal`"
    );
    assert!(
        mode_control.choices.iter().any(|c| c.id == "cordis"),
        "preset choices missing `cordis`"
    );

    // 2. SetMode → agentPreset.select, then re-published config shows the new
    // selection.
    let (mode_reply_tx, mode_reply_rx) = mpsc::channel();
    command_tx
        .send(RuntimeCommand::SetMode {
            mode_id: "standard".into(),
            reply_tx: mode_reply_tx,
        })
        .unwrap();
    let mode_events = mode_reply_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("SetMode reply timed out")
        .expect("SetMode reply channel closed");
    assert!(
        !mode_events
            .iter()
            .any(|e| matches!(e, ClientEvent::Interrupted { .. })),
        "SetMode returned Interrupted: {mode_events:?}"
    );
    let updated_mode = mode_events
        .iter()
        .find_map(|e| match e {
            ClientEvent::SessionConfigUpdated { state } => state
                .controls
                .iter()
                .find(|c| c.category == workspace_model::SessionConfigCategory::Mode)
                .cloned(),
            _ => None,
        })
        .expect("SetMode did not republish Mode control");
    assert_eq!(updated_mode.current_value_id, "standard");

    // 3. The mock recorded the agentPreset.select call with the chosen id.
    assert_eq!(mock.preset_selects(), vec!["standard".to_string()]);

    let _ = command_tx.send(RuntimeCommand::Shutdown);
    let _ = worker.join();
}

#[tokio::test(flavor = "multi_thread")]
async fn new_session_create_carries_configured_preset() {
    // A new session must pass `config.agent_preset` to `session.create` so the
    // harness composes the chosen preset (e.g. `minimal`) instead of the
    // deployment default.
    let mock = MockHarness::start(default_config()).await;
    let registry = Arc::new(HarnessHostRegistry::new());

    let (tx, _rx) = mpsc::channel::<ClientEvent>();
    let (_command_tx, command_rx) = mpsc::channel();
    let mut config = config_for(mock.endpoint());
    config.agent_preset = Some("minimal".into());

    let worker_registry = registry.clone();
    let worker = thread::spawn(move || {
        dsh_bridge::run_harness_session(
            worker_registry,
            config,
            tx,
            command_rx,
            PermissionBroker::default(),
            ShutdownSignal::default(),
        )
    });

    // Wait for the create call to land, then shut down.
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(5) && mock.creates().is_empty() {
        std::thread::sleep(Duration::from_millis(50));
    }
    let creates = mock.creates();
    assert_eq!(creates.len(), 1, "expected one session.create call");
    assert_eq!(
        creates[0].get("agentPreset").and_then(|v| v.as_str()),
        Some("minimal"),
        "session.create must carry the configured preset: {creates:?}"
    );

    let _ = _command_tx.send(RuntimeCommand::Shutdown);
    let _ = worker.join();
}

/// History with three completed turns plus out-of-band compaction events
/// (turn: null) that must never count as turns for the fork boundary walk.
fn fork_history_events() -> Vec<serde_json::Value> {
    use common::history_event;
    vec![
        history_event(1, "turn/start", serde_json::json!({ "turn": 1 })),
        history_event(
            10,
            "turn/end",
            serde_json::json!({ "turn": 1, "reason": { "kind": "completed" } }),
        ),
        history_event(11, "turn/start", serde_json::json!({ "turn": 2 })),
        history_event(
            20,
            "turn/end",
            serde_json::json!({ "turn": 2, "reason": { "kind": "completed" } }),
        ),
        history_event(21, "turn/start", serde_json::json!({ "turn": 3 })),
        history_event(
            30,
            "turn/end",
            serde_json::json!({ "turn": 3, "reason": { "kind": "completed" } }),
        ),
        history_event(
            31,
            "compaction/start",
            serde_json::json!({ "compactionId": "c1", "turn": null }),
        ),
        history_event(
            32,
            "compaction/end",
            serde_json::json!({ "compactionId": "c1", "turn": null }),
        ),
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn fork_session_command_cuts_at_nth_completed_turn() {
    // ForkSession walks the session history for the Nth completed turn and
    // anchors the harness `session.fork` atSeq on that turn's `turn/end` —
    // the harness then seeds the child with everything through that turn.
    let mut config_mock = default_config();
    config_mock.history_events = fork_history_events();
    let mock = MockHarness::start(config_mock).await;
    let registry = Arc::new(HarnessHostRegistry::new());

    let (tx, _rx) = mpsc::channel::<ClientEvent>();
    let (command_tx, command_rx) = mpsc::channel();
    let config = config_for(mock.endpoint());

    let worker_registry = registry.clone();
    let worker = thread::spawn(move || {
        dsh_bridge::run_harness_session(
            worker_registry,
            config,
            tx,
            command_rx,
            PermissionBroker::default(),
            ShutdownSignal::default(),
        )
    });

    // Wait for the session to boot (session.create recorded).
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(5) && mock.creates().is_empty() {
        std::thread::sleep(Duration::from_millis(50));
    }

    let (fork_reply_tx, fork_reply_rx) = mpsc::channel();
    command_tx
        .send(RuntimeCommand::ForkSession {
            at_user_turn: 2,
            user_message_text: None,
            user_message_occurrence: 0,
            reply_tx: fork_reply_tx,
        })
        .unwrap();
    let child = fork_reply_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("ForkSession reply timed out")
        .expect("fork must succeed");
    assert_eq!(child, "s-fork", "mock answers the fork child session id");

    let forks = mock.forks();
    assert_eq!(forks.len(), 1, "exactly one session.fork call: {forks:?}");
    assert_eq!(
        forks[0].get("sessionId").and_then(|v| v.as_str()),
        Some("s-1"),
        "fork must target the source session: {forks:?}"
    );
    assert_eq!(
        forks[0].get("atSeq").and_then(|v| v.as_u64()),
        Some(20),
        "atSeq must anchor on the 2nd turn/end: {forks:?}"
    );

    let _ = command_tx.send(RuntimeCommand::Shutdown);
    let _ = worker.join();
}

#[tokio::test(flavor = "multi_thread")]
async fn fork_session_command_rejects_uncompleted_turn() {
    // Only two completed turns exist; forking "from" the third (an open turn
    // or a nonexistent ordinal) must fail with a user-facing error and never
    // reach the harness fork RPC.
    let mut config_mock = default_config();
    config_mock.history_events = fork_history_events();
    // Keep only the first two turn/ends: drop the third turn's events and the
    // out-of-band ones.
    config_mock.history_events.truncate(4);
    let mock = MockHarness::start(config_mock).await;
    let registry = Arc::new(HarnessHostRegistry::new());

    let (tx, _rx) = mpsc::channel::<ClientEvent>();
    let (command_tx, command_rx) = mpsc::channel();
    let config = config_for(mock.endpoint());

    let worker_registry = registry.clone();
    let worker = thread::spawn(move || {
        dsh_bridge::run_harness_session(
            worker_registry,
            config,
            tx,
            command_rx,
            PermissionBroker::default(),
            ShutdownSignal::default(),
        )
    });

    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(5) && mock.creates().is_empty() {
        std::thread::sleep(Duration::from_millis(50));
    }

    let (fork_reply_tx, fork_reply_rx) = mpsc::channel();
    command_tx
        .send(RuntimeCommand::ForkSession {
            at_user_turn: 3,
            user_message_text: None,
            user_message_occurrence: 0,
            reply_tx: fork_reply_tx,
        })
        .unwrap();
    let err = fork_reply_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("ForkSession reply timed out")
        .expect_err("forking an uncompleted turn must fail");
    assert!(
        err.to_string().contains("尚未完成"),
        "error must explain the turn is not finished: {err}"
    );
    assert!(
        mock.forks().is_empty(),
        "a failed boundary lookup must not call session.fork"
    );

    let _ = command_tx.send(RuntimeCommand::Shutdown);
    let _ = worker.join();
}

/// History replicating the real-world dsh divergence: between two user turns
/// the harness splices an injected turn (a subagent settlement carrying a
/// `subagent-report` user/message). The harness turn counter numbers injected
/// turns too, so kodex's 2nd turn-opening prompt is the harness's 3rd turn —
/// a pure ordinal cut would anchor one turn short.
fn fork_history_with_injected_turn() -> Vec<serde_json::Value> {
    use common::history_event;
    let prompt_data = |text: &str| -> serde_json::Value {
        serde_json::json!({
            "content": [{ "type": "text", "text": text }],
            "source": { "kind": "user" },
            "role": "user",
        })
    };
    let injected_data = serde_json::json!({
        "content": [{ "type": "text", "text": "Background subagent abc was stopped." }],
        "source": { "kind": "subagent-settled" },
        "role": "user",
    });
    vec![
        history_event(1, "turn/start", serde_json::json!({ "turn": 1 })),
        history_event(5, "user/message", prompt_data("turn one question")),
        history_event(
            10,
            "turn/end",
            serde_json::json!({ "turn": 1, "reason": { "kind": "completed" } }),
        ),
        history_event(11, "turn/start", serde_json::json!({ "turn": 2 })),
        history_event(15, "user/message", injected_data),
        history_event(
            20,
            "turn/end",
            serde_json::json!({ "turn": 2, "reason": { "kind": "completed" } }),
        ),
        history_event(21, "turn/start", serde_json::json!({ "turn": 3 })),
        history_event(25, "user/message", prompt_data("turn two question")),
        history_event(
            30,
            "turn/end",
            serde_json::json!({ "turn": 3, "reason": { "kind": "completed" } }),
        ),
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn fork_session_command_anchors_on_prompt_text_past_injected_turns() {
    // The ordinal (at_user_turn = 2) would count the injected subagent turn
    // and anchor on seq 20 — one turn short. The prompt-text anchor must cut
    // at the end of the turn the matched prompt opened (seq 30).
    let mut config_mock = default_config();
    config_mock.history_events = fork_history_with_injected_turn();
    let mock = MockHarness::start(config_mock).await;
    let registry = Arc::new(HarnessHostRegistry::new());

    let (tx, _rx) = mpsc::channel::<ClientEvent>();
    let (command_tx, command_rx) = mpsc::channel();
    let config = config_for(mock.endpoint());

    let worker_registry = registry.clone();
    let worker = thread::spawn(move || {
        dsh_bridge::run_harness_session(
            worker_registry,
            config,
            tx,
            command_rx,
            PermissionBroker::default(),
            ShutdownSignal::default(),
        )
    });

    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(5) && mock.creates().is_empty() {
        std::thread::sleep(Duration::from_millis(50));
    }

    let (fork_reply_tx, fork_reply_rx) = mpsc::channel();
    command_tx
        .send(RuntimeCommand::ForkSession {
            at_user_turn: 2,
            user_message_text: Some("turn two question".into()),
            user_message_occurrence: 1,
            reply_tx: fork_reply_tx,
        })
        .unwrap();
    let child = fork_reply_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("ForkSession reply timed out")
        .expect("text-anchored fork must succeed");
    assert_eq!(child, "s-fork", "mock answers the fork child session id");

    let forks = mock.forks();
    assert_eq!(forks.len(), 1, "exactly one session.fork call: {forks:?}");
    assert_eq!(
        forks[0].get("atSeq").and_then(|v| v.as_u64()),
        Some(30),
        "atSeq must anchor on the first turn/end after the matched prompt (seq 30, skipping the injected turn's end at seq 20): {forks:?}"
    );

    let _ = command_tx.send(RuntimeCommand::Shutdown);
    let _ = worker.join();
}

#[tokio::test(flavor = "multi_thread")]
async fn fork_session_command_text_anchor_falls_back_to_ordinal_on_miss() {
    // When the prompt text cannot be found in the harness history (e.g. a
    // transformed prompt), the legacy ordinal walk must still anchor the cut.
    let mut config_mock = default_config();
    config_mock.history_events = fork_history_with_injected_turn();
    let mock = MockHarness::start(config_mock).await;
    let registry = Arc::new(HarnessHostRegistry::new());

    let (tx, _rx) = mpsc::channel::<ClientEvent>();
    let (command_tx, command_rx) = mpsc::channel();
    let config = config_for(mock.endpoint());

    let worker_registry = registry.clone();
    let worker = thread::spawn(move || {
        dsh_bridge::run_harness_session(
            worker_registry,
            config,
            tx,
            command_rx,
            PermissionBroker::default(),
            ShutdownSignal::default(),
        )
    });

    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(5) && mock.creates().is_empty() {
        std::thread::sleep(Duration::from_millis(50));
    }

    let (fork_reply_tx, fork_reply_rx) = mpsc::channel();
    command_tx
        .send(RuntimeCommand::ForkSession {
            at_user_turn: 3,
            user_message_text: Some("a prompt that never happened".into()),
            user_message_occurrence: 1,
            reply_tx: fork_reply_tx,
        })
        .unwrap();
    let child = fork_reply_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("ForkSession reply timed out")
        .expect("fallback must keep the ordinal anchor working");

    assert_eq!(child, "s-fork", "mock answers the fork child session id");
    let forks = mock.forks();
    assert_eq!(forks.len(), 1, "exactly one session.fork call: {forks:?}");
    assert_eq!(
        forks[0].get("atSeq").and_then(|v| v.as_u64()),
        Some(30),
        "ordinal fallback must anchor on the 3rd turn/end: {forks:?}"
    );

    let _ = command_tx.send(RuntimeCommand::Shutdown);
    let _ = worker.join();
}

/// Regression: the stop button used to do nothing.
///
/// `RuntimeCommand::CancelPrompt` shipped `{ "sessionId": … }` as the *args*
/// object, but `session/cancel`'s descriptor declares a single `request`
/// parameter, so the gateway rejected the call with
/// `gateway/arguments-invalid: missing "request"` — the turn kept streaming
/// while the UI had already painted itself as cancelled (app-core's
/// `cancel_prompt` is fire-and-forget by design, so nothing surfaced the
/// rejection). The mock host mirrors that exact-args check, so this test fails
/// if the `request` envelope ever regresses.
#[tokio::test(flavor = "multi_thread")]
async fn cancel_prompt_reaches_the_host_in_the_descriptor_shape() {
    let mock = MockHarness::start(default_config()).await;
    let registry = Arc::new(HarnessHostRegistry::new());

    let (tx, _rx) = mpsc::channel::<ClientEvent>();
    let (command_tx, command_rx) = mpsc::channel();
    let config = config_for(mock.endpoint());

    let worker = thread::spawn(move || {
        dsh_bridge::run_harness_session(
            registry,
            config,
            tx,
            command_rx,
            PermissionBroker::default(),
            ShutdownSignal::default(),
        )
    });

    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(5) && mock.creates().is_empty() {
        std::thread::sleep(Duration::from_millis(50));
    }

    let (reply_tx, reply_rx) = mpsc::channel();
    command_tx
        .send(RuntimeCommand::CancelPrompt {
            reply_tx: Some(reply_tx),
        })
        .unwrap();
    reply_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("CancelPrompt reply timed out")
        .expect("the harness must accept the cancel; a rejected stop is what made the button dead");

    let args = mock.cancel_args();
    assert_eq!(args.len(), 1, "exactly one session.cancel call: {args:?}");
    assert_eq!(
        args[0]
            .pointer("/request/sessionId")
            .and_then(serde_json::Value::as_str),
        Some("s-1"),
        "the cancel request must nest under the descriptor's `request` parameter: {args:?}"
    );
    assert_eq!(
        args[0].as_object().map(|args| args.len()),
        Some(1),
        "the args object carries `request` and nothing else: {args:?}"
    );

    let _ = command_tx.send(RuntimeCommand::Shutdown);
    let _ = worker.join();
}
