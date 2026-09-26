//! Shared `HarnessHost` + `SessionRouter` + `HarnessHostRegistry` (Mode B).
//!
//! One `HarnessHost` owns one `dsh web` process (or one external endpoint), one
//! mux + one host WebSocket connection, a dedicated multi-thread tokio runtime, and a
//! `SessionRouter`. Multiple Kodex `SessionHandle`s targeting the same endpoint
//! share the host: the mux/host WebSocket read loops run on the host's own runtime
//! (outliving any single session), and frames are demuxed by `sessionId` into
//! per-session sinks. Control POSTs go direct over the shared `reqwest` client
//! (not serialized through the router).

use acp_core::{ClientEvent, PermissionBroker};
use dashmap::DashMap;
use futures::StreamExt;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::sync::Notify;

use crate::approval::PendingApprovals;
use crate::frame::{ControlFrame, HostFrame, MuxFrame};
use crate::mojibake::{StreamRepairer, StreamTextKind};
use crate::rpc_types::{AnswerProtocol, RpcId, ServerRequest, SessionId};
use crate::transport::{HttpClient, SseStream};

/// Bound on the re-baseline fan-out (`session.history` calls) after a stream drop.
/// Matches the design doc's `REBASELINE_CONCURRENCY` default (4) for the
/// typical 1–3 session desktop case.
const REBASELINE_CONCURRENCY: usize = 4;

/// Reconnect backoff bounds for the optional `session/control` stream.
/// Harnesses before dsh 0.1.5 have no such endpoint, so repeated failures must
/// back off (and stop logging at `warn`) instead of hot-looping against a host
/// that will never serve it — their projections still arrive on the `$events`
/// mux.
const CONTROL_BACKOFF_BASE: Duration = Duration::from_millis(500);
const CONTROL_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Grace window before an ambiguous `api-session/status(<id>, false)` is
/// reported as a mid-turn interruption. The harness emits that idle
/// transition right after appending the turn's durable `turn/end`, but the two
/// travel on different streams (the idle transition on the `$events` mux, the
/// `turn/end` on the per-session `session/follow` journal) and can be
/// processed in either order — the status frame regularly wins the race. A
/// `turn/end` that lands within this window cancels the pending interruption;
/// only a turn that never settles is one the host actually stopped.
const TURN_END_SETTLE_GRACE: Duration = Duration::from_millis(1500);

/// One pending approval/question entry kind, recorded in the session sink so
/// the answer path can route the user's decision back to the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingApprovalKind {
    Approval,
    Question,
}

/// Per-session state held by the `SessionRouter`: the event channel back to the
/// `SessionHandle`, the permission broker, the last-delivered `seq` (for
/// re-baseline), and the pending approval/question table (keyed by the dsh
/// `rpcId`/`approvalId` surfaced to the UI).
pub struct SessionSink {
    pub tx_events: mpsc::Sender<ClientEvent>,
    pub permission_broker: PermissionBroker,
    /// Gateway-assigned id of the `$events` mux stream. Captured from the
    /// stream's `ready` item and echoed when resolving a `waterfall` request.
    pub last_seq: AtomicU64,
    /// Set while history replay is feeding events (resume / stream-gap
    /// re-baseline). History pages contain both the raw `assistant/chunk`
    /// deltas and the finalized `assistant/message`, so the mapping layer
    /// tracks which assistant steps already streamed their text and suppress
    /// the finalized block to avoid duplicates.
    replaying: AtomicBool,
    /// Assistant steps (`(turn, step)`) for which this sink has already emitted
    /// assistant text via `assistant/chunk` text-deltas. Kept across live and
    /// replay passes so a re-baseline that sees only the finalized
    /// `assistant/message` after a mid-stream gap does not append the full text
    /// over text already shown.
    streamed_text_steps: Mutex<std::collections::HashSet<(u64, u64)>>,
    /// Assistant steps (`(turn, step)`) whose per-call `TokenUsage` this sink
    /// has already emitted as a `TurnDelta` usage event. dsh surfaces the same
    /// model call's usage twice — a terminal `assistant/chunk`
    /// `{type:"usage"}` stream chunk and a finalized `assistant/message`
    /// `usage` rollup — and history replay (resume / stream-gap re-baseline)
    /// re-delivers both, so the mapping layer claims each step's usage exactly
    /// once per sink lifetime. Without this, every resume/reconnect re-appends
    /// the session's whole usage history (observed as 2×/4×/…/18× inflated
    /// `usage_events` rows).
    usage_emitted_steps: Mutex<std::collections::HashSet<(u64, u64)>>,
    /// Streaming mojibake repairers keyed by `(turn, step, block index)` — one
    /// per assistant text/reasoning block stream. A corrupted upstream stream
    /// delivers one Latin-1 char per delta, so repair needs cross-delta state
    /// (see `crate::mojibake`); entries are removed when the step's finalized
    /// `assistant/message` flushes them.
    stream_repairs: Mutex<StreamRepairTable>,
    /// Tool-call arguments keyed by call id, captured while mapping `tool/call`
    /// so a later `tool/result` can still synthesize diff previews for file
    /// editor tools whose dsh card variant did not carry a diff view.
    tool_call_args: Mutex<std::collections::HashMap<String, (String, serde_json::Value)>>,
    /// Pending approvals/questions keyed by the id surfaced to the UI (dsh
    /// `approvalId` for approvals — or, on dsh 0.1.5+, the forwarded
    /// waterfall's `eventId`; the question batch's first `question.id` for
    /// questions). The value carries the dsh `rpcId` the legacy `/api/respond`
    /// carrier echoes, while the `$events` waterfall `eventId` needed by
    /// `/api/$events/result` is stored separately in `waterfall_event_ids`.
    pending: Mutex<Vec<PendingEntry>>,
    /// Map UI request id → `$events` waterfall `eventId`, for question batches
    /// (whose UI id is the first question id) and for approvals forwarded as
    /// waterfalls. dsh resolves waterfall results by `eventId`, not by the
    /// UI-facing id.
    waterfall_event_ids: Mutex<Vec<(String, RpcId)>>,
    /// The ordered question ids of each pending batch, keyed by UI request id.
    /// dsh's `matchesQuestions` validates answers positionally
    /// (`answer[i].id === questions[i].id`), so the respond payload must list
    /// answers in the exact order the questions were asked.
    question_order: Mutex<std::collections::HashMap<String, Vec<String>>>,
    /// The dsh session id, set once `session.create` returns.
    session_id: Mutex<Option<SessionId>>,
    /// Command ids of tracked `compact` command runs, recorded when the
    /// `command/run` session event names the compact command so the paired
    /// `command/done` outcome can be mapped to the compaction notice. Capped:
    /// a run always settles, but a stream tear mid-run must not grow the
    /// table unbounded.
    compact_commands: Mutex<std::collections::VecDeque<String>>,
    /// Set when the session has been removed by the host (`host/session-removed`)
    /// during a stream gap, so re-baseline skips it and marks it Interrupted.
    removed: AtomicBool,
    /// Shared "a prompt turn is in flight" flag, owned by the session thread
    /// and cleared here when a `TurnFinished`/`Interrupted` event is sent to
    /// app-core. The session thread checks this before queueing a new prompt so
    /// a completed turn (whose completion flows through the sink, not the
    /// command loop) does not leave a stale guard that silently drops the next
    /// prompt. `None` until the session thread attaches it via
    /// [`SessionSink::set_inflight_flag`].
    inflight: Mutex<Option<Arc<AtomicBool>>>,
    /// Whether a prompt turn is currently in flight, maintained from the
    /// durable `turn/start` / `turn/end` session events. The harness emits
    /// `api-session/status(<sessionId>, false)` both when a turn finishes
    /// normally (the agent goes running→idle at every turn end) and when a
    /// host-side failure stops one mid-turn; this flag is the only signal that
    /// distinguishes them.
    turn_active: AtomicBool,
    /// Bumped on every `turn/start` / `turn/end` boundary. A deferred
    /// interruption decision (see `TURN_END_SETTLE_GRACE`) captures this epoch
    /// and only fires while it is unchanged, so a `turn/end` — or a whole new
    /// turn — that settles during the grace window cancels it.
    turn_epoch: AtomicU64,
}

