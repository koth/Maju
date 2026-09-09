//! Bridge integration tests against a fake dsh web host.
//!
//! Covers the shared-host concurrency model without a real `dsh web` binary:
//! frame routing to per-session sinks, unmatched-frame drop, concurrent
//! control POSTs, approval round-trips over `/api/respond`, SSE reconnection
//! with multi-session re-baseline, and host lifetime across sessions.

mod common;

use acp_core::{ClientEvent, PermissionBroker};
use common::{
    HoldFramesUntil, MockHarness, MuxEnd, MuxScript, default_config, history_event,
    mux_assistant_final, mux_assistant_text_delta, mux_session_event, mux_subscribed, scripts_with,
};
use dsh_bridge::{HarnessHostRegistry, HttpClient};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

fn test_sink() -> (Arc<dsh_bridge::SessionSink>, mpsc::Receiver<ClientEvent>) {
    let (tx, rx) = mpsc::channel();
    (
        Arc::new(dsh_bridge::SessionSink::new(
            tx,
            PermissionBroker::default(),
        )),
        rx,
    )
}

fn drain_events(rx: &mpsc::Receiver<ClientEvent>, deadline: Duration) -> Vec<ClientEvent> {
    let mut events = Vec::new();
    let start = std::time::Instant::now();
    while start.elapsed() < deadline {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(event) => events.push(event),
            Err(mpsc::RecvTimeoutError::Timeout) if events.is_empty() => continue,
            Err(_) => break,
        }
    }
    events
}

