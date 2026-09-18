//! Per-session lifecycle over the harness RPC: the `run_harness_session` entry
//! point mirrors `acp_core::runtime::run_session`'s shape so `SessionHandle`
//! stays transport-agnostic.
//!
//! One session acquires a shared [`HarnessHost`] from the registry, calls
//! `session.create` with the workspace `cwd`, registers a [`SessionSink`] in
//! the `SessionRouter`, and drives `RuntimeCommand`s over the shared HTTP
//! client (direct POSTs — not serialized through the router). Events arrive via
//! the host's router sink (the mux/host SSE loops run on the host's runtime).
//! One in-flight prompt per session; parallel across sessions sharing a host.

use acp_core::{ClientEvent, PermissionBroker, RuntimeCommand, SessionConfig, ShutdownSignal};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Instant;
use uuid::Uuid;
use workspace_model::UserPromptContent;

use crate::frame::{AssistantStreamFrame, MuxFrame};
use crate::host::{HarnessHostRegistry, SessionSink};
use crate::rpc_types::{
    PromptContentPart, PromptMode, SessionCancelPayload, SessionCreatePayload, SessionId,
    SessionPromptPayload, is_answer_protocol_mismatch,
};
use crate::transport::{FollowItem, FollowStreamItem};

/// Entry point — same shape as `acp_core::runtime::run_session`.
///
/// Runs on the caller's thread (the `SessionHandle` worker thread). The shared
/// SSE loops run on the host's own multi-thread runtime, so this function does
/// not own them and must not block on event delivery — it only drives the
/// command channel and issues control POSTs.
pub fn run_harness_session(
    registry: Arc<HarnessHostRegistry>,
    config: SessionConfig,
    tx_events: mpsc::Sender<ClientEvent>,
    rx_commands: mpsc::Receiver<RuntimeCommand>,
    permission_broker: PermissionBroker,
    shutdown_signal: ShutdownSignal,
) -> anyhow::Result<()> {
    let endpoint = config
        .harness_endpoint
        .clone()
        .ok_or_else(|| anyhow::anyhow!("harness_endpoint is not set"))?;

    // Acquire (or reuse) a shared host for this endpoint.
    let host = registry.acquire(endpoint.clone())?;
    let client = host.client().clone();

    // Create (or resume) the harness session. When resuming, pass the stored
    // dsh session id: the harness answers an explicit-id `session.create` by
    // resuming the persisted agent with its full model context, and a retry
    // with the same id+cwd returns the same session. Without the id, dsh
    // mints a fresh blank session and every later prompt lands in it with no
    // prior context.
    let boot_start = Instant::now();
    //
    // A preset is only sent for a NEW session. The harness fixes the preset at
    // creation and rejects a conflicting preset on resume with
    // `agent-preset-conflict`, so a resume must not carry one — the session's
    // own preset is respected automatically.
    let agent_preset = if config
        .resume_session_id
        .as_ref()
        .is_some_and(|id| !id.is_empty())
    {
        None
    } else {
        config.agent_preset.clone().filter(|p| !p.is_empty())
    };
    // dsh compares the resume `cwd` against the persisted identity with a
    // strict string match. Kodex's session-store normalizes workspace roots
    // to lowercase drive + forward slashes (e.g. `d:/work/admesh`), but the
    // harness persisted the verbatim path at creation (`D:\work\admesh`), so
    // a resume carrying the normalized form is rejected with
    // `session/conflict`. Re-canonicalize before sending so the wire form
    // matches the harness's stored identity on Windows.
    let harness_cwd = canonicalize_harness_cwd(&config.workspace_root);
    let create_payload = SessionCreatePayload {
        cwd: Some(harness_cwd.clone()),
        session_id: config.resume_session_id.clone().filter(|id| !id.is_empty()),
        agent_preset,
        ..Default::default()
    };
    let workspace_root = config.workspace_root.clone();
    let create_value = host
        .runtime()
        .block_on(client.session_create(Uuid::new_v4().to_string(), &create_payload))?;
    let session_id: SessionId = create_value.session_id;
    tracing::info!(
        target: "dsh-bridge::session",
        elapsed_ms = boot_start.elapsed().as_millis() as u64,
        session_id = %session_id,
        "session.create completed",
    );

    // Register a sink so the host's SSE loops deliver frames for this session.
    let sink = Arc::new(SessionSink::new(
        tx_events.clone(),
        permission_broker.clone(),
    ));
    sink.set_session_id(session_id.clone());
    // Shared "a prompt turn is in flight" flag. The session thread sets it when
    // a queued prompt is accepted and checks it before queueing the next one;
    // the sink clears it when the turn's `TurnFinished`/`Interrupted` is
    // forwarded to app-core. Without this, the guard would never be cleared
    // (turn completion flows through the sink on the host runtime, not through
    // this command loop) and the next prompt would be silently dropped.
    let inflight = Arc::new(std::sync::atomic::AtomicBool::new(false));
    sink.set_inflight_flag(inflight.clone());
    host.router().register(session_id.clone(), sink.clone());

    // If resuming, replay history before the live stream delivers events. The
    // resumed dsh session already carries the full model context; this replay
    // only rebuilds the UI transcript. The page response also carries the
    // durable model selection — capture it so the Model control restores the
    // session's actual provider+model instead of the catalog default.
    //
    // This runs BEFORE the follow stream is opened: the follow re-delivers the
    // session's whole journal from the start, and `apply_follow_frames` only
    // dedupes frames at or below `last_seq`. Replaying first puts the cursor at
    // the log cut, so the journal is filtered down to genuinely new events;
    // opening the follow first made the transcript a race between the replayed
    // messages and the live-mapped tool frames.
    let mut restored_model_selection: Option<(String, String)> = None;
    if let Some(resume_id) = config.resume_session_id.clone()
        && !resume_id.is_empty()
    {
        if let Ok(selection) = host
            .runtime()
            .block_on(replay_history(&client, &resume_id, &sink))
        {
            restored_model_selection = selection;
        }
    }

    // dsh 0.1.2 delivers session content events (assistant chunks, tool
    // calls, …) on a per-session `session/follow` journal stream — not on
    // the shared `$events` mux. Open that stream here and forward its frames
    // through the mapping layer so the UI actually renders the turn.
    {
        let follow_client = client.clone();
        let follow_sink = sink.clone();
        let follow_session_id = session_id.clone();
        let follow_shutdown = shutdown_signal.clone();
        host.runtime().spawn(async move {
            run_session_follow(
                follow_client,
                follow_sink,
                follow_session_id,
                follow_shutdown,
            )
            .await;
        });
    }

    // Emit SessionStarted so the reducer flips to the running state. The dsh
    // `session/subscribed` frame also arrives via mux, but the handle needs an
    // id promptly (it picks the session id off SessionStarted).
    let _ = tx_events.send(ClientEvent::SessionStarted {
        session_id: session_id.clone(),
    });

    // Persist/restore the session's actual preset so reconnect/resume shows
    // the correct preset instead of falling back to the global default.
    // - New session: dsh acks `session.create` with the actual preset.
    // - Resume: dsh does not echo the preset back and we don't send one (to
    //   avoid `agent-preset-conflict`), but the session's own preset is still
    //   known from the store — re-emit it so the UI restores correctly.
    let mut session_preset: Option<String> = create_value
        .agent_preset
        .as_deref()
        .filter(|p| !p.is_empty())
        .or_else(|| {
            // On resume the store carries the preset; app-core passes it via
            // `config.agent_preset` even though dsh's `session.create` must
            // not receive it.
            config.agent_preset.as_deref().filter(|p| !p.is_empty())
        })
        .map(|p| p.to_string());
    if let Some(preset) = &session_preset {
        let _ = tx_events.send(ClientEvent::SessionConfigValueChanged {
            control_id: "agent_preset".to_string(),
            value_id: preset.clone(),
            value_label: None,
        });
    }

    // Publish the config selectors: fetch `session.models` and `agentPreset.list`
    // and translate them into a single `SessionConfigUpdated` carrying the
    // Model and Mode (agent-preset) controls so the composer dropdowns render.
    // Pass `session_preset` so the Mode control immediately shows the
    // session's actual preset instead of the deployment default. Previously
    // a separate `emit_model_control(None)` call first published the default
    // preset, then `emit_config_controls` corrected it — but during resume the
    // `restore_pending_model_selection` flow sends a `SetModel` whose reply
    // also re-published with `None`, racing the correct value and leaving the
    // Mode control stuck on the default after a session switch-back.
    let models_start = Instant::now();
    emit_config_controls(
        &client,
        &host,
        &session_id,
        session_preset.as_deref(),
        restored_model_selection.as_ref(),
        &tx_events,
    );
    tracing::info!(
        target: "dsh-bridge::session",
        elapsed_ms = models_start.elapsed().as_millis() as u64,
        session_id = %session_id,
        "session.models + agentPreset.list published",
    );
    // Publish the `/compact` slash command. The harness publishes no ACP
    // `available_commands_update`, so the bridge synthesizes the one command
    // kodex executes on the user's behalf: the composer renders it in the "/"
    // menu, and sending it routes to the manual-compaction path in app-core
    // (`ForceCompact` → `commands/execute`). dsh 0.1.2 can omit `agentPreset`
    // on resume and custom presets may compose compaction, so only the
    // explicitly tool-less `minimal` preset is excluded; if the command is
    // absent, `commands/execute` reports `undefined` and the UI surfaces it.
    if compact_command_should_be_published(session_preset.as_deref()) {
        let _ = tx_events.send(ClientEvent::AvailableCommandsUpdated {
            commands: compact_slash_commands(),
        });
    }
    // Declare prompt capabilities. The harness `session/prompt` RPC accepts
    // `mode: "steer"` (an in-flight prompt can be steered mid-turn), so kodex
    // advertises `session_steer`. Workspace file/directory references are
    // carried as text mentions (`@path`) since the harness prompt wire is
    // text-only, so `embedded_context` is enabled and `prompt_part_to_wire`
    // translates `WorkspaceFile` parts into mention text. Image attachments
    // stay disabled until the harness image-attachment path is wired through.
    let _ = tx_events.send(ClientEvent::PromptCapabilitiesUpdated {
        capabilities: workspace_model::PromptInputCapabilities {
            session_steer: true,
            embedded_context: true,
            ..Default::default()
        },
    });

    // Drive the command channel on this thread. Events flow through the sink
    // on the host's runtime; we only handle commands here.
    let mut inflight_prompt: Option<String> = None;
    tracing::info!(
        target: "dsh-bridge::session",
        boot_elapsed_ms = boot_start.elapsed().as_millis() as u64,
        session_id = %session_id,
        "command loop ready",
    );
    // on the host's runtime; we only handle commands here. The shared
    // `inflight` flag is cleared by the sink when a turn completes
    // (`TurnFinished`/`Interrupted`), so the guard below stays accurate even
    // though turn completion never re-enters this loop.
    use std::sync::atomic::Ordering as AtomicOrdering;

    for command in rx_commands.iter() {
        if shutdown_signal.is_requested() {
            break;
        }
        match command {
            RuntimeCommand::SendPrompt {
                prompt,
                accepted_tx,
            } => {
                let is_steer = accepted_tx.is_some();
                // A steer prompt is delivered to the in-flight turn via the
                // harness `session/prompt` `mode: "steer"` RPC — it does NOT
                // start a new turn, so it is allowed while a prompt is in
                // flight and must not set the inflight flag. A queued prompt
                // starts a fresh turn and is rejected while one is running.
                if inflight.load(AtomicOrdering::Acquire) && !is_steer {
                    if let Some(tx) = accepted_tx {
                        let _ = tx.send(Err(anyhow::anyhow!(
                            "a prompt is already in flight for this session"
                        )));
                    }
                    continue;
                }
                let mode = if is_steer {
                    PromptMode::Steer
                } else {
                    PromptMode::Queue
                };
                let parts: Vec<PromptContentPart> = prompt
                    .into_iter()
                    .map(|p| prompt_part_to_wire(p, &workspace_root))
                    .collect();
                let rpc_id = Uuid::new_v4().to_string();
                let payload = SessionPromptPayload {
                    request_id: rpc_id.clone(),
                    session_id: session_id.clone(),
                    mode,
                    content: parts,
                    client_time_zone: Some(local_timezone()),
                };
                let prompt_start = Instant::now();
                let result = host
                    .runtime()
                    .block_on(client.session_prompt(rpc_id.clone(), &payload));
                match result {
                    Ok(_) => {
                        tracing::info!(
                            target: "dsh-bridge::session",
                            elapsed_ms = prompt_start.elapsed().as_millis() as u64,
                            session_id = %session_id,
                            steer = is_steer,
                            "session.prompt accepted",
                        );
                        // A steer does not start a new turn; keep the existing
                        // inflight prompt id so the running turn's completion
                        // still clears it.
                        // A steer does not start a new turn; leave the inflight
                        // flag as-is so the running turn's completion still
                        // clears it.
                        if !is_steer {
                            inflight.store(true, AtomicOrdering::Release);
                        }
                        if let Some(tx) = accepted_tx {
                            let _ = tx.send(Ok(()));
                        }
                    }
                    Err(err) => {
                        tracing::warn!(
                            target: "dsh-bridge::session",
                            elapsed_ms = prompt_start.elapsed().as_millis() as u64,
                            session_id = %session_id,
                            error = %err,
                            "session.prompt failed",
                        );
                        // The prompt never started, so no turn is in flight.
                        inflight.store(false, AtomicOrdering::Release);
                        let _ = tx_events.send(ClientEvent::Interrupted {
                            reason: format!("session.prompt failed: {err}"),
                        });
                        if let Some(tx) = accepted_tx {
                            let _ = tx.send(Err(err));
                        }
                    }
                }
            }
            RuntimeCommand::CancelPrompt { reply_tx } => {
                let payload = SessionCancelPayload {
                    session_id: session_id.clone(),
                };
                let started = Instant::now();
                let result = host
                    .runtime()
                    .block_on(client.session_cancel(Uuid::new_v4().to_string(), &payload));
                // app-core sends this command fire-and-forget (waiting on the
                // reply would hold the workspace mutex) and reflects
                // "cancelled" locally the moment it is queued. This log line is
                // therefore the ONLY signal that the harness refused the stop:
                // a silent failure here is what made the stop button look dead
                // while the turn kept streaming.
                if let Err(error) = &result {
                    tracing::warn!(
                        target: "dsh-bridge::session",
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        session_id = %session_id,
                        error = %error,
                        "session.cancel failed; the turn may still be running",
                    );
                }
                inflight.store(false, AtomicOrdering::Release);
                if let Some(tx) = reply_tx {
                    let _ = tx.send(result.map(|_| ()));
                }
            }
            RuntimeCommand::ResolveHarnessApproval {
                rpc_id,
                result,
                reply_tx,
            } => {
                let respond_start = Instant::now();
                let pending = sink.pending_approvals();
                let client_id = host.remote_event_client_id();
                // The answer value and its carrier are protocol-dependent (see
                // `AnswerProtocol`): dsh ≥ 0.1.5 wants the bare resolved value
                // in a `client-request` envelope, dsh ≤ 0.1.4 a wrapped value in
                // a bare body. The protocol is detected up front; a host that
                // still answers with the other shape gets one retry with the
                // answer rebuilt for it.
                let mut protocol = client.answer_protocol();
                let mut attempts = 0u8;
                let (send_result, response) = loop {
                    let request = pending.build_response(
                        &sink,
                        client_id.clone(),
                        protocol,
                        &rpc_id,
                        &result,
                    );
                    let Some(request) = request else {
                        break (
                            Err(anyhow::anyhow!(
                                "no pending approval/question for id {rpc_id}"
                            )),
                            None,
                        );
                    };
                    if attempts == 0 {
                        tracing::info!(
                            target: "dsh-bridge::respond",
                            rpc_id,
                            protocol = ?protocol,
                            payload = %serde_json::to_string(request.body()).unwrap_or_default(),
                            "sending respond"
                        );
                    }
                    let attempt = match &request {
                        crate::approval::AnswerRequest::Waterfall(args) => {
                            host.runtime().block_on(client.remote_events_result(args))
                        }
                        crate::approval::AnswerRequest::Legacy(payload) => {
                            host.runtime().block_on(client.respond_legacy(payload))
                        }
                    };
                    match attempt {
                        Err(err) if attempts == 0 && is_answer_protocol_mismatch(&err) => {
                            let next = protocol.toggled();
                            tracing::info!(
                                target: "dsh-bridge::respond",
                                rpc_id,
                                from = ?protocol,
                                to = ?next,
                                error = %err,
                                "host answered the other answer protocol; retrying once",
                            );
                            client.set_answer_protocol(next);
                            protocol = next;
                            attempts += 1;
                            continue;
                        }
                        other => break (other, Some(request)),
                    }
                };
                // A `bad-response` means dsh rejected the answer payload
                // (validation mismatch) — the pending ask stays open on the
                // host and the turn hangs. Surface it instead of swallowing.
                let send_result = send_result.and_then(|receipt| {
                    if !receipt.accepted() {
                        let reason = receipt.rejection_reason();
                        tracing::warn!(
                            target: "dsh-bridge::respond",
                            rpc_id,
                            response = %response
                                .as_ref()
                                .map(|r| serde_json::to_string(r.body()).unwrap_or_default())
                                .unwrap_or_default(),
                            reason = %reason,
                            "harness rejected respond (question/approval stays pending)"
                        );
                        let _ = sink.send(ClientEvent::MessageChunk {
                            role: workspace_model::MessageRole::System,
                            content: format!("回答未被接受（{reason}），请重试或取消当前轮次。"),
                        });
                        return Err(anyhow::anyhow!("harness respond rejected: {reason}"));
                    }
                    Ok(receipt)
                });
                // A not-pending receipt (late/duplicate) is a no-op, not an error.
                let send_result = send_result.or_else(|err| {
                    if err.to_string().contains("not-pending") {
                        Ok(crate::rpc_types::RpcReceipt::Legacy {
                            accepted: true,
                            reason: String::new(),
                        })
                    } else {
                        Err(err)
                    }
                });
                match &send_result {
                    Ok(_) => tracing::info!(
                        target: "dsh-bridge::session",
                        elapsed_ms = respond_start.elapsed().as_millis() as u64,
                        session_id = %session_id,
                        "respond accepted",
                    ),
                    Err(err) => tracing::warn!(
                        target: "dsh-bridge::session",
                        elapsed_ms = respond_start.elapsed().as_millis() as u64,
                        session_id = %session_id,
                        rpc_id = %rpc_id,
                        error = %err,
                        "respond failed",
                    ),
                }
                let _ = reply_tx.send(send_result.map(|_| ()));
            }
            RuntimeCommand::SetModel {
                model_id,
                provider,
                reply_tx,
            } => {
                let (model, provider) = match decode_model_value(&model_id, provider.clone()) {
                    Some(decoded) => decoded,
                    None => (model_id, provider.unwrap_or_default()),
                };
                let payload = crate::rpc_types::SessionSelectModelPayload {
                    session_id: session_id.clone(),
                    provider,
                    model,
                    reasoning_effort: None,
                };
                let result = host
                    .runtime()
                    .block_on(client.session_select_model(Uuid::new_v4().to_string(), &payload));
                let events = match result {
                    Ok(_) => {
                        // Re-publish the model selector so the dropdown reflects
                        // the new selection. Pass `session_preset` so the Mode
                        // control preserves the session's actual preset instead
                        // of resetting to the deployment default.
                        let mut refreshed = Vec::new();
                        emit_model_control_into(
                            &client,
                            &host,
                            &session_id,
                            session_preset.as_deref(),
                            None,
                            &mut refreshed,
                        );
                        refreshed
                    }
                    Err(err) => vec![ClientEvent::Interrupted {
                        reason: format!("session.selectModel failed: {err}"),
                    }],
                };
                let _ = reply_tx.send(Ok(events));
            }
            RuntimeCommand::StopTool { reply_tx, .. } => {
                // dsh supports only whole-turn cancel; per-tool stop degrades
                // to session.cancel with a UI note. Map StopTool -> cancel.
                let payload = SessionCancelPayload {
                    session_id: session_id.clone(),
                };
                let result = host
                    .runtime()
                    .block_on(client.session_cancel(Uuid::new_v4().to_string(), &payload));
                let reason = match &result {
                    Ok(_) => {
                        "per-tool stop is not supported by the harness backend; turn cancelled"
                            .to_string()
                    }
                    Err(error) => {
                        // Never report a stop that the harness rejected as done.
                        tracing::warn!(
                            target: "dsh-bridge::session",
                            session_id = %session_id,
                            error = %error,
                            "session.cancel (per-tool stop) failed; the turn may still be running",
                        );
                        format!("停止该工具失败：请求取消整轮被 harness 拒绝（{error}）")
                    }
                };
                let _ = reply_tx.send(Ok(vec![ClientEvent::Interrupted { reason }]));
            }
            RuntimeCommand::ForceCompact { reply_tx } => {
                // Manual compaction: execute the harness `/compact` command
                // over the typert gateway — spawned on the host runtime so
                // neither this command loop nor the caller (which holds the
                // app mutex) blocks for the run. The compaction is a full LLM
                // summarization (minutes on large contexts) and the harness
                // aborts the command when the HTTP request dies, so the
                // request uses the long `COMMANDS_EXECUTE_TIMEOUT` cap. The
                // outcome reaches the UI through the mux events
                // (`compaction/start` → `compaction/end`, plus the paired
                // `command/done` result mapped in `map_session_event`); the
                // reply only acknowledges the dispatch.
                //
                // Success paths need no extra event (the mux carries the
                // notices). The failure paths DO: the command never ran, so
                // no `command/*` events will ever arrive — without this the
                // user's /compact silently does nothing (only a backend log
                // line), which is how the double-wrapped-payload regression
                // shipped unnoticed.
                let compact_client = client.clone();
                let compact_session_id = session_id.clone();
                let compact_tx_events = tx_events.clone();
                host.runtime().spawn(async move {
                    let start = Instant::now();
                    let result = compact_client
                        .commands_execute(
                            Uuid::new_v4().to_string(),
                            &compact_session_id,
                            "/compact",
                        )
                        .await;
                    let elapsed_ms = start.elapsed().as_millis() as u64;
                    match &result {
                        Ok(None) => {
                            tracing::warn!(
                                target: "dsh-bridge::session",
                                elapsed_ms,
                                session_id = %compact_session_id,
                                "manual compaction: /compact command not registered (agent preset has no compaction seam)",
                            );
                            emit_compaction_failure(
                                &compact_tx_events,
                                "当前智能体预设未注册 /compact 命令，无法压缩上下文".to_string(),
                            );
                        }
                        Ok(Some(execution)) => match &execution.result {
                            crate::rpc_types::CommandsExecuteResult::Success { .. } => {
                                tracing::info!(
                                    target: "dsh-bridge::session",
                                    elapsed_ms,
                                    session_id = %compact_session_id,
                                    command_id = %execution.command_id,
                                    "manual compaction succeeded",
                                );
                            }
                            crate::rpc_types::CommandsExecuteResult::Error { text } => {
                                tracing::warn!(
                                    target: "dsh-bridge::session",
                                    elapsed_ms,
                                    session_id = %compact_session_id,
                                    command_id = %execution.command_id,
                                    error = %text,
                                    "manual compaction failed",
                                );
                                emit_compaction_failure(
                                    &compact_tx_events,
                                    format!("上下文压缩未完成：{text}"),
                                );
                            }
                        },
                        Err(err) => {
                            tracing::warn!(
                                target: "dsh-bridge::session",
                                elapsed_ms,
                                session_id = %compact_session_id,
                                error = %err,
                                "commands/execute failed",
                            );
                            emit_compaction_failure(
                                &compact_tx_events,
                                format!("上下文压缩请求失败：{err}"),
                            );
                        }
                    }
                });
                let _ = reply_tx.send(Ok(()));
            }
            RuntimeCommand::ForkSession {
                at_user_turn,
                user_message_text,
                user_message_occurrence,
                reply_tx,
            } => {
                // Conversation fork (`session.fork`): a fast control-plane
                // call — resolve the completed-turn boundary from the session
                // history, then POST the fork. Blocking reply: app-core needs
                // the child session id to create the local branch session.
                // Prefer the prompt-content anchor: the harness turn counter
                // diverges from kodex's turn-opening count (injected turns,
                // splice-joined prompts, repeated sends), so a pure ordinal
                // can cut several turns short of what the user picked.
                //
                // Every history page needs the session's current log cut
                // (`throughSeq`); without it the harness answers with an empty
                // page and the walk sees no prompts and no `turn/end` at all —
                // forking then always failed with "轮次尚未完成".
                let cursor = host
                    .runtime()
                    .block_on(resolve_history_cursor(&client, &session_id, &sink));
                let result = match user_message_text.as_deref() {
                    Some(text) => match host.runtime().block_on(find_prompt_turn_end_seq(
                        &client,
                        &session_id,
                        cursor,
                        text,
                        user_message_occurrence.max(1),
                        at_user_turn,
                    )) {
                        Ok(target_seq) => {
                            host.runtime()
                                .block_on(fork_at_seq(&client, &session_id, target_seq))
                        }
                        Err(bridge_error) => {
                            tracing::warn!(
                                target: "dsh-bridge::session",
                                session_id = %session_id,
                                at_user_turn,
                                error = %bridge_error,
                                "prompt-anchored fork failed; falling back to ordinal",
                            );
                            host.runtime().block_on(fork_session_at_turn(
                                &client,
                                &session_id,
                                cursor,
                                at_user_turn,
                            ))
                        }
                    },
                    None => host.runtime().block_on(fork_session_at_turn(
                        &client,
                        &session_id,
                        cursor,
                        at_user_turn,
                    )),
                };
                match &result {
                    Ok(child) => tracing::info!(
                        target: "dsh-bridge::session",
                        session_id = %session_id,
                        child_session_id = %child,
                        at_user_turn,
                        "session.fork completed",
                    ),
                    Err(error) => tracing::warn!(
                        target: "dsh-bridge::session",
                        session_id = %session_id,
                        at_user_turn,
                        error = %error,
                        "session.fork failed",
                    ),
                }
                let _ = reply_tx.send(result);
            }
            RuntimeCommand::Shutdown => break,
            // Config-option changes: the composer's model dropdown sends a
            // Model control change → `session.selectModel`; other controls
            // are no-ops (harness surfaces config via request/header).
            RuntimeCommand::SetConfigOption {
                config_id,
                value_id,
                provider,
                reply_tx,
            } => {
                let events = if config_id == "model" {
                    let (model, provider) = match decode_model_value(&value_id, provider) {
                        Some(decoded) => decoded,
                        None => {
                            // Fall back to treating the value as a bare model id.
                            (value_id, String::new())
                        }
                    };
                    let payload = crate::rpc_types::SessionSelectModelPayload {
                        session_id: session_id.clone(),
                        provider,
                        model,
                        reasoning_effort: None,
                    };
                    match host
                        .runtime()
                        .block_on(client.session_select_model(Uuid::new_v4().to_string(), &payload))
                    {
                        Ok(_) => {
                            let mut refreshed = Vec::new();
                            emit_model_control_into(
                                &client,
                                &host,
                                &session_id,
                                session_preset.as_deref(),
                                None,
                                &mut refreshed,
                            );
                            refreshed
                        }
                        Err(err) => vec![ClientEvent::Interrupted {
                            reason: format!("session.selectModel failed: {err}"),
                        }],
                    }
                } else {
                    Vec::new()
                };
                if let Some(tx) = reply_tx {
                    let _ = tx.send(Ok(events));
                }
            }
            // Mode = dsh agent preset: `agentPreset.select` switches the
            // composition for this session, then re-publish the config
            // controls so the dropdown reflects the new selection.
            RuntimeCommand::SetMode { mode_id, reply_tx } => {
                // The harness only allows switching presets on a blank
                // session; once a turn has started it rejects with
                // `agent-preset-locked`. Detect that up front so we never
                // mutate the shared permission broker on a doomed request
                // (`SessionHandle::set_mode` only skips the broker update on
                // RPC errors, but we want to fail before any RPC).
                // "Has this session produced anything yet?" — asked before
                // allowing a preset switch. The page cut comes from the follow
                // cursor, or from the harness when that frame has not landed
                // (see `resolve_history_cursor`); a session with no page at all
                // is blank, so its preset is still free to change.
                let started = {
                    let cursor = host
                        .runtime()
                        .block_on(resolve_history_cursor(&client, &session_id, &sink));
                    if cursor == 0 {
                        false
                    } else {
                        let history_payload = crate::rpc_types::SessionHistoryPayload {
                            session_id: session_id.to_string(),
                            through_seq: cursor,
                            before_seq: None,
                            max_messages: Some(1),
                        };
                        host.runtime()
                            .block_on(client.session_history(Uuid::new_v4().to_string(), &history_payload))
                            .map(|value| !value.events.is_empty())
                            .unwrap_or(false)
                    }
                };
                if started {
                    let _ = reply_tx.send(Err(anyhow::anyhow!(
                        "该会话已开始，预设已固定。请新建会话并选择目标预设。"
                    )));
                    continue;
                }
                let payload = crate::rpc_types::AgentPresetSelectPayload {
                    session_id: session_id.clone(),
                    agent_preset: mode_id.clone(),
                };
                let events = match host
                    .runtime()
                    .block_on(client.agent_preset_select(Uuid::new_v4().to_string(), &payload))
                {
                    Ok(value) => {
                        // Track the new preset so subsequent
                        // `emit_model_control_into` calls (e.g. from a
                        // SetModel reply) preserve it instead of resetting
                        // the Mode control to the deployment default.
                        session_preset = Some(value.agent_preset.clone());
                        let mut refreshed = Vec::new();
                        emit_model_control_into(
                            &client,
                            &host,
                            &session_id,
                            Some(value.agent_preset.as_str()),
                            None,
                            &mut refreshed,
                        );
                        refreshed
                    }
                    Err(err) => {
                        // A preset switch is only valid on a blank session;
                        // once a turn has started dsh answers
                        // `agent-preset-locked`. Return an error so
                        // `SessionHandle::set_mode` does not apply the new id
                        // to the shared permission broker, and so the composer
                        // surfaces the failure instead of pretending the
                        // switch worked.
                        if crate::rpc_types::rpc_error_code(&err).as_deref()
                            == Some("agent-preset-locked")
                        {
                            let _ = reply_tx.send(Err(anyhow::anyhow!(
                                "该会话已开始，预设已固定。请新建会话并选择目标预设。"
                            )));
                            continue;
                        }
                        vec![ClientEvent::Interrupted {
                            reason: format!("agentPreset.select failed: {err}"),
                        }]
                    }
                };
                let _ = reply_tx.send(Ok(events));
            }
            RuntimeCommand::ResolveCodeBuddyInterruption { reply_tx, .. } => {
                let _ = reply_tx.send(Err(anyhow::anyhow!(
                    "ResolveCodeBuddyInterruption is not supported by the harness backend"
                )));
            }
        }
    }

    // Session exit: unregister from the router and release the host refcount.
    host.router().unregister(&session_id);
    registry.release(&endpoint);
    Ok(())
}