/// Per-block-stream mojibake repairer table: `(turn, step, block index)` maps
/// to the stream kind plus its [`StreamRepairer`] state.
type StreamRepairTable =
    std::collections::HashMap<(u64, u64, u64), (StreamTextKind, StreamRepairer)>;

#[derive(Debug, Clone)]
pub struct PendingEntry {
    pub ui_id: String,
    pub kind: PendingApprovalKind,
    pub approval_id: String,
}

impl SessionSink {
    pub fn new(tx_events: mpsc::Sender<ClientEvent>, broker: PermissionBroker) -> Self {
        Self {
            tx_events,
            permission_broker: broker,
            last_seq: AtomicU64::new(0),
            replaying: AtomicBool::new(false),
            streamed_text_steps: Mutex::new(std::collections::HashSet::new()),
            usage_emitted_steps: Mutex::new(std::collections::HashSet::new()),
            stream_repairs: Mutex::new(StreamRepairTable::new()),
            tool_call_args: Mutex::new(std::collections::HashMap::new()),
            pending: Mutex::new(Vec::new()),
            waterfall_event_ids: Mutex::new(Vec::new()),
            question_order: Mutex::new(std::collections::HashMap::new()),
            session_id: Mutex::new(None),
            compact_commands: Mutex::new(std::collections::VecDeque::new()),
            removed: AtomicBool::new(false),
            inflight: Mutex::new(None),
            turn_active: AtomicBool::new(false),
            turn_epoch: AtomicU64::new(0),
        }
    }

    /// Receiver-less sink for mapping projection baselines, where the caller
    /// only inspects the returned client events and never sends them.
    pub fn new_for_projection_mapping() -> Self {
        let (tx, _rx) = mpsc::channel();
        Self::new(tx, PermissionBroker::default())
    }

    pub fn set_replaying(&self, active: bool) {
        self.replaying.store(active, Ordering::Release);
    }

    pub fn is_replaying(&self) -> bool {
        self.replaying.load(Ordering::Acquire)
    }

    /// Record that assistant text for a step has been emitted through
    /// `assistant/chunk` text-deltas (live or replay).
    pub fn mark_text_seen(&self, turn: u64, step: u64) {
        if let Ok(mut seen) = self.streamed_text_steps.lock() {
            seen.insert((turn, step));
        }
    }

    /// Feed one assistant text/reasoning delta through the block stream's
    /// mojibake repairer; returns the text safe to emit now (possibly empty
    /// while a corrupted sequence is still accumulating). On lock failure the
    /// delta passes through unrepaired.
    pub fn repair_stream_text(
        &self,
        turn: u64,
        step: u64,
        index: u64,
        kind: StreamTextKind,
        delta: &str,
    ) -> String {
        match self.stream_repairs.lock() {
            Ok(mut repairs) => {
                let entry = repairs
                    .entry((turn, step, index))
                    .or_insert_with(|| (kind, StreamRepairer::new()));
                entry.1.push(delta)
            }
            Err(_) => delta.to_string(),
        }
    }

    /// Flush the held-back repair tails of all block streams of a step (called
    /// when the step's finalized `assistant/message` arrives) and drop their
    /// repairers. Returns the non-empty tails with their stream kind.
    pub fn flush_stream_repairs(&self, turn: u64, step: u64) -> Vec<(StreamTextKind, String)> {
        let mut out = Vec::new();
        if let Ok(mut repairs) = self.stream_repairs.lock() {
            let keys: Vec<(u64, u64, u64)> = repairs
                .keys()
                .filter(|k| k.0 == turn && k.1 == step)
                .copied()
                .collect();
            for key in keys {
                if let Some((kind, mut repairer)) = repairs.remove(&key) {
                    let tail = repairer.flush();
                    if !tail.is_empty() {
                        out.push((kind, tail));
                    }
                }
            }
        }
        out
    }

    /// Whether assistant text for the step has already been emitted through
    /// chunks.
    pub fn text_seen(&self, turn: u64, step: u64) -> bool {
        self.streamed_text_steps
            .lock()
            .ok()
            .map(|seen| seen.contains(&(turn, step)))
            .unwrap_or(false)
    }

    /// Claim a step's per-call usage emission. Returns `true` on the first
    /// claim (the caller must emit the `TurnDelta` usage event); `false` when
    /// this sink already emitted usage for the step (duplicate chunk/rollup
    /// delivery, or a history replay pass re-delivering the same call).
    /// Fail-open on lock poisoning: emitting a duplicate is recoverable,
    /// silently dropping usage is not.
    pub fn claim_usage_emission(&self, turn: u64, step: u64) -> bool {
        match self.usage_emitted_steps.lock() {
            Ok(mut seen) => seen.insert((turn, step)),
            Err(_) => true,
        }
    }

    /// Record a tool call's parsed arguments for later synthetic diff rendering.
    pub fn record_tool_call(&self, call_id: String, name: String, args: serde_json::Value) {
        if let Ok(mut calls) = self.tool_call_args.lock() {
            calls.insert(call_id, (name, args));
        }
    }

    /// Take and remove one tool call's recorded name/args. Returns the raw
    /// arguments object when it parses to a JSON object.
    pub fn take_tool_call(
        &self,
        call_id: &str,
    ) -> Option<(String, serde_json::Map<String, serde_json::Value>)> {
        self.tool_call_args
            .lock()
            .ok()
            .and_then(|mut calls| calls.remove(call_id))
            .and_then(|(name, args)| args.as_object().map(|map| (name, map.clone())))
    }