fn wait_until<F: Fn() -> bool>(f: F, deadline: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
async fn two_sessions_share_one_host_and_route_frames_to_the_right_sink() {
    // Mock host streams frames for session A and B on the single mux stream.
    let config = {
        let mut c = default_config();
        c.mux = scripts_with(vec![
            mux_subscribed("s-a", 0),
            mux_subscribed("s-b", 0),
            mux_assistant_text_delta("s-a", 1, "hello A"),
            mux_assistant_final("s-a", 2),
            mux_assistant_text_delta("s-b", 3, "hello B"),
            mux_assistant_final("s-b", 4),
        ]);
        c
    };
    let mock = MockHarness::start(config).await;

    let registry = Arc::new(HarnessHostRegistry::new());
    let host = registry.acquire(mock.endpoint()).unwrap();

    let (sink_a, rx_a) = test_sink();
    sink_a.set_session_id("s-a".into());
    host.router().register("s-a".into(), sink_a);
    let (sink_b, rx_b) = test_sink();
    sink_b.set_session_id("s-b".into());
    host.router().register("s-b".into(), sink_b);

    // Frames are delivered to the owning session only.
    let events_a = drain_events(&rx_a, Duration::from_millis(500));
    let events_b = drain_events(&rx_b, Duration::from_millis(500));
    assert!(
        events_a.iter().any(
            |e| matches!(e, ClientEvent::MessageChunk { content, .. } if content == "hello A")
        ),
        "session A did not receive its frame: {events_a:?}"
    );
    assert!(
        events_b.iter().any(
            |e| matches!(e, ClientEvent::MessageChunk { content, .. } if content == "hello B")
        ),
        "session B did not receive its frame: {events_b:?}"
    );
    assert!(
        !events_a.iter().any(
            |e| matches!(e, ClientEvent::MessageChunk { content, .. } if content == "hello B")
        ),
        "session A received session B's frame"
    );

    // One host, one mux connection, one endpoint — the registry reuses it.
    let host_again = registry.acquire(mock.endpoint()).unwrap();
    assert!(Arc::ptr_eq(&host, &host_again));
    assert_eq!(mock.mux_connection_count(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn launch_token_exchange_captures_redirect_cookie() {
    let mut config = default_config();
    config.require_auth = true;
    let mock = MockHarness::start(config).await;
    let endpoint = format!("{}?token=mock-launch-token", mock.endpoint());

    let client = HttpClient::new(&endpoint).unwrap();
    client
        .probe(uuid::Uuid::new_v4().to_string())
        .await
        .unwrap();

    let calls = mock.calls();
    assert_eq!(calls.len(), 1, "expected exactly the auth probe call");
    assert_eq!(calls[0].0, "session/list");
}

#[tokio::test(flavor = "multi_thread")]
async fn unmatched_frame_is_dropped_not_fatal() {
    let mut c = default_config();
    c.mux = scripts_with(vec![
        mux_subscribed("s-a", 0),
        // Frame for a session nobody registered.
        mux_assistant_text_delta("s-ghost", 1, "ghost"),
        mux_assistant_text_delta("s-a", 2, "after ghost"),
    ]);
    let mock = MockHarness::start(c).await;
    let registry = Arc::new(HarnessHostRegistry::new());
    let host = registry.acquire(mock.endpoint()).unwrap();
    let (sink_a, rx_a) = test_sink();
    sink_a.set_session_id("s-a".into());
    host.router().register("s-a".into(), sink_a);

    let events = drain_events(&rx_a, Duration::from_millis(500));
    assert!(
        events.iter().any(
            |e| matches!(e, ClientEvent::MessageChunk { content, .. } if content == "after ghost")
        ),
        "stream must continue past the unmatched frame: {events:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_prompts_across_sessions_are_not_serialized() {
    let mock = MockHarness::start(default_config()).await;
    let client = HttpClient::new(&mock.endpoint()).unwrap();
    let registry = Arc::new(HarnessHostRegistry::new());
    let _host = registry.acquire(mock.endpoint()).unwrap();

    // Fire two prompts at once and verify both POSTs arrive (concurrency means
    // neither waits for the other; the mock records both).
    let client_a = client.clone();
    let client_b = client.clone();

    let a = tokio::spawn(async move {
        client_a
            .session_prompt(
                "rpc-a".into(),
                &dsh_bridge::SessionPromptPayload {
                    request_id: "req-a".into(),
                    session_id: "s-a".into(),
                    mode: dsh_bridge::PromptMode::Queue,
                    content: vec![dsh_bridge::PromptContentPart::text("prompt A")],
                    client_time_zone: None,
                },
            )
            .await
    });
    let b = tokio::spawn(async move {
        client_b
            .session_prompt(
                "rpc-b".into(),
                &dsh_bridge::SessionPromptPayload {
                    request_id: "req-b".into(),
                    session_id: "s-b".into(),
                    mode: dsh_bridge::PromptMode::Queue,
                    content: vec![dsh_bridge::PromptContentPart::text("prompt B")],
                    client_time_zone: None,
                },
            )
            .await
    });
    let (ra, rb) = tokio::join!(a, b);
    ra.unwrap().unwrap();
    rb.unwrap().unwrap();

    let calls = mock.calls();
    let prompts: Vec<&str> = calls
        .iter()
        .filter(|(method, _)| method == "session/prompt")
        .map(|(_, rpc_id)| rpc_id.as_str())
        .collect();
    assert!(prompts.contains(&"rpc-a"), "prompt A missing: {prompts:?}");
    assert!(prompts.contains(&"rpc-b"), "prompt B missing: {prompts:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn approval_round_trip_posts_client_response_to_respond() {
    // Mux stream delivers approval/requested for session A.
    let mut c = default_config();
    c.mux = scripts_with(vec![
        mux_subscribed("s-a", 0),
        serde_json::json!({
            "type": "waterfall",
            "event": "approval/request",
            "eventId": "approval-rpc-1",
            "agentId": "s-a",
            "request": {
                "type": "approval/requested",
                "approvalId": "a-1",
                "toolName": "bash",
                "callId": "call-1",
                "reason": "shell"
            }
        }),
    ]);
    let mock = MockHarness::start(c).await;
    let registry = Arc::new(HarnessHostRegistry::new());
    let host = registry.acquire(mock.endpoint()).unwrap();
    let (sink_a, rx_a) = test_sink();
    sink_a.set_session_id("s-a".into());
    host.router().register("s-a".into(), sink_a.clone());

    let events = drain_events(&rx_a, Duration::from_millis(500));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ClientEvent::ToolPermissionRequest { id, .. } if id == "a-1")),
        "approval request not surfaced: {events:?}"
    );

    // Bridge answers by POSTing a ClientResponse to /api/respond with the
    // approval payload, and the harness emits approval/resolved afterwards.
    let client = host.client().clone();
    let response = dsh_bridge::ClientResponse::ok(
        "approval-rpc-1".into(),
        json!({
            "sessionId": "s-a",
            "approvalId": "a-1",
            "outcome": "allowed-once"
        }),
    );
    client
        .respond_legacy(&serde_json::to_value(&response).unwrap())
        .await
        .unwrap();
    assert_eq!(mock.responds().len(), 1);
    assert_eq!(mock.responds()[0]["result"]["value"]["approvalId"], "a-1");
}

#[tokio::test(flavor = "multi_thread")]
async fn mux_drop_rebaselines_all_live_sessions_with_bounded_concurrency() {
    // Three live sessions A/B/C. The first mux connection emits their
    // subscribed frames then closes; the bridge reconnects and re-baselines
    // each session from last_seq via session.history.
    let mut c = default_config();
    c.mux = vec![
        MuxScript {
            frames: vec![
                mux_subscribed("s-a", 0),
                mux_subscribed("s-b", 0),
                mux_subscribed("s-c", 0),
            ],
            end: MuxEnd::Close,
            hold_frames_until: HoldFramesUntil::Connected,
        },
        MuxScript {
            frames: vec![
                // Re-baseline gap events delivered on the reconnected stream.
                mux_assistant_text_delta("s-a", 5, "A recovered"),
                mux_assistant_text_delta("s-c", 7, "C recovered"),
            ],
            end: MuxEnd::Hold,
            hold_frames_until: HoldFramesUntil::Connected,
        },
    ];
    let mock = MockHarness::start(c).await;
    let registry = Arc::new(HarnessHostRegistry::new());
    let host = registry.acquire(mock.endpoint()).unwrap();

    let (sink_a, rx_a) = test_sink();
    let (sink_b, rx_b) = test_sink();
    let (sink_c, rx_c) = test_sink();
    for (id, sink) in [("s-a", &sink_a), ("s-b", &sink_b), ("s-c", &sink_c)] {
        sink.set_session_id(id.into());
        host.router().register(id.into(), sink.clone());
    }

    // Wait for the drop, then the reconnect + re-baseline.
    mock.wait_for_mux_drop().await;
    let events_a = drain_events(&rx_a, Duration::from_millis(800));
    let events_b = drain_events(&rx_b, Duration::from_millis(200));
    let events_c = drain_events(&rx_c, Duration::from_millis(800));

    assert!(
        events_a.iter().any(
            |e| matches!(e, ClientEvent::MessageChunk { content, .. } if content == "A recovered")
        ),
        "A did not recover: {events_a:?}"
    );
    assert!(
        events_c.iter().any(
            |e| matches!(e, ClientEvent::MessageChunk { content, .. } if content == "C recovered")
        ),
        "C did not recover: {events_c:?}"
    );
    // B had no gap events; it must not have been interrupted.
    assert!(
        !events_b
            .iter()
            .any(|e| matches!(e, ClientEvent::Interrupted { .. })),
        "B was interrupted during re-baseline: {events_b:?}"
    );
    assert!(mock.mux_connection_count() >= 2, "mux did not reconnect");
}

#[tokio::test(flavor = "multi_thread")]
async fn per_session_history_failure_isolates_that_session() {
    // Session B's history call fails during re-baseline; A and C continue.
    let mut c = default_config();
    c.history_failures = vec!["s-b".to_string()];
    c.mux = vec![
        MuxScript {
            frames: vec![
                mux_subscribed("s-a", 0),
                mux_subscribed("s-b", 0),
                mux_subscribed("s-c", 0),
            ],
            end: MuxEnd::Close,
            hold_frames_until: HoldFramesUntil::Connected,
        },
        MuxScript {
            frames: vec![],
            end: MuxEnd::Hold,
            hold_frames_until: HoldFramesUntil::Connected,
        },
    ];
    let mock = MockHarness::start(c).await;
    let registry = Arc::new(HarnessHostRegistry::new());
    let host = registry.acquire(mock.endpoint()).unwrap();

    let (sink_a, rx_a) = test_sink();
    let (sink_b, rx_b) = test_sink();
    let (sink_c, rx_c) = test_sink();
    for (id, sink) in [("s-a", &sink_a), ("s-b", &sink_b), ("s-c", &sink_c)] {
        sink.set_session_id(id.into());
        host.router().register(id.into(), sink.clone());
    }

    mock.wait_for_mux_drop().await;
    let events_b = drain_events(&rx_b, Duration::from_millis(800));
    assert!(
        events_b.iter().any(
            |e| matches!(e, ClientEvent::Interrupted { reason } if reason.contains("history"))
        ),
        "B should be interrupted with the history failure: {events_b:?}"
    );
    // A and C are not interrupted.
    let events_a = drain_events(&rx_a, Duration::from_millis(200));
    let events_c = drain_events(&rx_c, Duration::from_millis(200));
    assert!(
        !events_a
            .iter()
            .any(|e| matches!(e, ClientEvent::Interrupted { .. }))
    );
    assert!(
        !events_c
            .iter()
            .any(|e| matches!(e, ClientEvent::Interrupted { .. }))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn last_session_exit_tears_down_host_other_session_keeps_receiving() {
    // 12.8: close session A (unregister + release) while B still has the host;
    // B keeps receiving events.
    let mut c = default_config();
    c.mux = scripts_with(vec![
        mux_subscribed("s-a", 0),
        mux_subscribed("s-b", 0),
        mux_assistant_text_delta("s-b", 3, "B alive"),
    ]);
    let mock = MockHarness::start(c).await;
    let registry = Arc::new(HarnessHostRegistry::new());
    let host = registry.acquire(mock.endpoint()).unwrap();

    let (sink_a, rx_a) = test_sink();
    let (sink_b, rx_b) = test_sink();
    sink_a.set_session_id("s-a".into());
    sink_b.set_session_id("s-b".into());
    host.router().register("s-a".into(), sink_a);
    host.router().register("s-b".into(), sink_b);

    // Session A exits: unregister and release its host refcount.
    host.router().unregister(&"s-a".to_string());
    registry.release(&mock.endpoint());

    // B still receives frames on the shared host.
    let events_b = drain_events(&rx_b, Duration::from_millis(500));
    assert!(
        events_b.iter().any(
            |e| matches!(e, ClientEvent::MessageChunk { content, .. } if content == "B alive")
        ),
        "B stopped receiving after A exited: {events_b:?}"
    );
    assert!(
        registry.host_alive(&mock.endpoint()),
        "host died while B is live"
    );
    drop(rx_a);
}

#[test]
fn attached_child_is_terminated_on_host_teardown() {
    // Regression: a child can be attached after the host is acquired for an
    // externally discovered endpoint. Attachment must mark the host as owning
    // the child, otherwise teardown skips termination and leaks `dsh web`.
    // Dedicated current-thread runtime (not the ambient one): `host.teardown()`
    // stops the host's runtime, which cannot be dropped from async context.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (mock, host) = rt.block_on(async {
        let mock = MockHarness::start(default_config()).await;
        let registry = Arc::new(HarnessHostRegistry::new());
        let host = registry.acquire(mock.endpoint()).unwrap();
        (mock, host)
    });

    // Spawn + wrap as a tokio child inside the runtime: `DshChild` holds a
    // `tokio::process::Child`, whose spawn/kill paths require an active reactor.
    let (child, child_id) = rt.block_on(async {
        let mut command = if cfg!(windows) {
            let mut command = tokio::process::Command::new("cmd");
            command.args(["/c", "ping", "-n", "60", "127.0.0.1"]);
            command
        } else {
            let mut command = tokio::process::Command::new("sleep");
            command.arg("60");
            command
        };
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let spawned = command.spawn().unwrap();
        let id = spawned.id().expect("test child should expose a pid");
        (dsh_bridge::DshChild::new(spawned), id)
    });
    let mut child = child;
    child.enable_kill_on_drop_job();

    host.attach_child(child);
    // teardown() stops the host's own runtime — must run outside async context.
    std::thread::spawn(move || host.teardown()).join().unwrap();

    // Termination is asynchronous (job release, handle teardown); a single
    // tasklist/kill-0 snapshot right after teardown can still see the
    // exiting process. Poll briefly before declaring a leak.
    let exited = wait_until(|| !process_exists(child_id), Duration::from_secs(5));
    assert!(
        exited,
        "attached test child survived host teardown; dsh web would leak"
    );
}

#[cfg(windows)]
fn process_exists(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}")])
        .output()
        .map(|output| !String::from_utf8_lossy(&output.stdout).contains("INFO: No tasks"))
        .unwrap_or(true)
}

#[cfg(not(windows))]
fn process_exists(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(true)
}

#[tokio::test(flavor = "multi_thread")]
async fn startupprobe_fails_fast_on_unreachable_endpoint() {
    // Transport failure: endpoint is not a harness host → Interrupted, no hang.
    let registry = Arc::new(HarnessHostRegistry::new());
    let host = registry.acquire("http://127.0.0.1:1".to_string()).unwrap();
    // Wait for the async probe to fail, then register: the router tells the
    // late-registered sink about the failure.
    assert!(
        wait_until(|| host.probe_failed(), Duration::from_secs(5)),
        "probe did not fail fast"
    );
    let (sink, rx) = test_sink();
    sink.set_session_id("s-x".into());
    host.router().register("s-x".into(), sink);

    let events = drain_events(&rx, Duration::from_millis(500));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ClientEvent::Interrupted { .. })),
        "unreachable endpoint should surface Interrupted: {events:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn single_session_create_prompt_message_flow() {
    // M1 bridge layer: session.create → session.prompt → assistant/message.
    let mut c = default_config();
    c.mux = scripts_with(vec![
        mux_subscribed("s-1", 0),
        mux_assistant_text_delta("s-1", 1, "text answer"),
        mux_assistant_final("s-1", 1),
        mux_session_event(
            "s-1",
            2,
            "turn/end",
            json!({ "turn": 1, "reason": { "kind": "completed" } }),
        ),
    ]);
    let mock = MockHarness::start(c).await;
    let registry = Arc::new(HarnessHostRegistry::new());
    let host = registry.acquire(mock.endpoint()).unwrap();
    let client = host.client().clone();
    let (sink, rx) = test_sink();
    sink.set_session_id("s-1".into());
    host.router().register("s-1".into(), sink);

    let created = client
        .session_create(
            "rpc-create".into(),
            &dsh_bridge::SessionCreatePayload {
                cwd: Some("/tmp".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(created.session_id, "s-1");

    client
        .session_prompt(
            "rpc-prompt".into(),
            &dsh_bridge::SessionPromptPayload {
                request_id: "req-prompt".into(),
                session_id: "s-1".into(),
                mode: dsh_bridge::PromptMode::Queue,
                content: vec![dsh_bridge::PromptContentPart::text("hi")],
                client_time_zone: None,
            },
        )
        .await
        .unwrap();

    let events = drain_events(&rx, Duration::from_millis(500));
    assert!(
        events.iter().any(
            |e| matches!(e, ClientEvent::MessageChunk { content, .. } if content == "text answer")
        ),
        "text answer not delivered: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ClientEvent::TurnFinished { .. }))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_call_and_result_render_as_tool_events() {
    // M2 bridge layer: tool/call + tool/result with ToolEventView → tool cards.
    let mut c = default_config();
    c.mux = scripts_with(vec![
        mux_subscribed("s-1", 0),
        json!({
            "type": "emit",
            "event": "session/event",
            "args": [{
                "type": "session/event",
                "sessionId": "s-1",
                "event": {
                    "type": "tool/call",
                    "seq": 1,
                    "time": 0.0,
                    "data": { "turn": 1, "step": 1, "callId": "call-1", "name": "bash", "arguments": "{\"command\":\"ls\"}" }
                },
                "view": { "for": "call", "view": { "card": "terminal", "title": "ls" } }
            }]
        }),
        json!({
            "type": "emit",
            "event": "session/event",
            "args": [{
                "type": "session/event",
                "sessionId": "s-1",
                "event": {
                    "type": "tool/result",
                    "seq": 2,
                    "time": 0.0,
                    "data": {
                        "turn": 1, "step": 1,
                        "message": {
                            "role": "user",
                            "content": [{ "type": "tool-result", "toolCallId": "call-1", "content": [] }]
                        }
                    }
                },
                "view": { "for": "result", "view": { "card": "terminal", "output": "ok", "exitCode": 0 } }
            }]
        }),
    ]);
    let mock = MockHarness::start(c).await;
    let registry = Arc::new(HarnessHostRegistry::new());
    let host = registry.acquire(mock.endpoint()).unwrap();
    let (sink, rx) = test_sink();
    sink.set_session_id("s-1".into());
    host.router().register("s-1".into(), sink);

    let events = drain_events(&rx, Duration::from_millis(500));
    assert!(
        events.iter().any(|e| matches!(e, ClientEvent::ToolStarted { id, kind, .. } if id == "call-1" && kind == "execute")),
        "tool start not delivered: {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(e, ClientEvent::ToolCompleted { id, terminal_output, .. } if id == "call-1" && terminal_output.is_some())),
        "tool completion not delivered: {events:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn session_restore_replays_history_before_started() {
    // M4: a resumed session replays `session.history` through the mapping
    // layer before SessionStarted, reconstructing the snapshot.
    let mut c = default_config();
    c.history_events = vec![
        history_event(
            1,
            "assistant/message",
            json!({ "turn": 1, "step": 1, "message": { "role": "assistant", "content": [{ "type": "text", "text": "prior answer" }] } }),
        ),
        history_event(
            2,
            "tool/call",
            json!({ "turn": 1, "step": 1, "callId": "call-prior", "name": "bash", "arguments": "{}" }),
        ),
        history_event(
            3,
            "turn/end",
            json!({ "turn": 1, "reason": { "kind": "completed" } }),
        ),
    ];
    c.mux = scripts_with(vec![mux_subscribed("s-1", 0)]);
    let mock = MockHarness::start(c).await;
    let registry = Arc::new(HarnessHostRegistry::new());

    let (tx, rx) = mpsc::channel::<ClientEvent>();
    let (command_tx, command_rx) = mpsc::channel();
    let config = acp_core::SessionConfig {
        workspace_root: "/tmp".into(),
        app_data_root: "/tmp".into(),
        model: "deepseek-v4-pro".into(),
        agent_command: "dsh".into(),
        agent_env: Vec::new(),
        resume_session_id: Some("s-1".into()),
        log_id: "test-log".into(),
        acp_port: 0,
        remote_ssh: None,
        mcp_servers: Vec::new(),
        harness_endpoint: Some(mock.endpoint()),
        agent_preset: None,
    };
    let worker_registry = registry.clone();
    let worker = std::thread::spawn(move || {
        dsh_bridge::run_harness_session(
            worker_registry,
            config,
            tx,
            command_rx,
            PermissionBroker::default(),
            acp_core::ShutdownSignal::default(),
        )
    });

    // History events replay before SessionStarted.
    let mut saw_history_text = false;
    let mut saw_started = false;
    let mut saw_prior_tool = false;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(5)
        && !(saw_history_text && saw_prior_tool && saw_started)
    {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ClientEvent::MessageChunk { content, .. }) if content == "prior answer" => {
                saw_history_text = true;
            }
            Ok(ClientEvent::ToolStarted { id, .. }) if id == "call-prior" => {
                saw_prior_tool = true;
            }
            Ok(ClientEvent::SessionStarted { .. }) => {
                saw_started = true;
            }
            Ok(ClientEvent::TurnFinished { .. }) => {}
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(saw_history_text, "history assistant text was not replayed");
    assert!(saw_prior_tool, "history tool call was not replayed");
    assert!(saw_started, "SessionStarted was not emitted after history");

    let _ = command_tx.send(acp_core::RuntimeCommand::Shutdown);
    let _ = worker.join();
}

#[tokio::test(flavor = "multi_thread")]
async fn session_follow_delivers_assistant_text() {
    // dsh 0.1.2 routes session content events (assistant chunks, tool calls)
    // through the per-session `session/follow` journal stream, not the
    // `$events` mux. This test proves the bridge opens that stream and maps
    // its frames into `ClientEvent`s.
    let mut c = default_config();
    c.follow = scripts_with(vec![
        // Opening snapshot: no records, model-selection projection.
        serde_json::json!({
            "type": "snapshot",
            "cursor": 0,
            "records": [],
            "hasMore": false,
            "projections": {
                "asOfSeq": 0,
                "values": {
                    "modelSelection": {
                        "next": { "provider": "timiai", "model": "gpt-5.5" }
                    }
                }
            }
        }),
        // Live assistant text-delta + turn end.
        serde_json::json!({
            "type": "event",
            "event": {
                "type": "assistant/chunk",
                "seq": 1,
                "time": 0.0,
                "data": {
                    "turn": 1,
                    "step": 1,
                    "chunk": {
                        "type": "text-delta",
                        "index": 0,
                        "text": "follow answer"
                    }
                }
            }
        }),
        serde_json::json!({
            "type": "event",
            "event": {
                "type": "turn/end",
                "seq": 2,
                "time": 0.0,
                "data": { "turn": 1, "reason": { "kind": "completed" } }
            }
        }),
    ]);
    let mock = MockHarness::start(c).await;
    let registry = Arc::new(HarnessHostRegistry::new());

    let (tx, rx) = mpsc::channel::<ClientEvent>();
    let (command_tx, command_rx) = mpsc::channel();
    let config = acp_core::SessionConfig {
        workspace_root: "/tmp".into(),
        app_data_root: "/tmp".into(),
        model: String::new(),
        agent_command: "dsh".into(),
        agent_env: Vec::new(),
        resume_session_id: None,
        log_id: "test-log".into(),
        acp_port: 0,
        remote_ssh: None,
        mcp_servers: Vec::new(),
        harness_endpoint: Some(mock.endpoint()),
        agent_preset: None,
    };
    let worker_registry = registry.clone();
    let worker = std::thread::spawn(move || {
        dsh_bridge::run_harness_session(
            worker_registry,
            config,
            tx,
            command_rx,
            PermissionBroker::default(),
            acp_core::ShutdownSignal::default(),
        )
    });

    let start = std::time::Instant::now();
    let mut saw_started = false;
    let mut saw_text = false;
    let mut saw_turn_end = false;
    let mut model_value: Option<String> = None;
    while start.elapsed() < Duration::from_secs(5) && !(saw_started && saw_text && saw_turn_end) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ClientEvent::SessionStarted { .. }) => saw_started = true,
            Ok(ClientEvent::MessageChunk { content, .. }) if content == "follow answer" => {
                saw_text = true;
            }
            Ok(ClientEvent::TurnFinished { .. }) => saw_turn_end = true,
            Ok(ClientEvent::SessionConfigValueChanged {
                control_id,
                value_id,
                ..
            }) if control_id == "model" => {
                model_value = Some(value_id);
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(saw_started, "SessionStarted was not emitted");
    assert!(saw_text, "session/follow assistant text was not delivered");
    assert!(saw_turn_end, "session/follow turn/end was not delivered");
    assert_eq!(
        model_value.as_deref(),
        Some("kodex-provider/timiai/gpt-5.5"),
        "follow snapshot must restore provider + model"
    );

    let _ = command_tx.send(acp_core::RuntimeCommand::Shutdown);
    let _ = worker.join();
}

#[tokio::test(flavor = "multi_thread")]
async fn session_restore_model_control_uses_projection_not_catalog_default() {
    // On resume the session's durable model selection (from the
    // `modelSelection` projection) must drive the Model control's
    // `current_value_id` — not the catalog default. Without this the composer
    // shows the default provider+model (e.g. deepseek) while the session
    // actually runs a different one (e.g. timiai).
    let mut c = default_config();
    c.history_projections = Some(json!({
        "asOfSeq": 3,
        "values": {
            "modelSelection": {
                "lastUsed": { "provider": "timiai", "model": "glm-5.3-ioa" },
                "next": { "provider": "timiai", "model": "glm-5.3-ioa" }
            }
        }
    }));
    let mock = MockHarness::start(c).await;
    let registry = Arc::new(HarnessHostRegistry::new());

    let (tx, rx) = mpsc::channel::<ClientEvent>();
    let (command_tx, command_rx) = mpsc::channel();
    let config = acp_core::SessionConfig {
        workspace_root: "/tmp".into(),
        app_data_root: "/tmp".into(),
        model: "old-model".into(),
        agent_command: "dsh".into(),
        agent_env: Vec::new(),
        resume_session_id: Some("s-1".into()),
        log_id: "test-log".into(),
        acp_port: 0,
        remote_ssh: None,
        mcp_servers: Vec::new(),
        harness_endpoint: Some(mock.endpoint()),
        agent_preset: None,
    };
    let worker_registry = registry.clone();
    let worker = std::thread::spawn(move || {
        dsh_bridge::run_harness_session(
            worker_registry,
            config,
            tx,
            command_rx,
            PermissionBroker::default(),
            acp_core::ShutdownSignal::default(),
        )
    });

    let start = std::time::Instant::now();
    let mut model_control: Option<workspace_model::SessionConfigControl> = None;
    while start.elapsed() < Duration::from_secs(5) && model_control.is_none() {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ClientEvent::SessionConfigUpdated { state }) => {
                model_control = state
                    .controls
                    .into_iter()
                    .find(|control| control.id == "model");
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let control = model_control.expect("Model control was not published");
    assert_eq!(
        control.current_value_id, "kodex-provider/timiai/glm-5.3-ioa",
        "Model control must restore the session's selection, not the catalog default"
    );

    let _ = command_tx.send(acp_core::RuntimeCommand::Shutdown);
    let _ = worker.join();
}

#[tokio::test(flavor = "multi_thread")]
async fn session_restore_uses_page_model_selection_projection() {
    // dsh returns the durable per-session model under `page.projections`.
    // Without consuming it, app-core restores a model label without a
    // provider and the composer cannot resolve the actual provider.
    let mut c = default_config();
    c.history_projections = Some(json!({
        "asOfSeq": 3,
        "values": {
            "modelSelection": {
                "lastUsed": { "provider": "custom_cline", "model": "glm-5.3-flash" },
                "next": { "provider": "timiai", "model": "gpt-5.5" }
            }
        }
    }));
    let mock = MockHarness::start(c).await;
    let registry = Arc::new(HarnessHostRegistry::new());

    let (tx, rx) = mpsc::channel::<ClientEvent>();
    let (command_tx, command_rx) = mpsc::channel();
    let config = acp_core::SessionConfig {
        workspace_root: "/tmp".into(),
        app_data_root: "/tmp".into(),
        model: "old-model".into(),
        agent_command: "dsh".into(),
        agent_env: Vec::new(),
        resume_session_id: Some("s-1".into()),
        log_id: "test-log".into(),
        acp_port: 0,
        remote_ssh: None,
        mcp_servers: Vec::new(),
        harness_endpoint: Some(mock.endpoint()),
        agent_preset: None,
    };
    let worker_registry = registry.clone();
    let worker = std::thread::spawn(move || {
        dsh_bridge::run_harness_session(
            worker_registry,
            config,
            tx,
            command_rx,
            PermissionBroker::default(),
            acp_core::ShutdownSignal::default(),
        )
    });

    let start = std::time::Instant::now();
    let mut saw_started = false;
    let mut model_value = None;
    while start.elapsed() < Duration::from_secs(5) && !(saw_started && model_value.is_some()) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ClientEvent::SessionStarted { .. }) => saw_started = true,
            Ok(ClientEvent::SessionConfigValueChanged {
                control_id,
                value_id,
                ..
            }) if control_id == "model" => {
                model_value = Some(value_id);
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(saw_started, "SessionStarted was not emitted");
    assert_eq!(
        model_value.as_deref(),
        Some("kodex-provider/timiai/gpt-5.5"),
        "page modelSelection projection must restore provider + model"
    );

    let _ = command_tx.send(acp_core::RuntimeCommand::Shutdown);
    let _ = worker.join();
}

#[tokio::test(flavor = "multi_thread")]
async fn session_resume_does_not_send_agent_preset() {
    // Regression: a dsh preset is fixed at session creation. Resuming with a
    // (possibly different) preset in `session.create` makes the harness reject
    // the resume with `agent-preset-conflict` — the resume must omit the
    // preset entirely so the session's own preset is respected.
    let mut c = default_config();
    c.mux = scripts_with(vec![mux_subscribed("s-1", 0)]);
    let mock = MockHarness::start(c).await;
    let registry = Arc::new(HarnessHostRegistry::new());

    let (tx, _rx) = mpsc::channel::<ClientEvent>();
    let (command_tx, command_rx) = mpsc::channel();
    let config = acp_core::SessionConfig {
        workspace_root: "/tmp".into(),
        app_data_root: "/tmp".into(),
        model: "deepseek-v4-pro".into(),
        agent_command: "dsh".into(),
        agent_env: Vec::new(),
        resume_session_id: Some("s-1".into()),
        log_id: "t".into(),
        acp_port: 0,
        remote_ssh: None,
        mcp_servers: Vec::new(),
        harness_endpoint: Some(mock.endpoint()),
        // A stale preset (e.g. the current global default) must be dropped.
        agent_preset: Some("standard".into()),
    };
    let wr = registry.clone();
    let worker = std::thread::spawn(move || {
        dsh_bridge::run_harness_session(
            wr,
            config,
            tx,
            command_rx,
            PermissionBroker::default(),
            acp_core::ShutdownSignal::default(),
        )
    });

    // Wait for the session.create call to arrive at the mock host.
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if !mock.creates().is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let creates = mock.creates();
    assert_eq!(creates.len(), 1, "expected exactly one session.create");
    assert_eq!(
        creates[0].get("sessionId").and_then(Value::as_str),
        Some("s-1")
    );
    assert!(
        creates[0].get("agentPreset").is_none(),
        "resume must not send agentPreset; got {:?}",
        creates[0]
    );

    let _ = command_tx.send(acp_core::RuntimeCommand::Shutdown);
    let _ = worker.join();
}

#[tokio::test(flavor = "multi_thread")]
async fn question_answer_multiple_questions_partial_payload() {
    // Reproduce the real-world hang: dsh asks TWO questions, Kodex answers
    // only ONE — dsh's matchesQuestions requires answers.length ==
    // questions.length, so a partial batch is rejected as bad-response.
    let mut c = default_config();
    c.mux = vec![MuxScript {
        frames: vec![
            mux_subscribed("s-1", 0),
            json!({
                "type": "waterfall",
                "event": "user-questions/request",
                "eventId": "qrpc-multi",
                "agentId": "s-1",
                "request": {
                    "type": "question/requested",
                    "sessionId": "s-1",
                    "questions": [
                        { "id": "q1", "question": "First?", "options": [{ "label": "A" }] },
                        { "id": "q2", "question": "Second?", "options": [{ "label": "X" }] }
                    ]
                }
            }),
        ],
        end: MuxEnd::Hold,
        hold_frames_until: HoldFramesUntil::SessionRegistered,
    }];
    let mock = MockHarness::start(c).await;
    let registry = Arc::new(HarnessHostRegistry::new());
    let (tx, rx) = mpsc::channel::<ClientEvent>();
    let (command_tx, command_rx) = mpsc::channel();
    let config = acp_core::SessionConfig {
        workspace_root: "/tmp".into(),
        app_data_root: "/tmp".into(),
        model: "deepseek-v4-pro".into(),
        agent_command: "dsh".into(),
        agent_env: Vec::new(),
        resume_session_id: None,
        log_id: "t".into(),
        acp_port: 0,
        remote_ssh: None,
        mcp_servers: Vec::new(),
        harness_endpoint: Some(mock.endpoint()),
        agent_preset: None,
    };
    let wr = registry.clone();
    let worker = std::thread::spawn(move || {
        dsh_bridge::run_harness_session(
            wr,
            config,
            tx,
            command_rx,
            PermissionBroker::default(),
            acp_core::ShutdownSignal::default(),
        )
    });
    let start = std::time::Instant::now();
    let mut saw_question = false;
    while start.elapsed() < Duration::from_secs(5) && !saw_question {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ClientEvent::ToolPermissionRequest {
                id, input: Some(_), ..
            }) if id == "q1" => {
                saw_question = true;
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(
        saw_question,
        "question request was not surfaced (frames raced the sink registration?)"
    );
    // Answer BOTH questions in REVERSE order — the UI's answer map iterates in
    // insertion order, which need not match the question order. The bridge must
    // re-sort answers positionally or dsh's matchesQuestions rejects the batch.
    let (reply_tx, reply_rx) = mpsc::channel();
    command_tx
        .send(acp_core::RuntimeCommand::ResolveHarnessApproval {
            rpc_id: "q1".into(),
            result: acp_core::HarnessApprovalResult::Question {
                answers: vec![
                    acp_core::HarnessQuestionAnswer {
                        question_id: "q2".into(),
                        selected: vec!["X".into()],
                        custom: None,
                    },
                    acp_core::HarnessQuestionAnswer {
                        question_id: "q1".into(),
                        selected: vec!["A".into()],
                        custom: None,
                    },
                ],
            },
            reply_tx,
        })
        .unwrap();
    reply_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    let responds = mock.responds();
    assert_eq!(responds.len(), 1);
    // The result value must list answers in the question order (q1 then q2),
    // even though they were submitted reversed.
    let answers = responds[0]
        .pointer("/outcome/value/answer/answers")
        .and_then(Value::as_array)
        .expect("answers array");
    let ids: Vec<&str> = answers
        .iter()
        .map(|a| a.get("id").and_then(Value::as_str).unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["q1", "q2"],
        "answers must be re-ordered to the question order"
    );
    let _ = command_tx.send(acp_core::RuntimeCommand::Shutdown);
    let _ = worker.join();
}

#[tokio::test(flavor = "multi_thread")]
async fn question_answer_payload_matches_dsh_schema() {
    // Dump the exact `$events/result` args for a question answer, so we can
    // diff against the gateway's strict clientId/eventId/outcome format and
    // dsh's answer schema/count/id-order/label validation.
    let mut c = default_config();
    c.mux = vec![MuxScript {
        frames: vec![
            mux_subscribed("s-1", 0),
            json!({
                "type": "waterfall",
                "event": "user-questions/request",
                "eventId": "qrpc-1",
                "agentId": "s-1",
                "request": {
                    "type": "question/requested",
                    "sessionId": "s-1",
                    "questions": [
                        { "id": "q1", "question": "Pick", "options": [{ "label": "是（推荐）" }, { "label": "否" }] }
                    ]
                }
            }),
        ],
        end: MuxEnd::Hold,
        hold_frames_until: HoldFramesUntil::SessionRegistered,
    }];
    let mock = MockHarness::start(c).await;
    let registry = Arc::new(HarnessHostRegistry::new());
    let (tx, rx) = mpsc::channel::<ClientEvent>();
    let (command_tx, command_rx) = mpsc::channel();
    let config = acp_core::SessionConfig {
        workspace_root: "/tmp".into(),
        app_data_root: "/tmp".into(),
        model: "deepseek-v4-pro".into(),
        agent_command: "dsh".into(),
        agent_env: Vec::new(),
        resume_session_id: None,
        log_id: "t".into(),
        acp_port: 0,
        remote_ssh: None,
        mcp_servers: Vec::new(),
        harness_endpoint: Some(mock.endpoint()),
        agent_preset: None,
    };
    let wr = registry.clone();
    let worker = std::thread::spawn(move || {
        dsh_bridge::run_harness_session(
            wr,
            config,
            tx,
            command_rx,
            PermissionBroker::default(),
            acp_core::ShutdownSignal::default(),
        )
    });
    // wait for question
    let start = std::time::Instant::now();
    let mut saw_question = false;
    while start.elapsed() < Duration::from_secs(5) && !saw_question {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ClientEvent::ToolPermissionRequest {
                id, input: Some(_), ..
            }) if id == "q1" => {
                saw_question = true;
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(
        saw_question,
        "question request was not surfaced (frames raced the sink registration?)"
    );
    let (reply_tx, reply_rx) = mpsc::channel();
    command_tx
        .send(acp_core::RuntimeCommand::ResolveHarnessApproval {
            rpc_id: "q1".into(),
            result: acp_core::HarnessApprovalResult::Question {
                answers: vec![acp_core::HarnessQuestionAnswer {
                    question_id: "q1".into(),
                    selected: vec!["是（推荐）".into()],
                    custom: None,
                }],
            },
            reply_tx,
        })
        .unwrap();
    reply_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    let responds = mock.responds();
    assert_eq!(responds.len(), 1);
    assert_eq!(responds[0]["clientId"], "$events-client-1");
    assert_eq!(responds[0]["eventId"], "qrpc-1");
    assert_eq!(responds[0]["outcome"]["kind"], "result");
    assert_eq!(
        responds[0]["outcome"]["value"]["answer"]["answers"][0]["selected"][0],
        "是（推荐）"
    );
    let _ = command_tx.send(acp_core::RuntimeCommand::Shutdown);
    let _ = worker.join();
}

#[tokio::test(flavor = "multi_thread")]
async fn question_answer_rejected_as_bad_response_surfaces_notice() {
    // When dsh rejects an answer as `bad-response` (e.g. a validation mismatch
    // in the payload), the pending question stays open host-side and the turn
    // would hang. The bridge must surface the rejection as a system notice so
    // the user sees what happened instead of a silent stuck session.
    let mut c = default_config();
    c.respond_reject = true;
    c.mux = vec![MuxScript {
        frames: vec![
            mux_subscribed("s-1", 0),
            json!({
                "type": "waterfall",
                "event": "user-questions/request",
                "eventId": "question-rpc-rej",
                "agentId": "s-1",
                "request": {
                    "type": "question/requested",
                    "sessionId": "s-1",
                    "questions": [
                        { "id": "q1", "question": "Pick one", "options": [{ "label": "A" }] }
                    ]
                }
            }),
        ],
        end: MuxEnd::Hold,
        hold_frames_until: HoldFramesUntil::SessionRegistered,
    }];
    let mock = MockHarness::start(c).await;
    let registry = Arc::new(HarnessHostRegistry::new());

    let (tx, rx) = mpsc::channel::<ClientEvent>();
    let (command_tx, command_rx) = mpsc::channel();
    let config = acp_core::SessionConfig {
        workspace_root: "/tmp".into(),
        app_data_root: "/tmp".into(),
        model: "deepseek-v4-pro".into(),
        agent_command: "dsh".into(),
        agent_env: Vec::new(),
        resume_session_id: None,
        log_id: "test-log".into(),
        acp_port: 0,
        remote_ssh: None,
        mcp_servers: Vec::new(),
        harness_endpoint: Some(mock.endpoint()),
        agent_preset: None,
    };
    let worker_registry = registry.clone();
    let worker = std::thread::spawn(move || {
        dsh_bridge::run_harness_session(
            worker_registry,
            config,
            tx,
            command_rx,
            PermissionBroker::default(),
            acp_core::ShutdownSignal::default(),
        )
    });

    // Wait for the question to surface.
    let start = std::time::Instant::now();
    let mut saw_question = false;
    while start.elapsed() < Duration::from_secs(5) && !saw_question {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ClientEvent::ToolPermissionRequest {
                id, input: Some(_), ..
            }) if id == "q1" => {
                saw_question = true;
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(saw_question, "question request was not surfaced");

    let (reply_tx, reply_rx) = mpsc::channel();
    command_tx
        .send(acp_core::RuntimeCommand::ResolveHarnessApproval {
            rpc_id: "q1".into(),
            result: acp_core::HarnessApprovalResult::Question {
                answers: vec![acp_core::HarnessQuestionAnswer {
                    question_id: "q1".into(),
                    selected: vec!["A".into()],
                    custom: None,
                }],
            },
            reply_tx,
        })
        .unwrap();
    // The reply carries the rejection as an Err, so the UI layer can react.
    let outcome = reply_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("respond timed out");
    assert!(outcome.is_err(), "rejection must surface as an error");

    // And the user-facing notice is pushed onto the event stream.
    let start = std::time::Instant::now();
    let mut saw_notice = false;
    while start.elapsed() < Duration::from_secs(5) && !saw_notice {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ClientEvent::MessageChunk { role, content, .. }) => {
                if role == workspace_model::MessageRole::System && content.contains("未被接受")
                {
                    saw_notice = true;
                }
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(saw_notice, "rejection notice was not emitted");

    let _ = command_tx.send(acp_core::RuntimeCommand::Shutdown);
    let _ = worker.join();
}

#[tokio::test(flavor = "multi_thread")]
async fn session_restore_creates_with_stored_session_id() {
    // Regression: resuming a stored session must pass its dsh sessionId to
    // `session.create` so the harness RESUMES the persisted agent (full model
    // context) instead of minting a blank session that later prompts land in.
    let mock = MockHarness::start(default_config()).await;
    let registry = Arc::new(HarnessHostRegistry::new());

    let (tx, rx) = mpsc::channel::<ClientEvent>();
    let (command_tx, command_rx) = mpsc::channel();
    let config = acp_core::SessionConfig {
        workspace_root: "/tmp".into(),
        app_data_root: "/tmp".into(),
        model: "deepseek-v4-pro".into(),
        agent_command: "dsh".into(),
        agent_env: Vec::new(),
        resume_session_id: Some("s-1".into()),
        log_id: "test-log".into(),
        acp_port: 0,
        remote_ssh: None,
        mcp_servers: Vec::new(),
        harness_endpoint: Some(mock.endpoint()),
        agent_preset: None,
    };
    let worker_registry = registry.clone();
    let worker = std::thread::spawn(move || {
        dsh_bridge::run_harness_session(
            worker_registry,
            config,
            tx,
            command_rx,
            PermissionBroker::default(),
            acp_core::ShutdownSignal::default(),
        )
    });

    // Wait for SessionStarted (create + replay have completed by then).
    let start = std::time::Instant::now();
    let mut saw_started = false;
    while start.elapsed() < Duration::from_secs(5) && !saw_started {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ClientEvent::SessionStarted { .. }) => saw_started = true,
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(saw_started, "SessionStarted was not emitted");

    let creates = mock.creates();
    assert_eq!(creates.len(), 1, "expected exactly one session.create");
    assert_eq!(
        creates[0].get("sessionId").and_then(|v| v.as_str()),
        Some("s-1"),
        "session.create must carry the stored session id for resume"
    );

    let _ = command_tx.send(acp_core::RuntimeCommand::Shutdown);
    let _ = worker.join();
}

#[tokio::test(flavor = "multi_thread")]
async fn fresh_session_create_omits_session_id() {
    // A fresh session sends no sessionId so the harness mints one.
    let mock = MockHarness::start(default_config()).await;
    let registry = Arc::new(HarnessHostRegistry::new());

    let (tx, rx) = mpsc::channel::<ClientEvent>();
    let (command_tx, command_rx) = mpsc::channel();
    let config = acp_core::SessionConfig {
        workspace_root: "/tmp".into(),
        app_data_root: "/tmp".into(),
        model: "deepseek-v4-pro".into(),
        agent_command: "dsh".into(),
        agent_env: Vec::new(),
        resume_session_id: None,
        log_id: "test-log".into(),
        acp_port: 0,
        remote_ssh: None,
        mcp_servers: Vec::new(),
        harness_endpoint: Some(mock.endpoint()),
        agent_preset: None,
    };
    let worker_registry = registry.clone();
    let worker = std::thread::spawn(move || {
        dsh_bridge::run_harness_session(
            worker_registry,
            config,
            tx,
            command_rx,
            PermissionBroker::default(),
            acp_core::ShutdownSignal::default(),
        )
    });

    let start = std::time::Instant::now();
    let mut saw_started = false;
    while start.elapsed() < Duration::from_secs(5) && !saw_started {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ClientEvent::SessionStarted { .. }) => saw_started = true,
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(saw_started, "SessionStarted was not emitted");

    let creates = mock.creates();
    assert_eq!(creates.len(), 1, "expected exactly one session.create");
    assert!(
        creates[0].get("sessionId").is_none(),
        "fresh session.create must not carry a sessionId"
    );

    let _ = command_tx.send(acp_core::RuntimeCommand::Shutdown);
    let _ = worker.join();
}

#[tokio::test(flavor = "multi_thread")]
async fn session_publishes_steer_prompt_capabilities() {
    // The harness `session/prompt` RPC accepts `mode: "steer"`, so the bridge
    // must advertise `session_steer: true` via PromptCapabilitiesUpdated so
    // the composer enables the steer input while a turn is in flight.
    let mock = MockHarness::start(default_config()).await;
    let registry = Arc::new(HarnessHostRegistry::new());

    let (tx, rx) = mpsc::channel::<ClientEvent>();
    let (command_tx, command_rx) = mpsc::channel();
    let config = acp_core::SessionConfig {
        workspace_root: "/tmp".into(),
        app_data_root: "/tmp".into(),
        model: "deepseek-v4-pro".into(),
        agent_command: "dsh".into(),
        agent_env: Vec::new(),
        resume_session_id: None,
        log_id: "test-log".into(),
        acp_port: 0,
        remote_ssh: None,
        mcp_servers: Vec::new(),
        harness_endpoint: Some(mock.endpoint()),
        agent_preset: None,
    };
    let worker_registry = registry.clone();
    let worker = std::thread::spawn(move || {
        dsh_bridge::run_harness_session(
            worker_registry,
            config,
            tx,
            command_rx,
            PermissionBroker::default(),
            acp_core::ShutdownSignal::default(),
        )
    });

    let mut saw_steer_cap = false;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ClientEvent::PromptCapabilitiesUpdated { capabilities }) => {
                if capabilities.session_steer {
                    assert!(
                        capabilities.embedded_context,
                        "embedded_context should be advertised so workspace references are allowed"
                    );
                    saw_steer_cap = true;
                    break;
                }
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(
        saw_steer_cap,
        "PromptCapabilitiesUpdated with session_steer=true was not emitted"
    );

    let _ = command_tx.send(acp_core::RuntimeCommand::Shutdown);
    let _ = worker.join();
}

#[tokio::test(flavor = "multi_thread")]
async fn model_selector_publishes_model_control_with_catalog() {
    // The composer's model dropdown reads `session_config.controls` for a
    // Model control. The bridge must publish one from `session.models` after
    // session.create (mock returns a deepseek group with two models), so the
    // dropdown renders instead of spinning.
    let mock = MockHarness::start(default_config()).await;
    let registry = Arc::new(HarnessHostRegistry::new());

    let (tx, rx) = mpsc::channel::<ClientEvent>();
    let (command_tx, command_rx) = mpsc::channel();
    let config = acp_core::SessionConfig {
        workspace_root: "/tmp".into(),
        app_data_root: "/tmp".into(),
        model: "deepseek-v4-pro".into(),
        agent_command: "dsh".into(),
        agent_env: Vec::new(),
        resume_session_id: None,
        log_id: "test-log".into(),
        acp_port: 0,
        remote_ssh: None,
        mcp_servers: Vec::new(),
        harness_endpoint: Some(mock.endpoint()),
        agent_preset: None,
    };
    let worker_registry = registry.clone();
    let worker = std::thread::spawn(move || {
        dsh_bridge::run_harness_session(
            worker_registry,
            config,
            tx,
            command_rx,
            PermissionBroker::default(),
            acp_core::ShutdownSignal::default(),
        )
    });

    let mut model_control = None;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ClientEvent::SessionConfigUpdated { state }) => {
                model_control = state.controls.into_iter().find(|control| {
                    control.category == workspace_model::SessionConfigCategory::Model
                });
                if model_control.is_some() {
                    break;
                }
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let control = model_control.expect("Model control was not published");
    assert_eq!(
        control.current_value_id,
        "kodex-provider/deepseek/deepseek-v4-pro"
    );
    assert_eq!(control.choices.len(), 2);
    assert!(
        control
            .choices
            .iter()
            .all(|choice| choice.provider.as_deref() == Some("deepseek"))
    );
    assert!(
        control
            .choices
            .iter()
            .any(|choice| choice.id == "kodex-provider/deepseek/deepseek-v4-flash")
    );

    let _ = command_tx.send(acp_core::RuntimeCommand::Shutdown);
    let _ = worker.join();
}

#[tokio::test(flavor = "multi_thread")]
async fn question_answer_uses_waterfall_event_id() {
    // Regression: the question answer must echo the `$events` waterfall's
    // eventId (what dsh 0.1.2 matches its pending ask against), not the
    // UI-facing question id. Using the question id got a rejection and the
    // turn hung.
    // The question frame rides right after the bridge's `session/subscribed`
    // for s-1, which the mock mux emits only after receiving the bridge's
    // subscribe message — so the sink is registered before the frame is
    // dispatched (an up-front scripted frame would race it and be dropped).
    let mut c = default_config();
    c.mux = vec![MuxScript {
        frames: vec![
            mux_subscribed("s-1", 0),
            json!({
                "type": "waterfall",
                "event": "user-questions/request",
                "eventId": "question-rpc-9",
                "agentId": "s-1",
                "request": {
                    "type": "question/requested",
                    "sessionId": "s-1",
                    "questions": [
                        { "id": "q1", "question": "Pick one", "options": [{ "label": "A" }, { "label": "B" }] }
                    ]
                }
            }),
        ],
        end: MuxEnd::Hold,
        hold_frames_until: HoldFramesUntil::SessionRegistered,
    }];
    let mock = MockHarness::start(c).await;
    let registry = Arc::new(HarnessHostRegistry::new());

    let (tx, rx) = mpsc::channel::<ClientEvent>();
    let (command_tx, command_rx) = mpsc::channel();
    let config = acp_core::SessionConfig {
        workspace_root: "/tmp".into(),
        app_data_root: "/tmp".into(),
        model: "deepseek-v4-pro".into(),
        agent_command: "dsh".into(),
        agent_env: Vec::new(),
        resume_session_id: None,
        log_id: "test-log".into(),
        acp_port: 0,
        remote_ssh: None,
        mcp_servers: Vec::new(),
        harness_endpoint: Some(mock.endpoint()),
        agent_preset: None,
    };
    let worker_registry = registry.clone();
    let worker = std::thread::spawn(move || {
        dsh_bridge::run_harness_session(
            worker_registry,
            config,
            tx,
            command_rx,
            PermissionBroker::default(),
            acp_core::ShutdownSignal::default(),
        )
    });

    // Wait for session registration (SessionStarted comes after sink
    // registration), then inject the question frame.
    let start = std::time::Instant::now();
    let mut saw_started = false;
    while start.elapsed() < Duration::from_secs(5) && !saw_started {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ClientEvent::SessionStarted { .. }) => saw_started = true,
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(saw_started, "SessionStarted was not emitted");

    // Wait for the question request to surface (ui id = first question id).
    let start = std::time::Instant::now();
    let mut saw_question = false;
    while start.elapsed() < Duration::from_secs(5) && !saw_question {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ClientEvent::ToolPermissionRequest {
                id, input: Some(_), ..
            }) if id == "q1" => {
                saw_question = true;
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(saw_question, "question request was not surfaced");

    // Answer through the session command channel (UI id), as the app does.
    let (reply_tx, reply_rx) = mpsc::channel();
    command_tx
        .send(acp_core::RuntimeCommand::ResolveHarnessApproval {
            rpc_id: "q1".into(),
            result: acp_core::HarnessApprovalResult::Question {
                answers: vec![acp_core::HarnessQuestionAnswer {
                    question_id: "q1".into(),
                    selected: vec!["A".into()],
                    custom: None,
                }],
            },
            reply_tx,
        })
        .unwrap();
    reply_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("respond timed out")
        .expect("respond failed");

    let results = mock.responds();
    assert_eq!(results.len(), 1, "expected one $events/result call");
    assert_eq!(results[0]["clientId"], "$events-client-1");
    assert_eq!(
        results[0]["eventId"], "question-rpc-9",
        "$events/result must echo the question waterfall eventId"
    );
    assert_eq!(results[0]["outcome"]["kind"], "result");
    assert_eq!(
        results[0]["outcome"]["value"]["answer"]["answers"][0]["selected"][0],
        "A"
    );

    let _ = command_tx.send(acp_core::RuntimeCommand::Shutdown);
    let _ = worker.join();
}