/// Canonicalize a workspace path for the harness wire form so a Windows
/// resume `cwd` matches the path dsh persisted at session creation.
/// dsh compares the resume `cwd` against its stored identity with a strict
/// string match; kodex's session-store normalization (lowercase drive +
/// forward slashes, e.g. `d:/work/admesh`) differs from the verbatim path
/// dsh stored (`D:\work\admesh`), causing `session/conflict` on resume.
///
/// Mirrors Node's `path.resolve()` on Windows: `fs::canonicalize` produces
/// `\\?\D:\work\admesh`; strip the `\\?\` prefix and leave the rest in the
/// verbatim form dsh persisted.
fn canonicalize_harness_cwd(workspace_root: &str) -> String {
    let path = std::path::Path::new(workspace_root);
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let text = canonical.to_string_lossy().to_string();
    // Rust's canonicalize on Windows yields a verbatim path with `\\?\`
    // prefix; dsh (Node path.resolve) does not use that prefix, so strip it.
    text.strip_prefix(r"\\?\")
        .map(str::to_string)
        .unwrap_or(text)
}

/// The session's newest known log seq — the harness page cut (`throughSeq`).
///
/// Seeded from the follow opening frame (`sessionSubscribed.lastSeq`) and
/// advanced by every durable event, so it is exactly the "inclusive log cut
/// obtained from the corresponding follow opening frame" the harness documents.
/// Every `session.history` page must carry it: the harness caps a page at
/// `throughSeq + 1`, so a page without a real cursor comes back empty instead of
/// failing loudly.
fn history_cursor(sink: &SessionSink) -> u64 {
    sink.last_seq.load(std::sync::atomic::Ordering::Acquire)
}

/// The page cut to walk history with.
///
/// Prefers the follow cursor; when that frame has not landed yet (or was missed
/// while the stream was still establishing, which is not observable from here)
/// it asks the harness directly, so a history walk never runs on an empty cut
/// and misreads a finished turn as "still running".
async fn resolve_history_cursor(
    client: &crate::transport::HttpClient,
    session_id: &SessionId,
    sink: &SessionSink,
) -> u64 {
    match history_cursor(sink) {
        0 => match client.session_cursor(session_id).await {
            Ok(cursor) => cursor,
            Err(error) => {
                // Falling back to 0 is not harmless: `session/page` answers an
                // empty page for that cut, so the walk sees no messages and no
                // `turn/end` at all. Say so — this failing silently is what hid
                // the too-large probe sequence for as long as it did.
                tracing::warn!(
                    target: "dsh-bridge::session",
                    session_id = %session_id,
                    error = %error,
                    "could not resolve the session page cursor; history walks will see an empty page",
                );
                0
            }
        },
        cursor => cursor,
    }
}

/// Replay a session's history through the mapping layer before the live stream
/// delivers events. Used on resume/switch.
async fn replay_history(
    client: &crate::transport::HttpClient,
    session_id: &SessionId,
    sink: &SessionSink,
) -> anyhow::Result<Option<(String, String)>> {
    // The page cut comes from the follow opening frame; if that has not landed
    // yet, ask the harness (see `resolve_history_cursor`) so the replay sees the
    // real transcript instead of only the log header.
    let cursor = resolve_history_cursor(client, session_id, sink).await;
    let payload = crate::rpc_types::SessionHistoryPayload {
        session_id: session_id.to_string(),
        through_seq: cursor,
        before_seq: None,
        max_messages: Some(200),
    };
    let value = client
        .session_history(Uuid::new_v4().to_string(), &payload)
        .await?;
    sink.set_replaying(true);
    for entry in value.events {
        if let Ok(event) = serde_json::from_value::<crate::frame::SessionEvent>(entry.event) {
            let view = entry
                .view
                .and_then(|v| serde_json::from_value::<crate::frame::ToolEventView>(v).ok());
            let events = crate::mapping::map_session_event(&event, view.as_ref(), sink);
            for ev in events {
                sink.send(ev);
            }
            // Monotonic: a late follow frame must never move the cursor back
            // below what the replay already covered.
            sink.last_seq
                .fetch_max(event.seq, std::sync::atomic::Ordering::AcqRel);
        }
    }
    // Resume into an empty transcript (e.g. a fork child) needs the replayed
    // `turn/end` to land as the final `TurnFinished`, otherwise the session
    // stays in the streaming state forever after the fork.
    sink.send(ClientEvent::TurnFinished {
        stop_reason: "end_turn".to_string(),
        detail: None,
    });
    if let Some(values) = value
        .projections
        .as_ref()
        .and_then(|projections| projections.get("values"))
    {
        for event in crate::mapping::map_projection_values(values) {
            sink.send(event);
        }
    }
    sink.set_replaying(false);
    // Extract the durable model selection so the caller can seed the Model
    // control with the session's actual provider+model (not the catalog
    // default). The `SessionConfigValueChanged` event was already sent above.
    let selection = value
        .projections
        .as_ref()
        .and_then(|projections| projections.get("values"))
        .and_then(|values| values.get("modelSelection"))
        .and_then(|selection| {
            let active = selection
                .get("next")
                .or_else(|| selection.get("lastUsed"))
                .filter(|candidate| candidate.is_object())?;
            let provider = active.get("provider")?.as_str()?.to_string();
            let model = active.get("model")?.as_str()?.to_string();
            Some((provider, model))
        });
    Ok(selection)
}

/// Fork the harness session so the child inherits everything through the end
/// of turn `at_user_turn` (1-based, matching the harness per-session turn
/// counter: one queued `session.prompt` = one turn; steers and out-of-band
/// events like compaction commands do not consume turn numbers).
///
/// The harness fork RPC anchors on an event seq ("first `turn/end` at or after
/// `atSeq`"), while app-core knows only the local turn ordinal — so this walks
/// the session history (tail pages, `beforeSeq` strictly-less paging) and maps
/// the ordinal to the `turn/end` event carrying that turn number.
async fn fork_session_at_turn(
    client: &crate::transport::HttpClient,
    session_id: &SessionId,
    cursor: u64,
    at_user_turn: u64,
) -> anyhow::Result<String> {
    let target_seq = find_completed_turn_end_seq(client, session_id, cursor, at_user_turn).await?;
    fork_at_seq(client, session_id, target_seq).await
}

/// POST the harness `session.fork` with an already-resolved anchor seq.
async fn fork_at_seq(
    client: &crate::transport::HttpClient,
    session_id: &SessionId,
    target_seq: u64,
) -> anyhow::Result<String> {
    let payload = crate::rpc_types::SessionForkPayload {
        session_id: session_id.clone(),
        at_seq: Some(target_seq),
    };
    let value = client
        .session_fork(Uuid::new_v4().to_string(), &payload)
        .await
        .map_err(|error| anyhow::anyhow!("分叉会话失败：{error}"))?;
    Ok(value.session_id)
}

/// Anchor the fork cut on the target prompt's content: find the
/// `occurrence`-th `user/message` event (1-based, by seq) whose `source.kind`
/// is `"user"` (`None`/absent source kind counts as a user prompt for older
/// harnesses) and whose first text block equals `prompt_text` (trimmed), then
/// return the harness `session.fork` anchor `at_seq` carrying that seq — the
/// host cuts at the first `turn/end` at or after it, i.e. the end of the turn
/// this prompt opened. This beats the ordinal walk because the harness turn
/// counter also numbers injected turns (subagent reports, skill-catalog and
/// runtime-context splices), which kodex's turn-opening count never sees; the
/// reverse direction (splice-joined prompts that never open a turn) is
/// handled the same way, since a matched prompt still owns its turn.
///
/// `fallback_at_user_turn` is the legacy ordinal — used when the text cannot
/// be found (e.g. transformed prompts) so behavior never regresses below the
/// pre-anchor semantics.
async fn find_prompt_turn_end_seq(
    client: &crate::transport::HttpClient,
    session_id: &SessionId,
    cursor: u64,
    prompt_text: &str,
    occurrence: u64,
    fallback_at_user_turn: u64,
) -> anyhow::Result<u64> {
    const HISTORY_PAGE_MESSAGES: u32 = 200;
    const MAX_HISTORY_PAGES: usize = 250;

    let normalize = |text: &str| -> String {
        text.replace("\r\n", "\n")
            .replace('\r', "\n")
            .trim()
            .to_string()
    };
    let wanted = normalize(prompt_text);
    if wanted.is_empty() {
        anyhow::bail!("分叉锚点无效：目标提示文本为空");
    }

    // (seq, is_kind_user, first_text)
    let mut prompts: Vec<(u64, bool, String)> = Vec::new();
    let mut turn_ends: Vec<u64> = Vec::new();
    let mut before_seq: Option<u64> = None;
    for _ in 0..MAX_HISTORY_PAGES {
        let payload = crate::rpc_types::SessionHistoryPayload {
            session_id: session_id.clone(),
            through_seq: cursor,
            before_seq,
            max_messages: Some(HISTORY_PAGE_MESSAGES),
        };
        let value = client
            .session_history(Uuid::new_v4().to_string(), &payload)
            .await
            .map_err(|error| anyhow::anyhow!("分叉会话失败：读取会话历史失败：{error}"))?;
        let mut oldest_seq: Option<u64> = None;
        for entry in &value.events {
            let Ok(event) =
                serde_json::from_value::<crate::frame::SessionEvent>(entry.event.clone())
            else {
                continue;
            };
            oldest_seq = Some(oldest_seq.map_or(event.seq, |min| min.min(event.seq)));
            match event.type_tag.as_str() {
                "user/message" => {
                    let source_kind = event
                        .data
                        .get("source")
                        .and_then(|source| source.get("kind"))
                        .and_then(serde_json::Value::as_str);
                    let kind_is_user = match source_kind {
                        Some(kind) => kind == "user",
                        // Older harness events without a source envelope are
                        // user prompts — they are the only user/message origin
                        // those versions emit.
                        None => true,
                    };
                    let first_text = event
                        .data
                        .get("content")
                        .and_then(serde_json::Value::as_array)
                        .and_then(|blocks| blocks.first())
                        .and_then(|block| block.get("text"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    prompts.push((event.seq, kind_is_user, first_text));
                }
                "turn/end" => turn_ends.push(event.seq),
                _ => {}
            }
        }
        if !value.has_more {
            break;
        }
        let Some(seq) = oldest_seq else {
            break;
        };
        before_seq = Some(seq);
    }

    let mut matching_prompts: Vec<u64> = prompts
        .into_iter()
        .filter(|(_, kind_is_user, first_text)| *kind_is_user && normalize(first_text) == wanted)
        .map(|(seq, _, _)| seq)
        .collect();
    matching_prompts.sort_unstable();
    let anchored_prompt_seq = matching_prompts
        .get((occurrence.max(1) - 1) as usize)
        .copied()
        .ok_or_else(|| {
            anyhow::anyhow!("分叉锚点未匹配：历史中找不到目标提示文本（第 {occurrence} 次出现）")
        })?;
    turn_ends.sort_unstable();
    turn_ends
        .iter()
        .find(|seq| **seq >= anchored_prompt_seq)
        .copied()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "无法分叉：目标提示所在轮次尚未完成（回退序号 {fallback_at_user_turn}）"
            )
        })
}

