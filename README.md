# Kodex

ACP-powered coding editor built with Rust, Tauri v2, React, TypeScript, and Monaco Editor.

Kodex keeps a strict boundary between protocol handling, application state, services, and presentation:

```text
workspace-model  ← pure shared DTOs
  ↑
git-service / session-store / acp-core
  ↑
app-core         ← orchestration and reducer state
  ↑
kodex-desktop    ← Tauri command bridge + React UI
```

## 项目结构

```text
apps/desktop/
  src-tauri/       Tauri v2 desktop shell, command bridge, native state wrapper
  ui/              React + TypeScript frontend (Vite, Monaco Editor)
crates/
  acp-core/        ACP transport, session lifecycle, event mapping, permissions
  app-core/        Application orchestration, reducer-based state, session flow
  git-service/     Git repository inspection and staging via git2
  session-store/   SQLite session persistence under the Kodex data directory
  workspace-model/ Shared DTOs consumed by backend and frontend bindings
tools/
  mock-acp-agent/  Mock ACP subprocess for integration testing
docs/              Architecture notes and technical debt roadmap
openspec/          Feature specifications and change proposals
```

## 前置要求

- [Rust](https://rustup.rs/) stable toolchain
- [Node.js](https://nodejs.org/) v18+ with npm
- Tauri v2 CLI, either through the workspace npm scripts or globally via:

  ```bash
  cargo install tauri-cli --version "^2"
  ```

- [CodeBuddy CLI](https://cnb.cool/codebuddy) on `PATH` for the default ACP backend

## 开发

Install frontend dependencies first:

```bash
npm --prefix apps/desktop/ui install
```

Start the desktop app in development mode:

```bash
cargo tauri dev --manifest-path apps/desktop/src-tauri/Cargo.toml
```

This starts the Vite dev server on `http://localhost:1420` and launches the Tauri window with hot reload.

You can also run from the Tauri crate directory:

```bash
cd apps/desktop/src-tauri
cargo tauri dev
```

## 构建与打包

```bash
npm --prefix apps/desktop/ui run desktop:build
```

This command runs the Tauri production pipeline:

1. Runs `npm run build` in `apps/desktop/ui` (TypeScript compilation + Vite bundling)
2. Compiles the Rust workspace crates in release mode
3. Embeds the frontend assets from `apps/desktop/ui/dist`
4. Generates platform-specific installers under `target/release/bundle/`
5. Leaves the directly launchable binary under `target/release/`

Do **not** use `cargo build -p kodex-desktop --release` to produce a clickable desktop app. A plain Cargo build does not run the Tauri production pipeline and can leave the app trying to load the development `devUrl` instead of embedded assets.

If a release executable opens with a `localhost` connection error, rebuild with the Tauri packaging command above. A later plain Cargo build can overwrite the packaged executable with a non-packaged binary.

Common outputs:

- Windows executable: `target/release/kodex-desktop.exe`
- Windows NSIS installer: `target/release/bundle/nsis/Kodex_0.1.0_x64-setup.exe`
- macOS app/bundle output: `target/release/bundle/macos/` and `target/release/bundle/dmg/`
- Linux package output: `target/release/bundle/deb/` and/or `target/release/bundle/rpm/`

| Platform | Output Formats |
|----------|----------------|
| Windows  | `.msi`, `.nsis`, `.exe` |
| macOS    | `.dmg`, `.app` |
| Linux    | `.deb`, `.rpm` |

Bundle configuration lives in `apps/desktop/src-tauri/tauri.conf.json`:

- **productName**: `Kodex`
- **identifier**: `com.kodex.editor`
- **bundle.targets**: `"all"` (generates all supported formats for the current platform)
- **icons**: `apps/desktop/src-tauri/icons/` (`.ico`, `.icns`, `.png` variants)

## 运行时数据

Packaged and development builds store Kodex-owned data under the current user's home directory:

```text
~/.kodex/
  config/
  logs/
  sessions/sessions.db
  workspaces/recent-workspaces.json
```

Workspace source files, git operations, and file edits remain scoped to the selected workspace. Kodex does not create workspace-local `.kodex` application data for new workspaces. Existing `{workspace}/.kodex/sessions.db` files are imported into `~/.kodex/sessions/sessions.db` without deleting the original file.

## ACP 后端

The default ACP backend is CodeBuddy CLI in ACP mode:

```bash
codebuddy --acp
```

On Windows, the spawned command is resolved as `codebuddy.cmd --acp` so child-process launch works correctly.

To override the backend agent command, set `ACP_AGENT_COMMAND` before launching:

```bash
ACP_AGENT_COMMAND='cargo run -p mock-acp-agent --quiet --' \
  cargo tauri dev --manifest-path apps/desktop/src-tauri/Cargo.toml
```

PowerShell equivalent:

```powershell
$env:ACP_AGENT_COMMAND='cargo run -p mock-acp-agent --quiet --'
cargo tauri dev --manifest-path apps/desktop/src-tauri/Cargo.toml
```

## 测试

Run Rust tests across workspace crates:

```bash
cargo test
```

Run frontend tests:

```bash
npm --prefix apps/desktop/ui test
```

Build the frontend only:

```bash
npm --prefix apps/desktop/ui run build
```

The `tools/mock-acp-agent` tool can be used for integration tests without a real ACP backend.

## UI 布局

The desktop window renders a three-pane layout:

- **Left** — file tree, session list, and tool activity inspector
- **Center** — conversation timeline, Monaco code editor, and anchored composer
- **Right** — git changes/review panel with diff view, file list, and patch actions

## 说明

- Frontend code consumes shared `workspace-model` DTOs rather than raw ACP types.
- Backend ACP details are converted to internal client events at the `acp-core` edge.
- Git operations go through `git-service`; the UI calls Tauri commands instead of touching git directly.
