# AGENTS.md

## Purpose

**Kodex** — ACP-powered coding editor: Rust backend, Tauri v2 shell, Monaco surface. Strict separation: protocol ↔ state ↔ services ↔ presentation.

## Architecture

| Crate / Package | Role | Boundary |
|---|---|---|
| `crates/workspace-model` | Pure DTOs (`UiSnapshot`, `ChatMessage`, `ToolInvocation`, `TimelineItem`, `RepositorySnapshot`) | No logic; only `serde` + `uuid` |
| `crates/acp-core` | ACP transport (`SessionHandle`), event mapping (`ClientEvent`), permission broker, terminal manager | Edge — translates `SessionNotification` → `ClientEvent`; handles CodeBuddy `_meta` extensions |
| `crates/app-core` | `Application` orchestration, reducer, session lifecycle, prompt flow, persistence delegation | Core logic; depends on all crates |
| `crates/git-service` | Git inspection (`RepositorySnapshot`) & staging via `git2` | Service interface; path sanitization enforced |
| `crates/session-store` | SQLite persistence (`.kodex/sessions.db`): sessions, messages, tools, file changes, timeline | WAL mode, cascade deletes, upsert semantics |
| `apps/desktop/src-tauri` | Tauri v2 host, `AppState` wrapper, command bridge (~24 cmds), event emitters | Thin shell — delegates to `app-core` via `state.with_app()` |
| `apps/desktop/ui` | React 18 + Monaco + Vite | Feature modules; Tauri commands/events only |
| `tools/mock-acp-agent` | Integration test agent | stdio: Initialize/NewSession/Prompt |

```
workspace-model  ← pure DTOs, no deps
  ↑
git-service / session-store / acp-core
  ↑
app-core         ← orchestrates all
  ↑
kodex-desktop    ← tauri v2 shell
```

Hard rules:
- Frontend consumes only `workspace-model` DTOs, never raw ACP types.
- Backend never depends on frontend types.
- `workspace-model` stays dependency-free (no ACP, no Git, no IO).

## Design Patterns

- **Channel-based ACP** — `SessionHandle::start()` spawns tokio thread; `event_rx`/`command_tx` mpsc channels; `PromptTask` drains via `collect_ready_events()` / `try_recv`.
- **Reducer** — `apply_event(&mut UiSnapshot, ClientEvent)` in `reducer.rs`: message coalescing, tool CRUD, timeline ordering, status transitions, diff preview, file change tracking.
- **Snapshot sync** — Frontend polls `session_get_state` → `poll_prompt_progress()` → `UiSnapshot` clone; backend also pushes Tauri events (`emit_session_status`, `emit_tool_updated`, etc.) for reactivity.
- **Timeline** — `Vec<TimelineItem>` interleaves `Message(Uuid)` | `Tool(Uuid)` chronologically.
- **Tool hierarchy** — `parent_call_id` + `is_subagent` from CodeBuddy `_meta`; `finalize_running_children()` on parent transitions.
- **Permissions** — `PermissionBroker` with `Plan` mode (reads + md edits) / `Build` mode (all workspace ops); outside-workspace always prompts.
- **Session resume** — `acp_session_id` in SQLite → ACP `session/load` when supported on switch/reconnect; `load_session()` reconstructs the local timeline by `seq`.

## Frontend (`apps/desktop/ui/src/features/`)

| Module | Purpose |
|---|---|
| `workbench/` | Shell layout: `Workbench`, `AppRail`, `GlobalChrome`, `TabBar`, `ThreadHeader`, `WelcomeLauncher` |
| `conversation/` | `ConversationTimeline`, `MarkdownBody` |
| `composer/` | `Composer` — prompt input |
| `tooling/` | `ToolCallCard` — tool invocation display |
| `editor/` | `EditorView`, `DiffTab`, `DiffView`, Monaco model registry/theme/view-state, TextMate engine |
| `filetree/` | `FileTree`, file-icons |
| `review/` | `ReviewPanel` |
| `changes/` | `ChangesBar`, `ChangesPanel` |
| `session/` | `SessionList` |

Shared: `src/types/` (TS types), `src/lib/tauri.ts` (API wrappers), `src/lib/events.ts` (event listeners).

## Tauri Commands (`src-tauri/src/commands/`)

- **session.rs** — get_state, send_prompt, set_config_control, resolve_permission, cancel, list, switch, create, delete, get_changes, get_file_diff, reconnect
- **workspace.rs** — open, close, get_recent, remove_recent
- **git.rs** — status, stage, unstage (TODO), commit (TODO), refresh
- **editor.rs** — file operations
- **review.rs** — diff review
- **fs.rs** — filesystem listing

## Editing Rules

- Feature-oriented modules; avoid monolithic files.
- ACP types stay at edge (`acp-core/src/mapping.rs`) — convert to `ClientEvent` before core/UI.
- Git ops go through `git-service`; frontend never touches `git2`.
- Monaco behind adapters (`monaco-model-registry.ts`, `monaco-theme.ts`, `monaco-view-state.ts`); other panels don't touch Monaco state.
- TextMate via `textmate-engine.ts` / `textmate-registry.ts` — separate from Monaco API.

## Editor Direction

- Tauri = app shell, window host, backend bridge, native integration.
- Monaco = code editing surface, inline diff review, patch preview.
- Rust = source of truth for ACP sessions, repo state, patches, filesystem.
- Monaco models are view-layer state only; canonical state lives in Rust services.

## Verification

- `cargo test` from workspace root for backend changes.
- Frontend build/test for UI or Monaco changes. Tests live where the logic lives.
- `tools/mock-acp-agent/` for integration tests without real ACP backend.

## Design Docs

- [`docs/architecture.md`](/docs/architecture.md) — update when layering changes.
- [`docs/editor-subsystem-design.md`](/docs/editor-subsystem-design.md) — update when editor architecture changes.
- `openspec/specs/` — feature specifications and change logs.