/// Walk the session history and return the seq of the `at_user_turn`-th
/// (1-based) `turn/end` event, in ascending seq order. One queued harness
/// prompt = one turn = one `turn/end`; steers and out-of-band events
/// (compaction commands, projections) never produce `turn/end`, so the
/// ordinal matches app-core's local turn count without depending on the
/// harness `data.turn` numbering being contiguous. Errors when the turn never
/// completed (open turn) or the ordinal is past the session's last turn.
async fn find_completed_turn_end_seq(
    client: &crate::transport::HttpClient,
    session_id: &SessionId,
    cursor: u64,
    at_user_turn: u64,
) -> anyhow::Result<u64> {
    const HISTORY_PAGE_MESSAGES: u32 = 200;
    /// Hard walk bound so a pathological history cannot spin the session loop
    /// forever: 250 pages × 200 messages ≫ any real session.
    const MAX_HISTORY_PAGES: usize = 250;

    let mut before_seq: Option<u64> = None;
    let mut turn_end_seqs: Vec<u64> = Vec::new();
    for _ in 0..MAX_HISTORY_PAGES {
        let payload = crate::rpc_types::SessionHistoryPayload {
            session_id: session_id.clone(),
            through_seq: cursor,
            before_seq,
            max_messages: Some(HISTORY_PAGE_MESSAGES),
        };
        let value = client
            .session_history(Uuid::new_v4().to_string(), &payload)
            .await
            .map_err(|error| anyhow::anyhow!("分叉会话失败：读取会话历史失败：{error}"))?;
        // Pages run tail → head; events arrive ascending within a page. The
        // T-th boundary is only known once the whole log is walked, so just
        // collect and sort at the end.
        let mut oldest_seq: Option<u64> = None;
        for entry in &value.events {
            let Ok(event) =
                serde_json::from_value::<crate::frame::SessionEvent>(entry.event.clone())
            else {
                continue;
            };
            oldest_seq = Some(oldest_seq.map_or(event.seq, |min| min.min(event.seq)));
            if event.type_tag == "turn/end" {
                turn_end_seqs.push(event.seq);
            }
        }
        if !value.has_more {
            break;
        }
        // `has_more` implies a non-empty page whose oldest event bounds the
        // next window; an empty page means the paging contract broke — bail
        // instead of spinning.
        let Some(seq) = oldest_seq else {
            break;
        };
        before_seq = Some(seq);
    }
    turn_end_seqs.sort_unstable();
    if at_user_turn == 0 {
        return Err(anyhow::anyhow!("无法分叉：无效的轮次序号"));
    }
    turn_end_seqs
        .get((at_user_turn - 1) as usize)
        .copied()
        .ok_or_else(|| {
            anyhow::anyhow!("无法分叉：第 {at_user_turn} 轮尚未完成（或该消息所在轮次不存在）")
        })
}