    pub fn set_session_id(&self, id: SessionId) {
        if let Ok(mut guard) = self.session_id.lock() {
            *guard = Some(id);
        }
    }

    pub fn session_id(&self) -> Option<SessionId> {
        self.session_id.lock().ok().and_then(|g| g.clone())
    }

    /// Track a `compact` command run by its `command/run` command id, so the
    /// paired `command/done` outcome maps to the compaction notice.
    pub fn track_compact_command(&self, command_id: String) {
        const COMPACT_COMMAND_TRACK_CAP: usize = 32;
        if let Ok(mut guard) = self.compact_commands.lock() {
            if guard.len() >= COMPACT_COMMAND_TRACK_CAP {
                guard.pop_front();
            }
            guard.push_back(command_id);
        }
    }

    /// Whether the id belongs to a tracked `compact` run (removes it).
    pub fn take_compact_command(&self, command_id: &str) -> bool {
        self.compact_commands.lock().ok().is_some_and(|mut guard| {
            let before = guard.len();
            guard.retain(|id| id != command_id);
            guard.len() != before
        })
    }

    pub fn mark_removed(&self) {
        self.removed.store(true, Ordering::Release);
    }

    pub fn is_removed(&self) -> bool {
        self.removed.load(Ordering::Acquire)
    }

    /// Record a durable turn boundary (`turn/start` / `turn/end`).
    pub fn set_turn_active(&self, active: bool) {
        self.turn_epoch.fetch_add(1, Ordering::Release);
        self.turn_active.store(active, Ordering::Release);
    }

    /// Whether a turn is in flight for this session. See the field docs for why
    /// the host-status mapping needs it.
    pub fn is_turn_active(&self) -> bool {
        self.turn_active.load(Ordering::Acquire)
    }

    /// Current turn-boundary epoch. See the field docs for how a deferred
    /// interruption decision uses it.
    pub fn turn_epoch(&self) -> u64 {
        self.turn_epoch.load(Ordering::Acquire)
    }

    /// Attach the shared in-flight flag owned by the session thread. The sink
    /// clears it when a `TurnFinished`/`Interrupted` reaches app-core, so the
    /// session thread's "one prompt per turn" guard stays in sync with turns
    /// that complete via the event stream (not the command loop).
    pub fn set_inflight_flag(&self, flag: Arc<AtomicBool>) {
        if let Ok(mut guard) = self.inflight.lock() {
            *guard = Some(flag);
        }
    }

    fn clear_inflight_if_turn_done(&self, event: &ClientEvent) {
        if matches!(
            event,
            ClientEvent::TurnFinished { .. } | ClientEvent::Interrupted { .. }
        ) {
            if let Ok(guard) = self.inflight.lock() {
                if let Some(flag) = guard.as_ref() {
                    flag.store(false, Ordering::Release);
                }
            }
        }
    }

    pub fn send(&self, event: ClientEvent) {
        // A turn completion / interruption flows through the sink (the session
        // thread's command loop never sees these events), so clear the shared
        // in-flight flag here before forwarding the event to app-core.
        self.clear_inflight_if_turn_done(&event);
        // A send error means the SessionHandle dropped its receiver; the router
        // will unregister it shortly. Log and continue.
        let _ = self.tx_events.send(event);
    }

    pub fn record_pending_approval(&self, approval_id: String, kind: PendingApprovalKind) {
        if let Ok(mut guard) = self.pending.lock() {
            guard.push(PendingEntry {
                ui_id: approval_id.clone(),
                kind,
                approval_id,
            });
        }
    }

    pub fn record_pending_question(&self, ui_id: String) {
        // The rpcId is attached later when the session loop receives the
        // `question/requested` ServerRequest (it has the rpcId); for now we
        // record a placeholder so the UI id is known.
        if let Ok(mut guard) = self.pending.lock() {
            guard.push(PendingEntry {
                ui_id: ui_id.clone(),
                kind: PendingApprovalKind::Question,
                approval_id: ui_id,
            });
        }
    }

    /// Record the ordered question ids for a pending batch, so the respond
    /// payload lists answers in the same positional order dsh validates.
    pub fn record_question_order(&self, ui_id: String, order: Vec<String>) {
        if let Ok(mut guard) = self.question_order.lock() {
            guard.insert(ui_id, order);
        }
    }

    /// The ordered question ids for a pending batch (empty when unknown).
    pub fn question_order(&self, ui_id: &str) -> Vec<String> {
        self.question_order
            .lock()
            .ok()
            .and_then(|g| g.get(ui_id).cloned())
            .unwrap_or_default()
    }

    /// Record the `$events` waterfall `eventId` behind one UI-facing request id,
    /// so the answer can be correlated by the gateway (`question/requested` and
    /// a dsh 0.1.5+ `approval/request` are forwarded waterfalls, and the answer
    /// must echo their `eventId`).
    pub fn attach_waterfall_event_id(&self, ui_id: String, event_id: RpcId) {
        if let Ok(mut guard) = self.waterfall_event_ids.lock() {
            guard.retain(|(id, _)| id != &ui_id);
            guard.push((ui_id, event_id));
        }
    }

    /// Remove a pending approval once the harness reports it resolved.
    pub fn clear_pending_approval(&self, ui_id: &str) {
        if let Ok(mut guard) = self.waterfall_event_ids.lock() {
            guard.retain(|(id, _)| id != ui_id);
        }
        if let Ok(mut pending) = self.pending.lock() {
            pending.retain(|e| !(e.kind == PendingApprovalKind::Approval && e.ui_id == ui_id));
        }
    }

    /// Remove a pending question batch once the harness reports it resolved.
    pub fn clear_pending_question(&self, ui_id: &str) {
        if let Ok(mut guard) = self.waterfall_event_ids.lock() {
            guard.retain(|(id, _)| id != ui_id);
        }
        if let Ok(mut guard) = self.question_order.lock() {
            guard.remove(ui_id);
        }
        if let Ok(mut pending) = self.pending.lock() {
            pending.retain(|e| !(e.kind == PendingApprovalKind::Question && e.ui_id == ui_id));
        }
    }

    /// The UI-facing request id for a pending waterfall, resolved from its
    /// `eventId` — `question/resolved` names the batch by that id, while
    /// `ToolPermissionResolved` must carry the id the UI registered.
    pub fn waterfall_ui_id_for_event_id(&self, event_id: &str) -> Option<String> {
        self.waterfall_event_ids.lock().ok().and_then(|guard| {
            guard
                .iter()
                .find(|(_, id)| id == event_id)
                .map(|(ui_id, _)| ui_id.clone())
        })
    }

    pub fn pending_approvals(&self) -> PendingApprovals {
        let entries = self.pending.lock().map(|g| g.clone()).unwrap_or_default();
        let waterfall_event_ids = self
            .waterfall_event_ids
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        PendingApprovals::from_entries(entries, waterfall_event_ids)
    }
}

