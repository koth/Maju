# ACP Editor Architecture

## Goals

- Keep ACP protocol integration, application state, desktop transport, and frontend rendering in separate modules.
- Make the conversation view, tool-call timeline, repository review, and Monaco editor surface evolvable without rewriting the whole app.
- Keep Rust responsible for ACP, Git, persistence, and workflow orchestration while keeping frontend code focused on interaction and rendering.

## Stack Direction

- Backend and services: Rust
- Desktop shell and native bridge: Tauri v2
- Frontend workbench: React + TypeScript inside Tauri WebView
- Code editor and diff surface: Monaco Editor (`@monaco-editor/react`)

This architecture is implemented. The previous egui prototype has been replaced.

## Layering

### `crates/workspace-model`

Shared domain and presentation-facing data only.

- Session, message, tool, repository, diff, layout, and editor DTOs.
- Change-set DTOs for scoped file diffs: agent turn, agent conversation, manual edit, Git worktree, and tool preview sources.
- No ACP SDK code.
- No `git2` operations.
- No frontend framework or Monaco types.

### `crates/acp-core`

ACP transport and protocol mapping layer.

- `events.rs`: stable internal event contract emitted by ACP runtime.
- `client.rs`: session orchestration and worker boundary.
- `runtime.rs`: real ACP connection using the ACP client stack.
- `mapping.rs`: converts ACP updates into internal messages, tool events, and patch proposals.

This crate should know ACP well and UI not at all.

### `crates/app-core`

Application orchestration and state transitions.

- Owns canonical application state.
- Applies ACP events into internal state.
- Coordinates persistence, file open/save, patch apply/reject, repository refresh, and change-set lifecycle.
- Persists historical agent and manual edit diffs as scoped change sets with stored base/target snapshots.
- Exposes command-style entry points consumed by Tauri command handlers.

This is where operations such as `send_prompt`, `refresh_repository`, `open_editor_file`, `save_editor_file`, `apply_patch`, `stage_file`, `unstage_file`, and `commit` should live.

### `crates/git-service`

Repository inspection and mutation layer.

- Worktree status.
- Diff summaries.
- Stage, unstage, discard, and commit operations.

This crate should not know about frontend interaction patterns.

### `apps/desktop/src-tauri`

Desktop host and command/event bridge.

- Tauri bootstrap.
- Window lifecycle.
- Native menu and dialog integration.
- Command registration.
- Event emission from backend to frontend.

Recommended layout:

- `commands/session.rs`
- `commands/editor.rs`
- `commands/review.rs`
- `commands/git.rs`
- `events.rs`
- `state.rs`

This layer should be thin and delegate to app-core.

### `apps/desktop/ui`

Workbench presentation layer.

- Conversation timeline and composer.
- Tool call timeline and detail views.
- Review file tree and diff navigation.
- Monaco editor and Monaco diff editor integration.
- Desktop shell layout, app rail/sidebar state, thread header, selection state, and interaction flow.

This layer must not parse ACP schema types or implement Git logic directly.

## UI Responsibilities

### Conversation Surface

The center workspace should support a real session surface rather than a simple message list.

Recommended structure:

- `features/conversation/timeline`
- `features/conversation/composer`
- `features/conversation/context-bar`

Responsibilities:

- Render user and assistant messages.
- Render tool call summary rows inline with the session.
- Provide prompt input, mode switches, and session-level controls.

### Tool Call Interaction

Tool calls are first-class entities.

Recommended structure:

- `features/tooling/timeline`
- `features/tooling/detail`
- `features/tooling/permissions`

Display rules:

- A tool call appears as a compact item in the conversation timeline.
- Selecting it opens detail in the right panel or bottom drawer.
- Diff-producing tool calls should deep-link into review and editor surfaces.

### Diff And Git Review

The right panel is a navigator and inspector, not the only diff renderer.

Recommended structure:

- `features/review/file-tree`
- `features/review/diff-outline`
- `features/review/actions`

The file list remains on the right. The selected diff should render in the center through Monaco diff mode where space is available.

Diff identity is explicit. Review and timeline surfaces must open diffs by `change_set_id + path`, not by path alone. Persisted sources such as agent turns, overall agent conversation, and manual edits use the stored historical base/target text. Live Git sources use current repository tree pairs only: staged `HEAD -> index`, unstaged `index -> worktree`, and untracked `empty -> worktree`.

## Editing Design

The coding surface should be built around Monaco, with Rust controlling authoritative document and patch workflows.

### Backend Editor Responsibilities

Rust-side services should own:

- Open file resolution.
- File content loading and persistence.
- Dirty-state reconciliation checkpoints.
- Patch proposal lifecycle.
- Diff metadata and revalidation.
- Save conflict detection.

### Frontend Editor Responsibilities

Frontend Monaco integration should own:

- Editor instances.
- Text models.
- View state restore.
- Decorations.
- Diff editor setup.
- Split editor presentation.

Recommended frontend modules:

- `features/editor/editor-store`
- `features/editor/monaco-model-registry`
- `features/editor/monaco-decorations`
- `features/editor/monaco-diff`
- `features/editor/monaco-view-state`

### Editing Workflow

Suggested behavior:

1. Selecting a changed file in the right panel opens it in a center editor tab.
2. If ACP provided a patch, Monaco diff editor shows original versus proposed content.
3. User can switch between `Diff Preview` and `Editable File`.
4. Applying or rejecting a patch goes through Tauri commands into app-core.
5. Saving persists through Rust, then refreshes Git state and review counts.

That separation is required so conversation state, patch state, and file persistence do not drift apart.

## Data Flow

### ACP To Frontend

- ACP runtime -> `acp-core`
- `acp-core` -> app-core internal event
- app-core -> Tauri event payload
- frontend store -> conversation, tooling, review, and editor UI

### Frontend To Backend

- UI action -> Tauri invoke
- Tauri command -> app-core operation
- app-core -> ACP client or git-service or filesystem
- result -> DTO response and optional event broadcast

### Patch Review Flow

- ACP tool result -> normalized patch proposal in app-core
- frontend selects proposal -> request diff payload
- Monaco diff editor renders preview
- user accepts, rejects, or opens editable file
- Rust applies changes and refreshes repository state

### Change-Set Flow

- Agent file writes create an `AgentTurn` change set on the first verified modification in a turn.
- Completing a turn binds that change set to the assistant message so historical timeline changes can reopen later.
- `AgentConversation` is an aggregate derived from persisted agent turn change sets, not from current workspace files.
- Editor saves create `ManualEdit` change sets scoped to the active session and workspace.
- Git review remains a live, non-persisted source and never mutates session change history.

## Frontend Feature Boundaries

The next pass should move to feature folders instead of growing broad files.

- `apps/desktop/ui/src/features/conversation/*`
- `apps/desktop/ui/src/features/tooling/*`
- `apps/desktop/ui/src/features/review/*`
- `apps/desktop/ui/src/features/editor/*`
- `apps/desktop/ui/src/features/workbench/*`
- `apps/desktop/ui/src/features/session/*`
- `crates/app-core/src/commands/*`
- `crates/app-core/src/state/*`

This gives us clear ownership for:

- Prompt sending and session control.
- Tool detail selection and display.
- Editor tab state and Monaco integration.
- Workbench shell composition: app rail, contextual thread sidebar shell, compact global chrome, and thread header.
- Thread navigation presentation and client-side grouping.
- Review actions and Git flows.
- Backend command handlers with testable boundaries.

## Design Constraints

- Keep ACP schema types at the edge.
- Keep Tauri handlers thin.
- Keep Monaco-specific code out of generic state modules.
- Keep Git operations outside frontend code.
- Keep canonical session, review, and patch state in Rust services.

## Remote-Control Plane (Mobile)

A phone companion app can drive a running kodex over the public internet via a relay, without any inbound port on the PC. The PC dials the relay outbound; the phone dials the same relay; the relay routes frames between paired devices. All post-pairing traffic is end-to-end encrypted so the relay routes ciphertext only.

Three new crates form the plane:

- `crates/relay-protocol` — pure serde wire contract (`Envelope`, `ControlRequest`/`ControlResponse`, `EventFrame`, pairing/auth/binding/subscription messages, `EncryptedEnvelope`). No IO, no network, no crypto; vendored by the PC, the relay service, and the phone app.
- `crates/relay-client` — outbound-only relay client: device identity (X25519 + HMAC auth), scan-code pairing (one-time code + QR), E2E (ChaCha20-Poly1305 AEAD with HKDF-derived session key + `to_device_id` as AAD), and the connection module (WS dial, heartbeat, reconnect). Transport-only; depends on `relay-protocol` and `workspace-model`, not `app-core`.
- `app-core` `remote_control` — `RemoteControl` trait (session ops + `subscribe_updates`) taking/returning `workspace-model` DTOs only; `AppCoreRemoteControl` impl for tests, `DesktopRemoteControl` (in the Tauri shell) for the relay path.
- `apps/mobile` — the phone companion app (React Native + Expo + TypeScript). It vendors mirror types of `relay-protocol` and `workspace-model`, reimplements the byte-aligned crypto (`@noble/curves`/`@noble/hashes`/`@noble/ciphers`) and the receive-loop/driver pairing flow, and renders the same `UiSnapshot`/`UiSnapshotPatch` reducer output as the desktop frontend. See [`apps/mobile/README.md`](../apps/mobile/README.md) and the `add-mobile-companion-app` OpenSpec change.

Event delivery is unified: `Application` broadcasts lightweight `AppUpdate` signals (`UiUpdated`/`PermissionRequested`) via a `tokio::sync::broadcast` channel. Both the local frontend (`start_snapshot_bridge`, now signal-driven with a 220ms fallback) and the phone (via relay) subscribe to the same source, keep their own `UiPatchCursor`, and fetch Full/Patch deltas on each signal. There is no separate poll loop for the phone.

Remote-mode permission gating: prompts dispatched from a remote caller set `Application::remote_mode`, which suppresses full-access auto-approval of destructive permissions. The phone must send an explicit `ResolvePermission`; the tool is not executed until approval returns.

Out of scope for this repo: the relay service itself (WS routing + account/subscription DB + payment). The phone companion app now lives in this repo at `apps/mobile` (it was previously out of scope); it consumes `relay-protocol`'s contract via vendored mirror types and byte-aligned crypto. Only the standalone relay service remains external.