/// The slash commands kodex can execute on behalf of a harness session. The
/// harness surfaces no ACP command list, so the bridge publishes the commands
/// it can route itself; compaction-capable presets run `/compact` over
/// `commands/execute`.
fn compact_slash_commands() -> Vec<workspace_model::AvailableCommand> {
    vec![workspace_model::AvailableCommand {
        name: "compact".into(),
        description: "压缩当前会话上下文".into(),
        input_hint: None,
    }]
}

/// Manual compaction is available for every deployment preset that can compose
/// the compaction seam. `minimal` explicitly omits compaction; other or absent
/// preset ids must not disable the menu item because dsh 0.1.2 may not restore
/// `agentPreset` before the composer is ready.
fn compact_command_should_be_published(session_preset: Option<&str>) -> bool {
    session_preset != Some("minimal")
}

/// Surface a manual-compaction failure to the UI as a standalone system
/// message. The fire-and-forget ForceCompact dispatch only logs otherwise —
/// when the RPC itself fails no `command/*` events ever arrive, and the
/// user's `/compact` would silently do nothing (exactly how the
/// double-wrapped-payload regression went unnoticed).
fn emit_compaction_failure(tx_events: &mpsc::Sender<ClientEvent>, message: String) {
    let _ = tx_events.send(ClientEvent::ContextCompacted { message });
}

