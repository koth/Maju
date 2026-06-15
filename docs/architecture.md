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
