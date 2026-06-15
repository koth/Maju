# Editor Subsystem Design

## Decision

The editor subsystem should be implemented as `Rust + Tauri v2 + Monaco`.

Tauri v2 is the desktop shell, native command bridge, and event transport. Monaco (`@monaco-editor/react`) is the code editing and diff rendering surface in the React + TypeScript frontend. Rust remains the source of truth for ACP sessions, patch workflows, repository state, and filesystem persistence. This architecture is now implemented.

This gives us a practical split:

- Tauri handles windows, menus, native file dialogs, command invocation, and backend event streaming.
- Monaco handles code editing, text selection, inline decorations, diff rendering, minimap, and language tooling UX.
- Rust handles ACP protocol integration, patch normalization, Git actions, file I/O, and app-level orchestration.

## Why This Direction

This is the fastest path to a usable coding workbench without building an editor renderer from scratch.

- Monaco already solves the hard UI problems for code editing: cursor behavior, selection, IME, folding, minimap, inline decorations, diff editor, and language-aware UX.
- Tauri keeps the product Rust-centric on the backend while giving us a clean desktop packaging and command bridge model.
- ACP integration, repository workflows, and tool execution are naturally backend responsibilities and fit Rust well.
- This architecture lets us focus engineering effort on agent workflows, patch review, and repository interaction instead of reimplementing editor basics.

## What Not To Do

- Do not let the frontend speak ACP schema types directly.
- Do not let Monaco become the source of truth for workspace state.
- Do not put Git mutation logic in frontend components.
- Do not let Tauri commands grow into ad hoc business-logic handlers with no app-core boundary.

## Target Architecture

### 1. `crates/acp-core`

Responsibilities:

- ACP transport.
- Session lifecycle.
- Mapping ACP payloads into internal events.
- Normalizing tool call, message, and patch events.

Output:

- Stable internal event stream consumed by app-core.

### 2. `crates/app-core`

Responsibilities:

- Own canonical application state.
- Coordinate session timeline, tool state, repository state, and editor session state.
- Expose command-style operations for open file, save file, apply patch, stage file, unstage file, commit, and refresh.
- Persist or rehydrate UI-relevant state when needed.

App-core should define the frontend-facing DTOs that Tauri returns.

### 3. `crates/git-service`

Responsibilities:

- Inspect repository state.
- Produce changed file lists and diff summaries.
- Execute stage, unstage, discard, and commit operations.

This crate should remain isolated from frontend concerns.

### 4. `apps/desktop/src-tauri`

Responsibilities:

- Tauri app bootstrap.
- Command registration.
- Window lifecycle.
- Event emission from backend to frontend.
- Bridging frontend requests into app-core operations.

Recommended structure:

- `commands/session.rs`
- `commands/editor.rs`
- `commands/review.rs`
- `commands/git.rs`
- `events.rs`
- `state.rs`

This layer should be thin. It is a transport adapter, not the business layer.

### 5. `apps/desktop/ui`

Responsibilities:

- Conversation timeline.
- Tool call timeline and detail drawers.
- File tree and review navigator.
- Monaco editor and Monaco diff editor integration.
- Interaction state such as selected tool call, selected file, open tabs, active center mode, split layout, and panel visibility.

Recommended structure:

- `src/features/conversation/*`
- `src/features/tooling/*`
- `src/features/review/*`
- `src/features/editor/*`
- `src/features/workbench/*`
- `src/lib/tauri.ts`
- `src/lib/events.ts`
- `src/lib/monaco/*`

## Editor Model

We still need an editor domain model even though Monaco renders the text.

### Rust-Side Canonical State

Rust should own:

- Open file identity.
- Saved file content and version.
- Dirty state checkpoints.
- ACP patch proposals and their lifecycle.
- Git diff metadata and live worktree diff scopes.
- Persisted change sets for agent turns, overall agent conversation, and manual editor saves.
- Conflict and divergence detection.

Recommended DTOs:

- `EditorTab`
- `EditorDocument`
- `EditorSelectionState`
- `PatchProposal`
- `PatchHunk`
- `DiffSummary`
- `ToolCallDetail`

### Frontend Monaco State

Frontend should own:

- Monaco editor instances.
- Monaco text models.
- View state restore data.
- Temporary cursor and scroll state.
- Decoration handles.
- Split editor layout state.

Monaco model content can be edited locally for responsiveness, but save, revert, apply-patch, and refresh actions must reconcile through Rust commands.

## Core Workflows

### File Open

1. User clicks a file in the review tree or workspace tree.
2. Frontend invokes `open_editor_file` through Tauri.
3. App-core resolves the file, loads content, and returns `EditorDocument`.
4. Frontend creates or reuses a Monaco model for that path.
5. The file opens in the center editor tab set.

### File Edit And Save

1. User edits text in Monaco.
2. Frontend tracks the dirty state locally and marks the tab dirty immediately.
3. On save, frontend sends the current content and version token to Tauri.
4. App-core validates the write, persists to disk, records or updates the session-scoped `ManualEdit` change set from the editor baseline to the saved target, updates document version, and refreshes repository metadata.
5. Frontend updates dirty markers, diff summaries, and review counts through scoped change-set APIs.

### ACP Tool Call Timeline

1. ACP runtime emits message or tool events.
2. `acp-core` maps them into internal events.
3. App-core appends them to session state.
4. Tauri emits frontend events for timeline refresh.
5. Frontend shows compact tool rows in the conversation stream.
6. Selecting a tool row opens a detail inspector with input, output, touched files, and status.