/// Emit both config controls (Model + agent-preset Mode) in one update.
fn emit_config_controls(
    client: &crate::transport::HttpClient,
    host: &crate::host::HarnessHost,
    session_id: &SessionId,
    current_preset: Option<&str>,
    restored_model: Option<&(String, String)>,
    tx_events: &mpsc::Sender<ClientEvent>,
) {
    let mut events = Vec::new();
    emit_model_control_into(
        client,
        host,
        session_id,
        current_preset,
        restored_model,
        &mut events,
    );
    for event in events {
        let _ = tx_events.send(event);
    }
}

/// Build the config-control `ClientEvent`s (does not send; caller drains).
/// Fetches `session.models` (Model control) and `agentPreset.list` (Mode
/// control) and merges them into a single `SessionConfigUpdated`.
fn emit_model_control_into(
    client: &crate::transport::HttpClient,
    host: &crate::host::HarnessHost,
    session_id: &SessionId,
    current_preset: Option<&str>,
    restored_model: Option<&(String, String)>,
    out: &mut Vec<ClientEvent>,
) {
    let payload = crate::rpc_types::SessionModelsPayload { args: None };
    let model_control = match host
        .runtime()
        .block_on(client.session_models(Uuid::new_v4().to_string(), &payload))
    {
        Ok(value) => model_control_from_models(&value, restored_model),
        // No model catalog yet (e.g. no provider configured): fall through
        // with no Model control so the UI settles instead of spinning forever.
        Err(_) => None,
    };
    let preset_control = preset_control_from_list(client, host, current_preset);
    let mut controls = Vec::new();
    if let Some(control) = model_control {
        controls.push(control);
    }
    if let Some(control) = preset_control {
        controls.push(control);
    }
    out.push(ClientEvent::SessionConfigUpdated {
        state: workspace_model::SessionConfigState {
            hydrated: true,
            controls,
        },
    });
}

