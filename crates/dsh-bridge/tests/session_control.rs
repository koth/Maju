//! Live usage projections over the `session/control` stream.
//!
//! dsh 0.1.5 moved the host-wide session control channel (queues, jobs, and the
//! per-session projection updates) off the `$events` mux onto its own
//! `session/control` logical stream. `contextPressure` is the harness's real
//! context occupancy — the only figure that reacts to `/compact` — and
//! `tokenUsage` is the durable cumulative usage. When the bridge followed only
//! `session/follow`, both arrived once in that stream's opening baseline and
//! never again, so the usage dock froze at the value from session load: it
//! neither tracked a running turn nor dropped after a compaction.
//!
//! These tests pin that the bridge opens the control stream and feeds its
//! frames (baseline and live) into the owning session's sink.

mod common;

use acp_core::{ClientEvent, PermissionBroker};
use common::{HoldFramesUntil, MockHarness, MuxEnd, MuxScript, default_config};
use serde_json::json;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

fn control_script(frames: Vec<serde_json::Value>) -> Vec<MuxScript> {
    vec![MuxScript {
        frames,
        end: MuxEnd::Hold,
        // The control stream is opened by the host itself, before any session
        // registers; hold the frames until `run_harness_session` has installed
        // its sink, else they are (correctly) dropped as unroutable.
        hold_frames_until: HoldFramesUntil::SessionRegistered,
    }]
}

/// Drive one harness session against the mock so a sink is registered, then
/// collect the events it received (stopping once `expected_updates` context
/// snapshots have arrived).
fn run_session_until_usage(mock: &MockHarness, expected_updates: usize) -> Vec<ClientEvent> {
    let registry = Arc::new(dsh_bridge::HarnessHostRegistry::new());
    let (tx, rx) = mpsc::channel::<ClientEvent>();
    let (_command_tx, command_rx) = mpsc::channel();
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
    let worker = std::thread::spawn(move || {
        dsh_bridge::run_harness_session(
            registry,
            config,
            tx,
            command_rx,
            PermissionBroker::default(),
            acp_core::ShutdownSignal::default(),
        )
    });

    let mut events = Vec::new();
    let mut updates = 0usize;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(5) && updates < expected_updates {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => {
                if is_context_snapshot(&event) {
                    updates += 1;
                }
                events.push(event);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    // The session thread owns the host for its lifetime; let it finish rather
    // than leaking the process handle into the next test.
    drop(worker);
    events
}

fn is_context_snapshot(event: &ClientEvent) -> bool {
    matches!(
        event,
        ClientEvent::UsageUpdated { usage }
            if usage.scope == workspace_model::UsageEventScope::ContextSnapshot
    )
}

/// Context occupancy of every context-snapshot event, in arrival order.
fn context_used(events: &[ClientEvent]) -> Vec<Option<u64>> {
    events
        .iter()
        .filter(|event| is_context_snapshot(event))
        .filter_map(|event| match event {
            ClientEvent::UsageUpdated { usage } => Some(usage.context.used_tokens),
            _ => None,
        })
        .collect()
}

fn session_total(events: &[ClientEvent]) -> Option<u64> {
    events.iter().find_map(|event| match event {
        ClientEvent::UsageUpdated { usage }
            if usage.scope == workspace_model::UsageEventScope::SessionTotal =>
        {
            // Mirrors the UI: the durable projection carries the cache-inclusive
            // input/output split, and `total_tokens` is only present when the
            // provider reports it directly.
            Some(usage.tokens.total_tokens.unwrap_or_else(|| {
                usage.tokens.input_tokens.unwrap_or(0) + usage.tokens.output_tokens.unwrap_or(0)
            }))
        }
        _ => None,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn session_control_delivers_context_pressure_updates() {
    let mut c = default_config();
    c.control = control_script(vec![
        // Opening baseline: occupancy plus the durable cumulative usage.
        json!({
            "type": "baseline",
            "value": {
                "queues": {},
                "jobs": {},
                "projections": {
                    "s-1": {
                        "asOfSeq": 4,
                        "values": {
                            "contextPressure": {
                                "pressureTokens": 400000,
                                "projectedTokens": 467000,
                                "contextWindow": 900000
                            },
                            "tokenUsage": {
                                "totals": {
                                    "uncachedInputTokens": 1000,
                                    "outputTokens": 234,
                                    "cacheReadTokens": 0,
                                    "cacheWriteTokens": 0
                                }
                            }
                        }
                    }
                }
            }
        }),
        // A live tick while the turn grows the surface…
        json!({
            "type": "projection",
            "sessionId": "s-1",
            "key": "contextPressure",
            "value": { "pressureTokens": 400000, "projectedTokens": 480000, "contextWindow": 900000 },
            "seq": 5
        }),
        // …and the post-`/compact` drop, which is the whole point: the panel
        // must follow the harness down, not stay stuck at the pre-compaction
        // figure.
        json!({
            "type": "projection",
            "sessionId": "s-1",
            "key": "contextPressure",
            "value": { "pressureTokens": 21000, "projectedTokens": 24000, "contextWindow": 900000 },
            "seq": 6
        }),
    ]);
    let mock = MockHarness::start(c).await;

    let events = run_session_until_usage(&mock, 3);

    let used = context_used(&events);
    assert!(
        used.len() >= 3,
        "expected baseline + two live context updates, got {used:?}"
    );
    assert_eq!(
        &used[..3],
        &[Some(467000), Some(480000), Some(24000)],
        "context occupancy must follow the control stream, including the post-compaction drop"
    );
    assert_eq!(
        session_total(&events),
        Some(1234),
        "the durable tokenUsage projection must reach the session too"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn session_control_ignores_unknown_frames() {
    // An additive control-frame kind must not break the stream (the bridge's
    // lenient-deserialization rule), and the projections that follow it must
    // still land.
    let mut c = default_config();
    c.control = control_script(vec![
        json!({ "type": "future-control-frame", "value": { "anything": true } }),
        json!({
            "type": "projection",
            "sessionId": "s-1",
            "key": "contextPressure",
            "value": { "projectedTokens": 9000, "contextWindow": 200000 }
        }),
    ]);
    let mock = MockHarness::start(c).await;

    let events = run_session_until_usage(&mock, 1);
    assert!(
        context_used(&events).contains(&Some(9000)),
        "the projection after an unknown frame must still be delivered: {events:?}"
    );
}