/// Demuxes host-level WebSocket frames by `sessionId` to per-session sinks.
/// Frames whose `sessionId` is not registered are dropped with a debug log
/// (not errors — e.g. a session created by another client of the same host).
pub struct SessionRouter {
    sinks: DashMap<SessionId, Arc<SessionSink>>,
    /// Set when the host startup probe failed. Any sink registered afterwards
    /// is immediately told (the probe may have raced ahead of registration).
    probe_error: Mutex<Option<String>>,
}

impl SessionRouter {
    pub fn new() -> Self {
        Self {
            sinks: DashMap::new(),
            probe_error: Mutex::new(None),
        }
    }

    /// Record a startup-probe failure so late-registered sinks are told too.
    pub fn set_probe_error(&self, error: String) {
        if let Ok(mut guard) = self.probe_error.lock() {
            *guard = Some(error);
        }
    }

    pub fn register(&self, session_id: SessionId, sink: Arc<SessionSink>) {
        if let Ok(guard) = self.probe_error.lock()
            && let Some(error) = guard.as_ref()
        {
            sink.send(ClientEvent::Interrupted {
                reason: format!("harness host unreachable: {error}"),
            });
        }
        self.sinks.insert(session_id, sink);
    }

    pub fn unregister(&self, session_id: &SessionId) {
        self.sinks.remove(session_id);
    }

    pub fn get(&self, session_id: &SessionId) -> Option<Arc<SessionSink>> {
        self.sinks.get(session_id).map(|r| r.clone())
    }

    /// Snapshot all live sessions for re-baseline after a stream drop.
    pub fn live_sessions(&self) -> Vec<(SessionId, Arc<SessionSink>)> {
        self.sinks
            .iter()
            .map(|r| (r.key().clone(), r.value().clone()))
            .collect()
    }

    /// Broadcast a host-global event to every sink (e.g. a fatal reopen
    /// failure). Used when the mux stream cannot be reopened.
    pub fn broadcast(&self, event: ClientEvent) {
        for entry in self.sinks.iter() {
            entry.value().send(event.clone());
        }
    }
}

impl Default for SessionRouter {
    fn default() -> Self {
        Self::new()
    }
}

/// One harness host: a shared `HttpClient`, a dedicated multi-thread `Runtime`,
/// the mux + host WebSocket read loops, and a `SessionRouter`. Refcounted by active
/// sessions; the last drop closes the WebSocket streams, disposes the runtime, and
/// kills the spawned `dsh web` process (if Kodex spawned it).
pub struct HarnessHost {
    endpoint: String,
    client: HttpClient,
    router: Arc<SessionRouter>,
    /// Gateway-assigned client id from the `$events` mux stream. Shared across
    /// all sessions on this host (one mux connection per host).
    remote_event_client_id: Mutex<Option<String>>,
    runtime: Runtime,
    /// Notified when the WebSocket loops should stop (last session dropped or
    /// shutdown). Drives cancellation of the read loops.
    stop: Arc<Notify>,
    /// Whether Kodex owns a child attached to this host. Only owned children
    /// are killed on last-drop; external endpoints are never terminated.
    spawned: AtomicBool,
    /// Optional child handle for a Kodex-spawned `dsh web` process.
    child: Mutex<Option<crate::process::DshChild>>,
    /// Host version recorded by the startup probe (for diagnostics/branching).
    version: Mutex<Option<String>>,
    /// Prevents double-teardown.
    torn_down: AtomicBool,
}