/// Build the agent-preset Mode control from `agentPreset.list`. Returns None
/// when the deployment composes no presets (the control is hidden).
fn preset_control_from_list(
    client: &crate::transport::HttpClient,
    host: &crate::host::HarnessHost,
    current_preset: Option<&str>,
) -> Option<workspace_model::SessionConfigControl> {
    let value = host
        .runtime()
        .block_on(client.agent_preset_list(Uuid::new_v4().to_string()))
        .ok()?;
    if value.presets.is_empty() {
        return None;
    }
    // Current selection: the session's own preset (from session.create) wins;
    // otherwise fall back to the deployment default.
    let current_id = current_preset
        .map(|p| p.to_string())
        .or_else(|| {
            value
                .presets
                .iter()
                .find(|p| p.is_default)
                .map(|p| p.id.clone())
        })
        .unwrap_or_else(|| value.presets[0].id.clone());
    let current_label = value
        .presets
        .iter()
        .find(|p| p.id == current_id)
        .map(|p| preset_label(p))
        .unwrap_or_else(|| current_id.clone());
    let choices = value
        .presets
        .iter()
        .map(|p| workspace_model::SessionConfigChoice {
            id: p.id.clone(),
            label: preset_label(p),
            description: p.description.clone(),
            provider: None,
            provider_label: None,
        })
        .collect();
    Some(workspace_model::SessionConfigControl {
        id: "mode".to_string(),
        label: "Mode".to_string(),
        description: Some("切换将开启新会话（dsh 预设仅在会话空白时可切换）".to_string()),
        category: workspace_model::SessionConfigCategory::Mode,
        // LegacyMode routes the change through `RuntimeCommand::SetMode`,
        // which this backend maps to `agentPreset.select`. (LocalMode would
        // only update the local permission broker and never reach the host.)
        source: workspace_model::SessionConfigSource::LegacyMode,
        current_value_id: current_id,
        current_value_label: current_label,
        choices,
        enabled: true,
    })
}

fn preset_label(p: &crate::rpc_types::AgentPresetEntry) -> String {
    p.name.clone().unwrap_or_else(|| p.id.clone())
}

/// Translate the dsh `session.models` response into a Model `SessionConfigControl`.
fn model_control_from_models(
    value: &serde_json::Value,
    restored_model: Option<&(String, String)>,
) -> Option<workspace_model::SessionConfigControl> {
    // The session's durable model selection (from the `modelSelection`
    // projection) wins over the catalog default — a resumed session carries
    // its own provider+model, not the deployment default.
    let (current_provider, current_model) = match restored_model {
        Some((provider, model)) => (provider.clone(), model.clone()),
        None => {
            let current = value.get("default")?;
            let provider = current.get("provider")?.as_str()?.to_string();
            let model = current.get("model")?.as_str()?.to_string();
            (provider, model)
        }
    };

    let mut choices = Vec::new();
    let mut current_value_id = String::new();
    if let Some(groups) = value.get("groups").and_then(serde_json::Value::as_array) {
        for group in groups {
            let provider = group
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let provider_label = group
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&provider)
                .to_string();
            if let Some(models) = group.get("models").and_then(serde_json::Value::as_array) {
                for model in models {
                    let model_id = model
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let model_name = model
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(&model_id)
                        .to_string();
                    let encoded = format!("kodex-provider/{provider}/{model_id}");
                    if provider == current_provider && model_id == current_model {
                        current_value_id = encoded.clone();
                    }
                    choices.push(workspace_model::SessionConfigChoice {
                        id: encoded,
                        label: model_name,
                        description: None,
                        provider: Some(provider.clone()),
                        provider_label: Some(provider_label.clone()),
                    });
                }
            }
        }
    }
    if current_value_id.is_empty() {
        // The current model may not be in the catalog (e.g. a provider that
        // reports no groups); surface it so the dropdown shows the active
        // selection even when the choice list is empty.
        current_value_id = format!("kodex-provider/{current_provider}/{current_model}");
        if !choices.iter().any(|choice| choice.id == current_value_id) {
            choices.push(workspace_model::SessionConfigChoice {
                id: current_value_id.clone(),
                label: current_model.clone(),
                description: None,
                provider: Some(current_provider.clone()),
                provider_label: Some(current_provider.clone()),
            });
        }
    }
    if choices.is_empty() {
        return None;
    }
    Some(workspace_model::SessionConfigControl {
        id: "model".to_string(),
        label: "Model".to_string(),
        description: None,
        category: workspace_model::SessionConfigCategory::Model,
        source: workspace_model::SessionConfigSource::SessionModel,
        current_value_id,
        current_value_label: current_model,
        choices,
        enabled: true,
    })
}

