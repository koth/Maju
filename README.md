# Kodex

ACP-powered coding editor built with Rust, Tauri v2, React, and Monaco Editor.

## Project Structure

```
apps/desktop/
  src-tauri/       Tauri desktop shell, command bridge, native menus, state management
  ui/              React + TypeScript frontend (Vite, Monaco Editor)
crates/
  acp-core/        ACP client transport, session lifecycle, event stream parsing
  app-core/        Application orchestration, reducer-based state, persistence
  git-service/     Git repository inspection and mutation via git2
  session-store/   Session storage
  workspace-model/ Shared domain DTOs and presentation models
tools/
  mock-acp-agent/  Mock ACP subprocess for integration testing
docs/              Architecture and design documentation
openspec/          Design specifications and feature proposals
```

## Prerequisites

- [Rust](https://rustup.rs/) (stable toolchain)
- [Node.js](https://nodejs.org/) (v18+) with npm
- [Tauri CLI](https://tauri.app/) — install via `cargo install tauri-cli --version "^2"`
- [CodeBuddy CLI](https://cnb.cool/codebuddy) — ensure `codebuddy` is on `PATH`

## Development

```powershell
cargo tauri dev
```

This starts the Vite dev server on `http://localhost:1420` and launches the Tauri window with hot reload.

## Build & Packaging

```powershell
npm --prefix apps/desktop/ui run desktop:build
```

This single command orchestrates the full build pipeline:

1. Runs `npm run build` in `apps/desktop/ui` (TypeScript compilation + Vite bundling)
2. Compiles all Rust workspace crates in release mode
3. Embeds the frontend assets (`ui/dist`) into the binary
4. Generates platform-specific installers under `target/release/bundle/`
5. Leaves the directly launchable binary at `target/release/kodex-desktop.exe` on Windows, or the host-platform equivalent under `target/release/`

Do not use `cargo build -p kodex-desktop --release` to produce a clickable desktop app. That plain Cargo build does not run the Tauri production pipeline and can leave the app trying to load the development `devUrl` instead of embedded assets.

If a release executable opens with a `localhost` connection error, rebuild with the Tauri packaging command above. A later plain Cargo build can overwrite `target/release/kodex-desktop.exe` with a non-packaged binary.

On Windows, the useful outputs are:

- Direct executable: `target/release/kodex-desktop.exe`
- Installer: `target/release/bundle/nsis/Kodex_0.1.0_x64-setup.exe`

| Platform | Output Formats |
|----------|---------------|
| Windows  | `.msi`, `.nsis`, `.exe` |
| macOS    | `.dmg`, `.app` |
| Linux    | `.deb`, `.rpm` |

Bundle configuration lives in `apps/desktop/src-tauri/tauri.conf.json`:

- **productName**: `Kodex`
- **identifier**: `com.kodex.editor`
- **bundle.targets**: `"all"` (generates all supported formats for the current platform)
- **icons**: `apps/desktop/src-tauri/icons/` (`.ico`, `.icns`, `.png` variants)

## Runtime Data

Packaged and development builds store Kodex-owned data under the current user's home directory:

```text
~/.kodex/
  config/
  logs/
  sessions/sessions.db
  workspaces/recent-workspaces.json
```

Workspace source files, git operations, and file edits remain scoped to the selected workspace. Kodex does not create workspace-local `.kodex` application data for new workspaces; existing `{workspace}/.kodex/sessions.db` files are imported into `~/.kodex/sessions/sessions.db` without deleting the original file.

## ACP Backend

The default ACP backend is CodeBuddy CLI in ACP mode:

```powershell
codebuddy --acp
```

On Windows, the spawned command is resolved as `codebuddy.cmd --acp` so child-process launch works correctly.

To override the backend agent command, set `ACP_AGENT_COMMAND` before launching:

```powershell
$env:ACP_AGENT_COMMAND='cargo run -p mock-acp-agent --quiet --'
cargo tauri dev
```

## Testing

```powershell
cargo test
```

Runs all Rust tests across workspace crates. The `mock-acp-agent` tool can be used for integration tests without a real ACP backend.

## UI Layout

The desktop window renders a three-pane layout:

- **Left** — File tree, session list, tool activity inspector
- **Center** — Conversation timeline, Monaco code editor, and anchored composer
- **Right** — Git changes panel with diff view, file list, and patch actions