### ACP Patch Review

1. ACP emits a patch-producing tool result.
2. App-core stores a normalized `PatchProposal` per target file.
3. Frontend can request a diff view model for a file or proposal.
4. Monaco diff editor renders original vs proposed content.
5. UI offers:
   - Open diff preview
   - Apply file patch
   - Reject file patch
   - Open editable file
   - Jump to touched hunk

### Editable Patch Follow-Up

1. User opens the target file in standard Monaco editor mode.
2. Frontend overlays decorations for proposed hunks or changed ranges.
3. If the user edits the file beyond the original proposal, frontend requests revalidation.
4. App-core marks the proposal as applied, pending, rejected, or diverged.

### Git Actions

1. Stage, unstage, discard, and commit actions are invoked through Tauri commands.
2. Git service performs repository mutation.
3. App-core refreshes live Git review state without writing session change-set history.
4. Tauri emits updated review snapshots.
5. Frontend updates file groups, counts, and selected diff/editor state.

### Scoped Diff Loading

Every persisted file diff is addressed by `change_set_id + path`. Historical agent turn diffs, overall conversation diffs, and manual editor diffs load the stored base/target snapshots even if the file has since changed or been deleted. Path-only diff loading is a compatibility shim and must not be used by new review, timeline, or editor surfaces.

## UI Composition

### Left Panel

- Sessions
- Workspace context
- Branch and repository status
- Navigation entry points

### Center Workspace

The center region should switch between these modes:

- `Conversation`
- `Editor`
- `Diff`
- `SplitConversationEditor`
- `SplitDiffEditor`

Recommended default behavior:

- Conversation is primary when no file is selected.
- Opening a file or diff takes over center focus.
- Tool-detail-heavy workflows can keep conversation visible in split mode.

### Right Panel

The right side is the contextual navigator.

- `Files`: changed files, staged and unstaged groups, counts.
- `Diff Outline`: touched files, hunks, and patch summaries.
- `Tool Detail`: selected tool call metadata and outputs.

### Bottom Drawer

Reserve the bottom drawer for:

- Raw tool output.
- Terminal stream later.
- Search results.
- Diagnostics.

This avoids overloading the right panel and matches coding-tool expectations.

## Monaco Integration Design

### Editor Adapter Layer

Add a dedicated frontend adapter instead of calling Monaco APIs everywhere.

Recommended modules:

- `src/features/editor/monaco-model-registry.ts`
- `src/features/editor/monaco-decorations.ts`
- `src/features/editor/monaco-diff.ts`
- `src/features/editor/monaco-view-state.ts`
- `src/features/editor/editor-store.ts`

Responsibilities:

- Create and dispose models.
- Track URI-to-model mapping.
- Apply decorations for ACP hunks and Git changes.
- Preserve editor view state across tab switches.
- Isolate Monaco-specific logic from generic UI components.

### Diff Strategy

Use Monaco diff editor for file-level patch preview first.

Phase progression:

- Phase 1: full-file diff preview.
- Phase 2: inline changed-range decorations in standard editor mode.
- Phase 3: hunk-level apply and reject interactions.

### Large File And Sync Constraints

- Avoid re-creating Monaco models on every selection change.
- Use path-based model caching.
- Stream only required file content and diff payloads.
- Keep frontend patches coarse enough to avoid command chatter on every keystroke.
- Save explicitly, not on every edit.

## Data Flow

### ACP To UI

- ACP runtime -> `acp-core`
- `acp-core` -> app-core internal event
- app-core -> Tauri event payload
- frontend stores -> conversation/tool/review/editor UI

### UI To Backend

- frontend interaction -> Tauri invoke
- Tauri command -> app-core command handler
- app-core -> git-service or filesystem or ACP client
- result -> DTO response + optional event broadcast

### Patch Application

- ACP patch -> app-core normalized proposal
- frontend requests diff DTO
- Monaco diff/editor renders proposal
- user accepts/rejects/applies
- Tauri command -> app-core apply flow -> write/refresh -> UI refresh

## Delivery Plan

### Phase 1

- Establish `apps/desktop/src-tauri` and `apps/desktop/ui` boundaries.
- Add Tauri command and event bridge.
- Open files into Monaco editor tabs.
- Show conversation and file review in the new workbench layout.

### Phase 2

- Render ACP tool calls as first-class timeline items.
- Add tool detail inspector.
- Add Monaco diff preview for patch-producing tool calls.

### Phase 3

- Add save, refresh, and dirty-state reconciliation.
- Add Git stage, unstage, and discard actions.
- Refresh diff state after file persistence.

### Phase 4

- Add hunk-level apply and reject.
- Add bottom drawer for raw tool output and diagnostics.
- Add command palette, search, and richer workbench interactions.

## Constraints

- Keep ACP-specific payloads out of the frontend feature layer.
- Keep Monaco-specific code out of generic app-state modules.
- Keep canonical repository and patch state in Rust.
- Keep Tauri command handlers thin and delegating into app-core.

## Recommendation Summary

Use Tauri as the Rust desktop shell and integration bridge, and use Monaco as the editor and diff surface. Treat Rust as the authority for ACP, patch, file, and Git workflows. Treat Monaco as the rendering and interaction engine for code editing, not the owner of application truth.