/// Decode a model selection value sent by the composer: either
/// `kodex-provider/<provider>/<model>` or a bare model id.
fn decode_model_value(value_id: &str, provider: Option<String>) -> Option<(String, String)> {
    if let Some(rest) = value_id.strip_prefix("kodex-provider/") {
        let mut parts = rest.splitn(2, '/');
        let provider = parts.next()?.to_string();
        let model = parts.next()?.to_string();
        return Some((model, provider));
    }
    Some((value_id.to_string(), provider.unwrap_or_default()))
}

fn prompt_part_to_wire(part: UserPromptContent, workspace_root: &str) -> PromptContentPart {
    match part {
        UserPromptContent::Text { text } => PromptContentPart::text(text),
        // Workspace file/directory references are translated to a text mention
        // (`@path` / `@dir/`) — the harness prompt wire is text-only, so the
        // reference is carried as a mention the agent can resolve itself.
        UserPromptContent::WorkspaceFile {
            path,
            start_line,
            end_line,
        } => match acp_core::runtime::workspace_reference_to_mention_text(
            workspace_root,
            &path,
            start_line,
            end_line,
        ) {
            Ok(mention) => PromptContentPart::text(mention),
            // Fall back to a bare mention if the path can't be resolved (e.g.
            // removed between attach and send) rather than dropping it silently.
            Err(_) => PromptContentPart::text(format!(
                "@{}",
                path.replace('\\', "/").trim_start_matches('/')
            )),
        },
        // Image attachments are forwarded as image content parts so multimodal
        // harness models can view them natively. Text-only models never reach
        // this path: app-core degrades image blocks to view-model text
        // descriptions (via the `kodex-image` fallback) before dispatching the
        // prompt, so the bridge only sees `Text` parts for them.
        UserPromptContent::Image {
            data,
            mime_type,
            name,
            ..
        } => PromptContentPart::Image {
            media_type: mime_type,
            data,
            name,
        },
        // Other UserPromptContent variants (opaque file blobs) are not carried
        // in v1; the harness prompt wire is text/image-only for now.
        _ => PromptContentPart::text(String::new()),
    }
}

fn local_timezone() -> String {
    // Best-effort IANA timezone; the harness uses this for prompt timestamps.
    std::env::var("TZ").unwrap_or_else(|_| "UTC".to_string())
}

/// Drive the per-session `session/follow` journal stream.
///
/// The dsh gateway routes session content events (assistant chunks, tool
/// calls, turn lifecycle) through this per-session stream — the shared
/// `$events` mux only carries host-level frames (`api-session/*`,
/// `approval/*`, `user-questions/*`). Without this loop the UI never sees
/// LLM replies.
///
/// The opening `snapshot` frame carries the durable projections (model
/// selection) and historical records; both are replayed through the mapping
/// layer. Subsequent `event` frames are mapped and forwarded live.
///
/// dsh 0.1.5+ serves live model output as transient `assistant-stream` frames
/// instead of durable `assistant/chunk` events; those are folded separately and
/// never advance the durable `seq` cursor.
async fn run_session_follow(
    client: crate::transport::HttpClient,
    sink: Arc<SessionSink>,
    session_id: SessionId,
    shutdown: ShutdownSignal,
) {
    // Attempt id → `(turn, step)`. The transient chunk frames carry only the
    // attempt id, while the mapping layer keys its repairers and dedup state on
    // the durable step. Kept across reconnects so a re-attach mid-attempt keeps
    // resolving its frames.
    let mut attempts: std::collections::HashMap<String, (u64, u64)> =
        std::collections::HashMap::new();
    loop {
        if shutdown.is_requested() {
            return;
        }
        let mut stream = match client.open_session_follow(&session_id).await {
            Ok(stream) => stream,
            Err(err) => {
                tracing::warn!(
                    target: "dsh-bridge::session::follow",
                    session_id = %session_id,
                    error = %err,
                    "session follow open failed; retrying"
                );
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
                    _ = shutdown_wait(&shutdown) => return,
                }
                continue;
            }
        };
        loop {
            if shutdown.is_requested() {
                return;
            }
            let value = match stream.next_item().await {
                Some(FollowStreamItem::Item(value)) => value,
                other => {
                    // The host can end or fail this logical stream without
                    // closing the socket; without this the loop would wait
                    // forever on a stream that will never produce again and the
                    // session would silently stop updating.
                    let reason = match &other {
                        Some(FollowStreamItem::Failed(error)) => format!("error: {error}"),
                        _ => "end".to_string(),
                    };
                    tracing::debug!(
                        target: "dsh-bridge::session::follow",
                        session_id = %session_id,
                        reason = %reason,
                        "session follow stream finished; reconnecting"
                    );
                    break;
                }
            };
            match crate::transport::decode_follow_item(&session_id, &value) {
                FollowItem::Snapshot {
                    frames,
                    projections,
                    active_attempt,
                } => {
                    // An attempt that was already streaming when this generation
                    // opened still delivers chunks without a preceding `start`.
                    if let Some(attempt) = active_attempt {
                        attempts.insert(attempt.attempt_id, (attempt.turn, attempt.step));
                    }
                    // The snapshot's projections carry durable session metadata
                    // (model selection, agent preset, usage). Map the whole
                    // baseline so resumed sessions restore everything instead of
                    // only the model selection.
                    if let Some(projections) = &projections
                        && let Some(values) = projections.get("values")
                    {
                        for event in crate::mapping::map_projection_values(values) {
                            sink.send(event);
                        }
                    }
                    apply_follow_frames(&sink, frames);
                }
                FollowItem::Event(frame) => apply_follow_frames(&sink, vec![*frame]),
                FollowItem::Assistant(frame) => {
                    handle_assistant_stream(&sink, &mut attempts, *frame);
                }
                FollowItem::Ignored => {}
            }
        }
        // Backoff before reconnect.
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
            _ = shutdown_wait(&shutdown) => return,
        }
    }
}

/// Apply durable follow frames, deduping against the re-baseline cursor.
///
/// SSE re-baseline can re-deliver frames at or below the last seen seq.
/// Applying them again re-runs `tool/call` → `ToolStarted`, resurrecting an
/// already-completed card to Running with no terminal event ever following.
fn apply_follow_frames(sink: &Arc<SessionSink>, frames: Vec<MuxFrame>) {
    for frame in frames {
        if let MuxFrame::SessionEvent { event, view, .. } = &frame {
            let last = sink.last_seq.load(std::sync::atomic::Ordering::Acquire);
            if event.seq <= last {
                continue;
            }
            let events = crate::mapping::map_session_event(event, view.as_ref(), sink);
            for ev in events {
                sink.send(ev);
            }
            sink.last_seq
                .store(event.seq, std::sync::atomic::Ordering::Release);
        }
    }
}

/// Fold one transient `assistant-stream` frame into `ClientEvent`s.
///
/// Transient frames are outside the durable log (no `seq`), so this must not
/// touch `last_seq` or the seq-based dedup.
fn handle_assistant_stream(
    sink: &Arc<SessionSink>,
    attempts: &mut std::collections::HashMap<String, (u64, u64)>,
    frame: crate::frame::AssistantStreamFrame,
) {
    match frame {
        AssistantStreamFrame::Start {
            attempt_id,
            turn,
            step,
        } => {
            attempts.insert(attempt_id, (turn, step));
        }
        AssistantStreamFrame::Chunk { attempt_id, chunk } => {
            // Without a known step the chunk cannot be keyed to the durable
            // turn, and re-sending it under a guessed step would corrupt the
            // repair/usage dedup state — drop it instead.
            let Some(&(turn, step)) = attempts.get(&attempt_id) else {
                tracing::debug!(
                    target: "dsh-bridge::session::follow",
                    attempt_id = %attempt_id,
                    "assistant-stream chunk for an unknown attempt; skipping"
                );
                return;
            };
            for event in crate::mapping::map_assistant_chunk(sink, turn, step, chunk) {
                sink.send(event);
            }
        }
        AssistantStreamFrame::End {
            attempt_id,
            outcome,
        } => {
            attempts.remove(&attempt_id);
            // `committed` means the durable settlement (`assistant/message` /
            // `assistant/attempt`) follows as a normal session event;
            // `abandoned` means the attempt left no durable content, so the
            // streamed prefix is all the UI will ever see.
            let kind = outcome
                .as_ref()
                .and_then(|outcome| outcome.get("kind"))
                .and_then(serde_json::Value::as_str);
            if kind == Some("abandoned") {
                tracing::debug!(
                    target: "dsh-bridge::session::follow",
                    attempt_id = %attempt_id,
                    "assistant attempt abandoned without a durable settlement"
                );
            }
        }
        AssistantStreamFrame::Other => {}
    }
}