impl std::fmt::Debug for HarnessHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarnessHost")
            .field("endpoint", &self.endpoint)
            .field("spawned", &self.spawned.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl HarnessHost {
    /// Connect to an external endpoint (no process spawn).
    pub fn connect(endpoint: String) -> anyhow::Result<Arc<Self>> {
        Self::build(endpoint, false)
    }

    fn build(endpoint: String, spawned: bool) -> anyhow::Result<Arc<Self>> {
        let client = HttpClient::new(&endpoint)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build host runtime: {e}"))?;
        let host = Arc::new(Self {
            endpoint,
            client,
            router: Arc::new(SessionRouter::new()),
            remote_event_client_id: Mutex::new(None),
            runtime,
            stop: Arc::new(Notify::new()),
            spawned: AtomicBool::new(spawned),
            child: Mutex::new(None),
            version: Mutex::new(None),
            torn_down: AtomicBool::new(false),
        });
        // Startup probe: confirm the endpoint is a harness host and record the
        // version. Fail fast with a diagnostic instead of hanging on SSE.
        let probed = host.clone();
        host.runtime.spawn(async move {
            match probed.client.probe(uuid::Uuid::new_v4().to_string()).await {
                Ok(()) => {
                    let version = "0.1.2-remote".to_string();
                    if let Ok(mut guard) = probed.version.lock() {
                        *guard = Some(version.clone());
                    }
                    tracing::info!(target: "dsh-bridge::host", endpoint = %probed.endpoint, version = %version, "connected to harness host");
                }
                Err(err) => {
                    tracing::warn!(target: "dsh-bridge::host", endpoint = %probed.endpoint, error = %err, "startup probe failed");
                    probed.router.set_probe_error(err.to_string());
                    // Broadcast an Interrupted to any already-registered sinks.
                    probed.router.broadcast(ClientEvent::Interrupted {
                        reason: format!("harness host unreachable: {err}"),
                    });
                }
            }
        });
        // Start the single remote-event read loop on the host's own runtime.
        let host_for_mux = host.clone();
        host.runtime
            .spawn(async move { host_for_mux.run_mux_loop().await });
        // …and the session control stream, which carries the live projections
        // (context occupancy, cumulative token usage) from dsh 0.1.5 on.
        let host_for_control = host.clone();
        host.runtime
            .spawn(async move { host_for_control.run_control_loop().await });
        Ok(host)
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn client(&self) -> &HttpClient {
        &self.client
    }

    pub fn router(&self) -> &Arc<SessionRouter> {
        &self.router
    }

    /// The gateway-assigned `$events` client id, if captured.
    pub fn remote_event_client_id(&self) -> Option<String> {
        self.remote_event_client_id
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    /// Whether the startup probe has failed (endpoint unreachable / not a
    /// harness host). Late-registered sinks are told via the router.
    pub fn probe_failed(&self) -> bool {
        self.router
            .probe_error
            .lock()
            .map(|guard| guard.is_some())
            .unwrap_or(false)
    }

    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// Attach a Kodex-spawned child so it is killed on last-drop.
    pub fn attach_child(&self, child: crate::process::DshChild) {
        self.spawned.store(true, Ordering::Release);
        if let Ok(mut guard) = self.child.lock() {
            *guard = Some(child);
        }
    }

    /// Mux SSE read loop. Reconnects with bounded backoff on stream end; on
    /// each reconnect, re-baselines every live session from its `last_seq` via
    /// `session.history` with bounded concurrency before resuming the live
    /// stream. A failed reopen (after retries) fails all sessions with
    /// `Interrupted`.
    async fn run_mux_loop(self: Arc<Self>) {
        loop {
            tokio::select! {
                _ = self.stop.notified() => return,
                result = self.client.open_mux() => {
                    let mut stream = match result {
                        Ok(stream) => stream,
                        Err(err) => {
                            tracing::warn!(target: "dsh-bridge::host::mux", error = %err, "mux open failed; backing off");
                            if !self.backoff_or_fail().await { return; }
                            continue;
                        }
                    };
                    // On (re)open, re-baseline all live sessions from last_seq.
                    self.capture_remote_event_client_id(&mut stream).await;
                    self.rebaseline_all().await;
                    loop {
                        tokio::select! {
                            _ = self.stop.notified() => return,
                            frame = stream.next() => {
                                let Some(req) = frame else { break; };
                                self.dispatch_mux(&req);
                            }
                        }
                    }
                    // Stream ended — backoff and reopen (unless stopped).
                    tracing::debug!(target: "dsh-bridge::host::mux", "mux stream ended; reconnecting");
                }
            }
        }
    }

    /// Session control read loop (dsh 0.1.5+).
    ///
    /// The `session/control` logical stream is host-wide: it carries every
    /// session's queue, jobs, and projection values. Projections are what keep
    /// the usage dock honest — `contextPressure` is the harness's real context
    /// occupancy (and the only figure that reacts to `/compact`) and
    /// `tokenUsage` is the durable cumulative usage. Only the
    /// `session/follow` opening baseline carries them otherwise, so without
    /// this loop the panel freezes at the value read when the session was
    /// loaded.
    ///
    /// Reconnects with the same bounded backoff as the mux loop. A failure here
    /// is NOT fatal for the sessions (usage would simply go stale), so unlike
    /// the mux loop it never broadcasts `Interrupted`.
    async fn run_control_loop(self: Arc<Self>) {
        let mut consecutive_failures: u32 = 0;
        loop {
            tokio::select! {
                _ = self.stop.notified() => return,
                result = self.client.open_session_control() => {
                    let mut stream = match result {
                        Ok(stream) => stream,
                        Err(err) => {
                            if !self
                                .control_backoff(&mut consecutive_failures, Some(&err))
                                .await
                            {
                                return;
                            }
                            continue;
                        }
                    };
                    consecutive_failures = 0;
                    loop {
                        tokio::select! {
                            _ = self.stop.notified() => return,
                            item = stream.next_item() => {
                                let Some(item) = item else { break; };
                                match item {
                                    crate::transport::FollowStreamItem::Item(value) => {
                                        self.dispatch_control(&value);
                                    }
                                    crate::transport::FollowStreamItem::Ended => break,
                                    crate::transport::FollowStreamItem::Failed(value) => {
                                        tracing::warn!(
                                            target: "dsh-bridge::host::control",
                                            frame = %value,
                                            "session control stream failed; reconnecting"
                                        );
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    tracing::debug!(target: "dsh-bridge::host::control", "session control stream ended; reconnecting");
                    if !self.control_backoff(&mut consecutive_failures, None).await {
                        return;
                    }
                }
            }
        }
    }

    /// Bounded, escalating sleep between control-stream reconnect attempts.
    /// Returns `false` when the host is stopping.
    async fn control_backoff(
        &self,
        consecutive_failures: &mut u32,
        err: Option<&anyhow::Error>,
    ) -> bool {
        *consecutive_failures = consecutive_failures.saturating_add(1);
        let delay = CONTROL_BACKOFF_BASE
            .saturating_mul(1u32 << (*consecutive_failures).min(6))
            .min(CONTROL_BACKOFF_MAX);
        if *consecutive_failures <= 2 {
            match err {
                Some(err) => tracing::warn!(
                    target: "dsh-bridge::host::control",
                    error = %err,
                    failures = *consecutive_failures,
                    "session control open failed; backing off (harnesses before dsh 0.1.5 have no control stream)"
                ),
                None => tracing::debug!(
                    target: "dsh-bridge::host::control",
                    failures = *consecutive_failures,
                    "session control stream closed; backing off"
                ),
            }
        } else {
            tracing::debug!(
                target: "dsh-bridge::host::control",
                failures = *consecutive_failures,
                delay_ms = delay.as_millis() as u64,
                "session control still unavailable; backing off"
            );
        }
        tokio::select! {
            _ = self.stop.notified() => return false,
            _ = tokio::time::sleep(delay) => {}
        }
        !self.stop_inner()
    }

    /// Route one `session/control` value to the owning sessions' sinks.
    fn dispatch_control(&self, value: &Value) {
        let frame: ControlFrame = match serde_json::from_value(value.clone()) {
            Ok(frame) => frame,
            Err(err) => {
                tracing::debug!(target: "dsh-bridge::host::control", error = %err, "dropping unparseable control frame");
                return;
            }
        };
        match frame {
            ControlFrame::Projection {
                session_id,
                key,
                value,
            } => {
                self.dispatch_mux_frame(
                    MuxFrame::SessionProjection {
                        session_id,
                        key,
                        value,
                        // The control stream's per-frame `seq` is a projection
                        // watermark, not a journal cursor; nothing in the
                        // mapping layer reads it.
                        seq: 0,
                    },
                    String::new(),
                );
            }
            ControlFrame::Baseline { value } => {
                // Background jobs at the cut (`Record<SessionId, SessionJob[]>`).
                // dsh 0.1.7 removed the key from the baseline (jobs moved to
                // the per-session `job/list` stream, `session.rs::run_job_list`),
                // so this arm only fires on older harnesses.
                if let Some(jobs) = value.get("jobs").and_then(Value::as_object) {
                    for (session_id, jobs) in jobs {
                        if let Some(jobs) = jobs.as_array() {
                            crate::jobs::record_session_jobs(session_id, jobs);
                        }
                    }
                }
                // Every session's values at the cut. Keys are session ids;
                // each value is a projection snapshot `{ asOfSeq, values }`.
                let Some(projections) = value.get("projections").and_then(Value::as_object) else {
                    return;
                };
                for (session_id, snapshot) in projections {
                    let Some(values) = snapshot.get("values") else {
                        continue;
                    };
                    let Some(sink) = self.router.get(session_id) else {
                        continue;
                    };
                    for event in crate::mapping::map_projection_values(values) {
                        sink.send(event);
                    }
                }
            }
            // A `jobs` control frame is the FULL job snapshot for one session
            // (后台任务); recorded for the context dock (no ClientEvent).
            // dsh 0.1.7 no longer sends these (jobs moved to the per-session
            // `job/list` stream); kept for older harnesses.
            ControlFrame::Jobs { session_id, jobs } => {
                crate::jobs::record_session_jobs(&session_id, &jobs);
            }
            // session/queue is not represented in `ClientEvent` (same as the
            // mux frames for it).
            ControlFrame::Queue { .. } | ControlFrame::Other => {}
        }
    }

    /// Dispatch one mux `ServerRequest`: parse the payload as a `MuxFrame`,
    /// demux by `sessionId`, map to `ClientEvent`(s), send to the matched sink.
    /// Unmatched/unknown frames are dropped with a debug log.
    async fn capture_remote_event_client_id(&self, stream: &mut SseStream) {
        if let Some(client_id) = stream.remote_event_client_id().await {
            if let Ok(mut guard) = self.remote_event_client_id.lock() {
                *guard = Some(client_id.clone());
            }
            tracing::info!(target: "dsh-bridge::host::mux", client_id = %client_id, "dsh remote events ready");
        }
        // The 0.1.5 `item` envelope is the answer-protocol probe: the same
        // release that wraps every `$events` value moved `$events/result`
        // behind the shared Connection RPC envelope, so a top-level `ready`
        // means the pre-0.1.5 bare-body carrier. A wrong guess costs one
        // retry (the answer is rebuilt for the other protocol then).
        let protocol = if stream.uses_item_envelope() {
            AnswerProtocol::Envelope
        } else {
            AnswerProtocol::Legacy
        };
        tracing::debug!(
            target: "dsh-bridge::host::mux",
            ?protocol,
            "dsh answer protocol detected"
        );
        self.client.set_answer_protocol(protocol);
    }

    fn dispatch_mux(&self, req: &ServerRequest) {
        if req.method == "remote/event" {
            let Some(event) = req.payload.get("event").and_then(Value::as_str) else {
                return;
            };
            match event {
                "api-session/status" => {
                    if let (Some(session_id), Some(running)) = (
                        req.payload
                            .get("args")
                            .and_then(|args| args.get(0))
                            .and_then(Value::as_str),
                        req.payload
                            .get("args")
                            .and_then(|args| args.get(1))
                            .and_then(Value::as_bool),
                    ) {
                        // The agent reports `running: false` on every turn end
                        // (running→idle), not only when a session dies, and a
                        // `running: true` transition carries no UI meaning at
                        // all (the host-frame mapping ignores it).
                        if running {
                            return;
                        }
                        let Some(sink) = self.router.get(&session_id.to_string()) else {
                            return;
                        };
                        // An idle transition that follows the turn's own
                        // `turn/end` must not be surfaced as an interruption
                        // (it would put the UI into its "session disconnected"
                        // state after every reply). But the two frames race:
                        // the idle transition rides the `$events` mux while
                        // the durable `turn/end` rides the per-session
                        // `session/follow` journal, and the status frame
                        // regularly wins the race — against a real dsh host it
                        // beat `turn/end` by ~1ms. So a turn that has already
                        // settled is dismissed immediately, while a
                        // still-active one defers the decision past
                        // `TURN_END_SETTLE_GRACE`: only a turn that never
                        // settles is one the host actually stopped.
                        if !sink.is_turn_active() {
                            return;
                        }
                        let deferred = sink.clone();
                        let epoch = sink.turn_epoch();
                        self.runtime().spawn(async move {
                            tokio::time::sleep(TURN_END_SETTLE_GRACE).await;
                            if deferred.is_turn_active() && deferred.turn_epoch() == epoch {
                                deferred.send(ClientEvent::Interrupted {
                                    reason: "harness session stopped".to_string(),
                                });
                            }
                        });
                    }
                }
                // dsh 0.1.5 carries agent failures on `api-session/error`
                // instead of a host frame; surface it so a turn that died is
                // reported rather than left silently spinning.
                "api-session/error" => {
                    if let (Some(session_id), Some(error)) = (
                        req.payload
                            .get("args")
                            .and_then(|args| args.get(0))
                            .and_then(Value::as_str),
                        req.payload
                            .get("args")
                            .and_then(|args| args.get(1))
                            .and_then(Value::as_str),
                    ) {
                        let frame = serde_json::json!({
                            "type": "host/agent-error",
                            "sessionId": session_id,
                            "message": error
                        });
                        if let Ok(frame) = serde_json::from_value::<HostFrame>(frame) {
                            self.dispatch_host_frame(frame);
                        }
                    }
                }
                "api-session/removed" => {
                    if let Some(session_id) = req
                        .payload
                        .get("args")
                        .and_then(|args| args.get(0))
                        .and_then(Value::as_str)
                    {
                        let frame = serde_json::json!({
                            "type": "host/session-removed",
                            "sessionId": session_id
                        });
                        if let Ok(frame) = serde_json::from_value::<HostFrame>(frame) {
                            self.dispatch_host_frame(frame);
                        }
                    }
                }
                "approval/request" => {
                    if let Some(session_id) = req.payload.get("agentId").and_then(Value::as_str) {
                        let request = req.payload.get("request").cloned().unwrap_or(Value::Null);
                        // dsh 0.1.5 forwards approvals as a scoped waterfall
                        // whose request carries no approval id at all: the
                        // approval IS the waterfall, identified by its
                        // `eventId` (our envelope rpcId), which is also what the
                        // answer must echo. Older hosts name it explicitly.
                        let approval_id = request
                            .get("approvalId")
                            .or_else(|| request.get("id"))
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| req.rpcId.clone());
                        let frame = serde_json::json!({
                            "type": "approval/requested",
                            "sessionId": session_id,
                            "approvalId": approval_id,
                            "toolName": request.get("toolName"),
                            "callId": request.get("callId"),
                            "reason": request.get("reason")
                        });
                        if let Ok(frame) = serde_json::from_value(frame) {
                            if let Some(sink) = self.router.get(&session_id.to_string()) {
                                sink.attach_waterfall_event_id(approval_id, req.rpcId.clone());
                            }
                            self.dispatch_mux_frame(frame, req.rpcId.clone());
                        }
                    }
                }
                "user-questions/request" => {
                    if let Some(session_id) = req.payload.get("agentId").and_then(Value::as_str) {
                        let request = req.payload.get("request").cloned().unwrap_or(Value::Null);
                        let frame = serde_json::json!({
                            "type": "question/requested",
                            "sessionId": session_id,
                            "questions": request.get("questions")
                        });
                        if let Ok(frame) = serde_json::from_value(frame) {
                            self.dispatch_mux_frame(frame, req.rpcId.clone());
                        }
                    }
                }
                _ => {}
            }
            if matches!(event, "session/event" | "session/subscribed") {
                if let Some(frame) = req
                    .payload
                    .get("args")
                    .and_then(Value::as_array)
                    .and_then(|args| args.first())
                    .and_then(|value| serde_json::from_value::<MuxFrame>(value.clone()).ok())
                {
                    self.dispatch_mux_frame(frame, req.rpcId.clone());
                }
            }
            return;
        }
        let frame: MuxFrame = match serde_json::from_value(req.payload.clone()) {
            Ok(f) => f,
            Err(err) => {
                tracing::debug!(target: "dsh-bridge::host::mux", error = %err, "dropping unparseable mux frame");
                return;
            }
        };
        self.dispatch_mux_frame(frame, req.rpcId.clone());
    }

    fn dispatch_host_frame(&self, frame: HostFrame) {
        if let HostFrame::HostSessionRemoved { session_id } = &frame {
            if let Some(sink) = self.router.get(session_id) {
                sink.mark_removed();
            }
        }
        let mapped = crate::mapping::map_host_frame(&frame);
        for event in mapped.events {
            if let Some(session_id) = frame.session_id() {
                if let Some(sink) = self.router.get(&session_id) {
                    sink.send(event);
                }
            }
        }
    }

    fn dispatch_mux_frame(&self, frame: MuxFrame, rpc_id: RpcId) {
        // Attach question rpcId: question/requested frames carry the batch's
        // stable rpcId on the ServerRequest envelope; record it on the sink so
        // the respond path can echo it.
        if let MuxFrame::QuestionRequested {
            session_id,
            questions,
            ..
        } = &frame
        {
            if let Some(sink) = self.router.get(session_id) {
                let ui_id = questions
                    .first()
                    .map(|q| q.id.clone())
                    .unwrap_or_else(|| rpc_id.clone());
                sink.attach_waterfall_event_id(ui_id, rpc_id);
            }
        }
        // question/resolved names the batch by rpcId, while the UI tracks it by
        // the request id (the first question id): translate before mapping so
        // ToolPermissionResolved reaches the panel that is waiting on it.
        if let MuxFrame::QuestionResolved {
            session_id,
            question_rpc_id,
            ..
        } = &frame
            && let Some(sink) = self.router.get(session_id)
            && let Some(ui_id) = sink.waterfall_ui_id_for_event_id(question_rpc_id)
        {
            sink.clear_pending_question(&ui_id);
            sink.send(ClientEvent::ToolPermissionResolved {
                id: ui_id,
                outcome: match frame {
                    MuxFrame::QuestionResolved { outcome, .. } => outcome.clone(),
                    _ => unreachable!(),
                },
            });
            return;
        }
        let Some(session_id) = frame.session_id() else {
            tracing::debug!(target: "dsh-bridge::host::mux", "mux frame without sessionId; dropping");
            return;
        };
        let Some(sink) = self.router.get(session_id) else {
            tracing::debug!(target: "dsh-bridge::host::mux", session_id, "mux frame for unregistered session; dropping");
            return;
        };
        let mapped = crate::mapping::map_mux_frame(&frame, &sink);
        for event in mapped.events {
            sink.send(event);
        }
    }

    fn dispatch_host(&self, req: &ServerRequest) {
        let frame: HostFrame = match serde_json::from_value(req.payload.clone()) {
            Ok(f) => f,
            Err(err) => {
                tracing::debug!(target: "dsh-bridge::host::host", error = %err, "dropping unparseable host frame");
                return;
            }
        };
        // Honor host/session-removed: mark the sink so re-baseline skips it,
        // and drop the session's background-job snapshot (后台任务) with it.
        if let HostFrame::HostSessionRemoved { session_id } = &frame {
            crate::jobs::clear_session_jobs(session_id);
            if let Some(sink) = self.router.get(session_id) {
                sink.mark_removed();
            }
        }
        let Some(session_id) = frame.session_id() else {
            // Host-global frame — ignored in v1 (workspace/archived/remote).
            return;
        };
        let Some(sink) = self.router.get(session_id) else {
            return;
        };
        let mapped = crate::mapping::map_host_frame(&frame);
        for event in mapped.events {
            sink.send(event);
        }
    }

    /// Re-baseline every live session from its `last_seq` via `session.history`,
    /// bounded with `buffer_unordered(REBASELINE_CONCURRENCY)`. Per-session
    /// failure isolates (that session gets `Interrupted`); others continue.
    async fn rebaseline_all(&self) {
        let sessions = self.router.live_sessions();
        if sessions.is_empty() {
            return;
        }
        let client = self.client.clone();
        let mut futures = futures::stream::iter(sessions.into_iter().map(
            |(session_id, sink)| {
                let client = client.clone();
                async move {
                    if sink.is_removed() {
                        return;
                    }
                    let from_seq = sink.last_seq.load(Ordering::Acquire);
                    let payload = crate::rpc_types::SessionHistoryPayload {
                        session_id: session_id.clone(),
                        // The re-baseline wants the tail page ending at the
                        // cursor the (re)opened follow frame just reported, so
                        // the cut is that cursor and `before_seq` bounds the
                        // page edge to the same seq. The harness caps a page at
                        // `throughSeq + 1`, so passing anything lower silently
                        // returned the wrong window.
                        through_seq: from_seq,
                        before_seq: Some(from_seq + 1),
                        max_messages: None,
                    };
                    match client
                        .session_history(uuid::Uuid::new_v4().to_string(), &payload)
                        .await
                    {
                        Ok(value) => {
                            sink.set_replaying(true);
                            for entry in value.events {
                                let event_json = entry.event;
                                let view_json = entry.view;
                                if let Ok(event) =
                                    serde_json::from_value::<crate::frame::SessionEvent>(event_json)
                                {
                                    let view = view_json
                                        .and_then(|v| serde_json::from_value::<crate::frame::ToolEventView>(v).ok());
                                    let events = crate::mapping::map_session_event(
                                        &event, view.as_ref(), &sink,
                                    );
                                    for ev in events {
                                        sink.send(ev);
                                    }
                                    sink.last_seq.store(event.seq, Ordering::Release);
                                }
                            }
                            sink.set_replaying(false);
                        }
                        Err(err) => {
                            tracing::warn!(target: "dsh-bridge::host::rebaseline", session_id = %session_id, error = %err, "history fetch failed; isolating session");
                            sink.send(ClientEvent::Interrupted {
                                reason: format!("history replay failed: {err}"),
                            });
                        }
                    }
                }
            },
        ))
        .buffer_unordered(REBASELINE_CONCURRENCY);
        while let Some(_) = futures.next().await {}
    }

    /// Bounded backoff before reopening a stream. Returns `false` if the host
    /// is stopping (caller should exit the loop). After a few failed retries,
    /// broadcasts `Interrupted` to all sessions but keeps trying.
    async fn backoff_or_fail(&self) -> bool {
        if self.stop_inner() {
            return false;
        }
        tokio::select! {
            _ = self.stop.notified() => return false,
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
        }
        true
    }

    fn stop_inner(&self) -> bool {
        // The host is stopping if the router is empty AND teardown began. We
        // rely on the `stop` Notify for explicit cancellation; this is a
        // best-effort check to avoid spinning when no sessions remain.
        self.torn_down.load(Ordering::Acquire)
    }

    /// Tear down the host: stop SSE loops, dispose runtime, kill spawned child.
    /// Called when the last session drops its refcount. Abrupt Kodex death
    /// that never reaches this (force-quit, SIGKILL, panic) is backstopped by
    /// the per-spawn exit watchdog and, as the last resort, the next-launch
    /// orphan reap — see `process.rs`.
    pub fn teardown(&self) {
        if self.torn_down.swap(true, Ordering::AcqRel) {
            return;
        }
        self.stop.notify_waiters();
        // Kill a Kodex-spawned process (SIGTERM grace then SIGKILL on Unix);
        // never kill an external endpoint.
        if self.spawned.load(Ordering::Acquire) {
            tracing::info!(
                target: "dsh-bridge::host",
                endpoint = %self.endpoint,
                "teardown: killing spawned dsh web child"
            );
            let direct_reaped = if let Ok(mut guard) = self.child.lock() {
                if let Some(child) = guard.take() {
                    crate::process::kill_child_reaped(child).unwrap_or(false)
                } else {
                    tracing::warn!(
                        target: "dsh-bridge::host",
                        endpoint = %self.endpoint,
                        "teardown: spawned host had no child handle to kill"
                    );
                    false
                }
            } else {
                false
            };
            // Outcome audit: without this the intent line above is the last
            // word, and a child that survived SIGTERM+SIGKILL (or a missing
            // handle) leaves no trace that the quit path failed.
            if direct_reaped {
                tracing::info!(
                    target: "dsh-bridge::host",
                    endpoint = %self.endpoint,
                    "teardown: spawned dsh web child terminated"
                );
            } else if cfg!(windows) {
                tracing::warn!(
                    target: "dsh-bridge::host",
                    endpoint = %self.endpoint,
                    "teardown: spawned dsh web child not reaped; trying port-kill fallback"
                );
            } else {
                tracing::warn!(
                    target: "dsh-bridge::host",
                    endpoint = %self.endpoint,
                    "teardown: spawned dsh web child not reaped; no port-kill fallback on this platform"
                );
            }
            // Fallback for the shim case: when the `dsh` launcher (`cmd.exe`
            // /volta) has already exited, the long-lived `node` is orphaned and
            // `kill_child` (which only reaps the direct child) is a no-op. If
            // the kill-on-close job also failed to attach, that node survives.
            // Reap whatever still owns the loopback port only when the direct
            // child was not reaped; keep the common exit path free of the
            // slower `netstat`/`taskkill` fallback.
            if !direct_reaped && let Some(port) = parse_loopback_port(&self.endpoint) {
                crate::process::kill_port_owner(port);
            }
        }
    }
}

/// Extract the TCP port from a `http://127.0.0.1:<port>` harness endpoint, if
/// it is a well-formed URL. Used to locate the orphaned `node` behind an
/// exited shim for the teardown port-kill fallback.
fn parse_loopback_port(endpoint: &str) -> Option<u16> {
    reqwest::Url::parse(endpoint.trim_end_matches('/'))
        .ok()
        .and_then(|url| url.port())
}

impl Drop for HarnessHost {
    fn drop(&mut self) {
        self.teardown();
    }
}

/// Registry of shared `HarnessHost`s keyed by endpoint URL. A second session
/// targeting the same endpoint reuses the existing host instead of spawning a
/// new process. Held by `app-core` so the registry outlives any single session.
pub struct HarnessHostRegistry {
    hosts: Mutex<Vec<(String, Arc<HarnessHost>)>>,
}

#[derive(Clone)]
pub struct HarnessHostRegistryHandle {
    inner: Arc<HarnessHostRegistry>,
}

impl HarnessHostRegistry {
    pub fn new() -> Self {
        Self {
            hosts: Mutex::new(Vec::new()),
        }
    }

    /// Acquire (or create) a `HarnessHost` for `endpoint`. If `spawn` is true
    /// and no endpoint is given, the caller is expected to have spawned the
    /// process and pass its discovered endpoint; this method connects to it.
    pub fn acquire(&self, endpoint: String) -> anyhow::Result<Arc<HarnessHost>> {
        if let Ok(guard) = self.hosts.lock() {
            for (url, host) in guard.iter() {
                if url == &endpoint {
                    return Ok(host.clone());
                }
            }
        }
        let host = HarnessHost::connect(endpoint.clone())?;
        if let Ok(mut guard) = self.hosts.lock() {
            guard.push((endpoint, host.clone()));
        }
        Ok(host)
    }

    /// Release a refcount on the host for `endpoint`. The last release tears
    /// down the host (closes SSE, disposes runtime, kills spawned process).
    pub fn release(&self, endpoint: &str) {
        let mut to_drop = None;
        if let Ok(mut guard) = self.hosts.lock() {
            guard.retain(|(url, host)| {
                if url == endpoint {
                    // Arc strong count: 1 in the registry + however many
                    // sessions. When only the registry entry remains, drop it.
                    if Arc::strong_count(host) <= 1 {
                        to_drop = Some(host.clone());
                        return false;
                    }
                }
                true
            });
        }
        if let Some(host) = to_drop {
            // Drop outside the lock to avoid deadlock with the host's own
            // teardown (which may join runtime tasks).
            host.teardown();
        }
    }

    /// Whether a host for `endpoint` is registered and still alive (i.e. the
    /// registry entry has not been torn down by a last-session release).
    pub fn host_alive(&self, endpoint: &str) -> bool {
        self.hosts
            .lock()
            .map(|guard| guard.iter().any(|(url, _)| url == endpoint))
            .unwrap_or(false)
    }

    /// Tear down every registered host (close SSE loops, dispose runtimes, kill
    /// any spawned `dsh web` tree) and clear the registry. Intended for the
    /// app's `before-quit` hook so the dsh process is reaped deterministically
    /// instead of relying on `Drop` — a `process::exit` / Electron `app.exit`
    /// skips destructors, so a registry held in a long-lived `Arc` would never
    /// drop and `kill_child` would never run.
    pub fn shutdown_all(&self) {
        let hosts: Vec<Arc<HarnessHost>> = match self.hosts.lock() {
            Ok(guard) => guard.iter().map(|(_, h)| h.clone()).collect(),
            Err(_) => Vec::new(),
        };
        for host in hosts {
            host.teardown();
        }
        if let Ok(mut guard) = self.hosts.lock() {
            guard.clear();
        }
    }
}

impl Default for HarnessHostRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl HarnessHostRegistryHandle {
    pub fn new(registry: Arc<HarnessHostRegistry>) -> Self {
        Self { inner: registry }
    }

    pub fn acquire(&self, endpoint: String) -> anyhow::Result<Arc<HarnessHost>> {
        self.inner.acquire(endpoint)
    }

    pub fn release(&self, endpoint: &str) {
        self.inner.release(endpoint);
    }

    /// Tear down every host and clear the registry. Wire to the app's
    /// `before-quit` hook so spawned `dsh web` processes are reaped on a normal
    /// exit (which otherwise skips `Drop`).
    pub fn shutdown_all(&self) {
        self.inner.shutdown_all();
    }
}

// silence unused-import warnings for types referenced only in docs/comments
#[allow(dead_code)]
fn _unused(_: Value, _: RpcId) {}