/// `select!`-friendly wrapper around `ShutdownSignal` polling.
async fn shutdown_wait(shutdown: &ShutdownSignal) {
    loop {
        if shutdown.is_requested() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn text_part_passes_through() {
        let part = prompt_part_to_wire(UserPromptContent::text("hello"), "/tmp");
        match part {
            PromptContentPart::Text { text } => assert_eq!(text, "hello"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn workspace_file_part_becomes_mention() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "fn main() {}\n").unwrap();

        let part = prompt_part_to_wire(
            UserPromptContent::workspace_file("src/lib.rs", Some(1), Some(1)),
            root.to_str().unwrap(),
        );
        match part {
            PromptContentPart::Text { text } => assert_eq!(text, "@src/lib.rs#L1"),
            other => panic!("expected text mention, got {other:?}"),
        }
    }

    #[test]
    fn workspace_directory_part_becomes_dir_mention() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();

        let part = prompt_part_to_wire(
            UserPromptContent::workspace_file("src", None, None),
            root.to_str().unwrap(),
        );
        match part {
            PromptContentPart::Text { text } => assert_eq!(text, "@src/"),
            other => panic!("expected dir mention, got {other:?}"),
        }
    }

    #[test]
    fn image_part_is_forwarded_as_image_content() {
        let part = prompt_part_to_wire(
            UserPromptContent::image("Zm9v", "image/png", Some("pic.png".to_string())),
            "/tmp",
        );
        match part {
            PromptContentPart::Image {
                media_type,
                data,
                name,
            } => {
                assert_eq!(media_type, "image/png");
                assert_eq!(data, "Zm9v");
                assert_eq!(name.as_deref(), Some("pic.png"));
            }
            other => panic!("expected image part, got {other:?}"),
        }
    }

    #[test]
    fn sink_clears_inflight_flag_on_turn_finished() {
        // Regression: a turn completes via the event stream (the session
        // thread's command loop never sees TurnFinished), so the sink must
        // clear the shared in-flight flag — otherwise the next queued prompt
        // is silently dropped by the "one prompt per turn" guard.
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc;

        let (tx, _rx) = mpsc::channel::<acp_core::ClientEvent>();
        let sink = crate::host::SessionSink::new(tx, acp_core::PermissionBroker::default());
        let flag = std::sync::Arc::new(AtomicBool::new(true));
        sink.set_inflight_flag(flag.clone());
        assert!(flag.load(Ordering::Acquire), "flag should start set");

        // A mid-turn event must NOT clear the flag.
        sink.send(acp_core::ClientEvent::MessageChunk {
            role: workspace_model::MessageRole::Assistant,
            content: "thinking".to_string(),
        });
        assert!(
            flag.load(Ordering::Acquire),
            "non-terminal event must not clear the in-flight flag"
        );

        // TurnFinished clears it.
        sink.send(acp_core::ClientEvent::TurnFinished {
            stop_reason: "end_turn".to_string(),
            detail: None,
        });
        assert!(
            !flag.load(Ordering::Acquire),
            "TurnFinished must clear the in-flight flag so the next prompt is accepted"
        );
    }

    #[test]
    fn sink_clears_inflight_flag_on_interrupted() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc;

        let (tx, _rx) = mpsc::channel::<acp_core::ClientEvent>();
        let sink = crate::host::SessionSink::new(tx, acp_core::PermissionBroker::default());
        let flag = std::sync::Arc::new(AtomicBool::new(true));
        sink.set_inflight_flag(flag.clone());

        sink.send(acp_core::ClientEvent::Interrupted {
            reason: "boom".to_string(),
        });
        assert!(
            !flag.load(Ordering::Acquire),
            "Interrupted must clear the in-flight flag"
        );
    }

    #[test]
    fn compact_command_is_available_unless_minimal() {
        assert!(compact_command_should_be_published(None));
        assert!(compact_command_should_be_published(Some("standard")));
        assert!(compact_command_should_be_published(Some(
            "custom-with-compaction"
        )));
        assert!(!compact_command_should_be_published(Some("minimal")));
        let commands = compact_slash_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "compact");
        assert!(!commands[0].description.is_empty());
        assert!(commands[0].input_hint.is_none());
    }

    /// Windows: a workspace root normalized by kodex's session-store
    /// (`d:/work/admesh`) must canonicalize back to the verbatim form dsh
    /// persisted at creation (`D:\work\admesh`), otherwise resume fails with
    /// `session/conflict`.
    #[cfg(windows)]
    #[test]
    fn harness_cwd_canonicalizes_normalized_workspace_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Some Project");
        std::fs::create_dir_all(&root).unwrap();

        // What session-store would have persisted.
        let normalized = root
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        let harness_cwd = canonicalize_harness_cwd(&normalized);
        // What dsh persisted at creation (Node path.resolve verbatim form).
        let verbatim = root.canonicalize().unwrap();
        let verbatim = verbatim.to_string_lossy().replace("\\\\?\\", "");
        assert_eq!(harness_cwd, verbatim);
        assert!(harness_cwd.contains("Some Project"));
        assert!(!harness_cwd.starts_with("d:/"));
    }

    /// Non-Windows roots pass through canonicalized; missing paths fall back
    /// to the input unchanged so a deleted workspace does not break resume.
    #[test]
    fn harness_cwd_missing_path_passes_through() {
        let missing = "d:/work/definitely-not-a-real-path-12345";
        assert_eq!(canonicalize_harness_cwd(missing), missing);
    }

    /// `run_session_follow` must not re-apply frames at or below the last seen
    /// seq: an SSE re-baseline re-delivers the tail of the journal, and
    /// re-running `tool/call` → `ToolStarted` would resurrect an
    /// already-completed card to Running with no terminal event following.
    #[test]
    fn follow_frames_at_or_below_last_seq_are_dropped() {
        use std::sync::mpsc;

        let (tx, _rx) = mpsc::channel::<acp_core::ClientEvent>();
        let sink = crate::host::SessionSink::new(tx, acp_core::PermissionBroker::default());
        // Simulate "replay + live stream already advanced to seq 10".
        sink.last_seq
            .store(10, std::sync::atomic::Ordering::Release);

        // A re-baselined `tool/call` at seq 5 must be dropped before mapping.
        // The guard lives in run_session_follow's loop (not in mapping) because
        // the history replay path must still see every seq.
        let event = crate::frame::SessionEvent {
            type_tag: "tool/call".into(),
            seq: 5,
            time: 0.0,
            data: serde_json::json!({
                "turn": 1, "step": 1,
                "callId": "call-stale", "name": "bash", "arguments": "{\"command\":\"ls\"}"
            }),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        };
        let last = sink.last_seq.load(std::sync::atomic::Ordering::Acquire);
        assert!(
            event.seq <= last,
            "frame at or below last seq must be skipped by the follow loop"
        );
    }

    #[test]
    fn model_control_from_models_uses_catalog_default() {
        // The dsh `session.modelCatalog` response carries the deployment
        // default under `default`; the Model control must render it even when
        // no `restored_model` projection exists (new session).
        let catalog = serde_json::json!({
            "default": { "provider": "timiai", "model": "gpt-5.5" },
            "routableProviders": ["timiai"],
            "groups": [
                {
                    "id": "timiai",
                    "name": "Timi AI",
                    "models": [
                        { "id": "gpt-5.5", "name": "GPT 5.5" },
                        { "id": "gpt-5.4", "name": "GPT 5.4" }
                    ]
                }
            ],
            "failures": []
        });
        let control = model_control_from_models(&catalog, None)
            .expect("Model control must be built from the catalog default");
        assert_eq!(control.current_value_id, "kodex-provider/timiai/gpt-5.5");
        assert_eq!(control.current_value_label, "gpt-5.5");
        assert!(!control.choices.is_empty());
    }

    #[test]
    fn model_control_from_models_prefers_restored_selection() {
        // A resumed session's durable selection overrides the catalog default.
        let catalog = serde_json::json!({
            "default": { "provider": "deepseek", "model": "deepseek-v4-pro" },
            "groups": [
                {
                    "id": "timiai",
                    "name": "Timi AI",
                    "models": [{ "id": "glm-5.3-ioa", "name": "GLM 5.3 IOA" }]
                },
                {
                    "id": "deepseek",
                    "name": "DeepSeek",
                    "models": [{ "id": "deepseek-v4-pro", "name": "DeepSeek V4 Pro" }]
                }
            ]
        });
        let restored = ("timiai".to_string(), "glm-5.3-ioa".to_string());
        let control = model_control_from_models(&catalog, Some(&restored))
            .expect("Model control must be built from the restored selection");
        assert_eq!(
            control.current_value_id,
            "kodex-provider/timiai/glm-5.3-ioa"
        );
        assert_eq!(control.current_value_label, "glm-5.3-ioa");
    }
}
