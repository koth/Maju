# Maju

> 码具：码农的工具

Maju 是一个智能体编码工作台：用 Rust/Tauri 承载本地能力，用 React + Monaco 提供编辑体验，把智能体对话、代码编辑、Git 审阅和终端放在同一个工作台里。

它适合需要“边聊边改、边看 diff 边落地”的工程场景：智能体负责生成和执行方案，Maju 负责把上下文、文件、变更和权限边界稳稳托住。

Maju 本身不含模型。它通过 **ACP** 接入 Codex ACP、Claude Agent ACP，并原生集成 **DeepSeek Harness**（dsh，经私有 host RPC 桥接，不走 ACP）；模型来源是你自己的 API Key（BYOK 模式）。配套的手机端 App 可以通过中继服务器远程查看会话、发送指令、审批权限。

## 截图

| 工作台总览                                                                          |
| ----------------------------------------------------------------------------------- |
| <img src="docs/screenshots/kodex-workbench.png" alt="Maju 工作台总览" width="720"> |

| 首次设置                                                                                   |
| ------------------------------------------------------------------------------------------ |
| <img src="docs/screenshots/kodex-first-run-settings.png" alt="Maju 首次设置" width="720"> |

## 亮点

- **一个窗口完成编码闭环**：对话、Monaco 编辑器、diff review、Git changes、集成终端同屏协作。
- **多智能体后端**：ACP 接入 Codex ACP / Claude Agent ACP；原生集成 DeepSeek Harness（`dsh-bridge` 经 dsh host RPC 桥接，支持会话恢复、预设模式与用量上报）。
- **手机端遥控**：桌面端连上中继服务器后，用手机 App 远程发指令、看进度、批权限、收完成提醒；流量端到端加密，中继只转发密文。
- **定时任务**：可编排自动化调度，到点由指定智能体在工作区里执行提示词，桌面端与手机端均可查看运行记录。
- **变更可审阅**：智能体写文件、终端命令和手动编辑都会进入变更视图，方便逐文件检查和回滚。
- **本地优先**：Rust 后端负责会话、权限、Git、SQLite 持久化和文件系统访问，前端只消费共享 DTO。

## 使用

> 📖 上手指南：[桌面端](docs/desktop-user-guide.md) · [手机端](docs/mobile-user-guide.md)

安装、首次设置、BYOK 配置、远程目录、常见工作流等操作说明都在上面的指南里，此处不再重复。

## 智能体后端

| 后端 | 接入方式 | 说明 |
| ---- | -------- | ---- |
| Codex ACP（`codex-acp`） | ACP | 打包内置，设置页可选为默认智能体 |
| Claude Agent ACP（`claude-agent-acp`） | ACP | 打包内置，设置页可选为默认智能体 |
| DeepSeek Harness（`dsh`） | 私有 host RPC（非 ACP） | `npm i -g @deepseek-ai/dsh` 后可用，由 `dsh-bridge` 桥接 |

开发时可用环境变量覆盖后端命令，例如用 mock agent 跑集成测试：

```bash
ACP_AGENT_COMMAND='cargo run -p mock-acp-agent --quiet --' \
  cargo tauri dev --manifest-path apps/desktop/src-tauri/Cargo.toml
```

PowerShell equivalent:

```powershell
$env:ACP_AGENT_COMMAND='cargo run -p mock-acp-agent --quiet --'
cargo tauri dev --manifest-path apps/desktop/src-tauri/Cargo.toml
```

## 架构概览

Maju 保持 protocol、state、services、presentation 的边界清晰：

```text
workspace-model  ← pure shared DTOs
  ↑
git-service / session-store / acp-core / dsh-bridge
  ↑
app-core         ← orchestration and reducer state
  ↑
maju-desktop    ← Tauri command bridge + React UI
  ↑
relay-client → maju-relay-server ← 手机端 App（端到端加密）
```

- `acp-core`：ACP 传输、会话生命周期、事件映射、权限代理。
- `dsh-bridge`：DeepSeek Harness 宿主 RPC 桥接；实现 `acp-core` 的 `HarnessBackend` trait，依赖单向（`dsh-bridge` → `acp-core`）。
- `relay-protocol` / `relay-client` / `server`：手机端遥控的中继通道，X25519 + ChaCha20-Poly1305 端到端加密。

## 项目结构

```text
apps/
  desktop/
    src-tauri/     Tauri v2 desktop shell, command bridge, native state wrapper
    ui/            React + TypeScript frontend (Vite, Monaco Editor)
  mobile/          React Native 手机端 App（远程遥控桌面端）
crates/
  acp-core/        ACP transport, session lifecycle, event mapping, permissions
  app-core/        Application orchestration, reducer-based state, session flow
  dsh-bridge/      DeepSeek Harness host RPC bridge (harness backend)
  git-service/     Git repository inspection and staging via git2
  relay-client/    Desktop-side relay client for the mobile companion channel
  relay-protocol/  Relay wire protocol and end-to-end encryption primitives
  session-store/   SQLite session persistence under the Maju data directory
  terminal-service/ Integrated PTY terminal service
  workspace-model/ Shared DTOs consumed by backend and frontend bindings
  codebuddy-proxy/ 已下线的 CodeBuddy 集成遗留（不再接入 UI），待清理
  codebuddy-sdk/   同上
server/            中继服务器（maju-relay-server），手机端遥控通道
tools/
  mock-acp-agent/  Mock ACP subprocess for integration testing
docs/              Architecture notes, user guides, screenshots
openspec/          Feature specifications and change proposals
```

## 前置要求

- [Rust](https://rustup.rs/) stable toolchain
- [Node.js](https://nodejs.org/) v18+ with npm
- Tauri v2 CLI, either through the workspace npm scripts or globally via:

  ```bash
  cargo install tauri-cli --version "^2"
  ```

- 可选：`npm i -g @deepseek-ai/dsh` 作为 DeepSeek Harness 后端（Codex ACP / Claude Agent ACP 随安装包内置）

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

Do **not** use `cargo build -p maju-desktop --release` to produce a clickable desktop app. A plain Cargo build does not run the Tauri production pipeline and can leave the app trying to load the development `devUrl` instead of embedded assets.

If a release executable opens with a `localhost` connection error, rebuild with the Tauri packaging command above. A later plain Cargo build can overwrite the packaged executable with a non-packaged binary.

Common outputs:

- Windows executable: `target/release/maju-desktop.exe`
- Windows NSIS installer: `target/release/bundle/nsis/Maju_0.1.0_x64-setup.exe`
- macOS app/bundle output: `target/release/bundle/macos/` and `target/release/bundle/dmg/`
- Linux package output: `target/release/bundle/deb/` and/or `target/release/bundle/rpm/`

| Platform | Output Formats          |
| -------- | ----------------------- |
| Windows  | `.msi`, `.nsis`, `.exe` |
| macOS    | `.dmg`, `.app`          |
| Linux    | `.deb`, `.rpm`          |

Bundle configuration lives in `apps/desktop/src-tauri/tauri.conf.json`:

- **productName**: `Maju`
- **identifier**: `com.kodex.editor`
- **bundle.targets**: `"all"` (generates all supported formats for the current platform)
- **icons**: `apps/desktop/src-tauri/icons/` (`.ico`, `.icns`, `.png` variants)

## 自动更新与发布

Maju uses the Tauri v2 updater and GitHub Releases. The desktop app checks:

```text
https://github.com/koth/Kodex/releases/latest/download/latest.json
```

Before publishing an updater-enabled release, replace `KODEX_UPDATER_PUBLIC_KEY_PLACEHOLDER` in `apps/desktop/src-tauri/tauri.conf.json` with a real Tauri updater public key:

```bash
cd apps/desktop/ui
npx tauri signer generate -w ~/.tauri/kodex.key
```

The command prints the public key and writes the private key to `~/.tauri/kodex.key`.

Configure these GitHub repository secrets:

- `TAURI_SIGNING_PRIVATE_KEY`: contents of `~/.tauri/kodex.key`, or a path available inside the runner.
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`: optional password if the key was generated with one.

Updater signing is separate from macOS Developer ID notarization and Windows Authenticode signing. Without platform code signing, downloaded installers can still show platform trust warnings even when updater signature verification succeeds.

Release flow:

```bash
# update apps/desktop/src-tauri/tauri.conf.json version first
git tag app-v0.1.1
git push origin app-v0.1.1
```

The `.github/workflows/release.yml` workflow builds Windows x64, macOS Intel, and macOS Apple Silicon artifacts. Release jobs merge `apps/desktop/src-tauri/tauri.release.conf.json` to enable updater artifact generation, then upload installer assets, `.sig` files, and `latest.json` to a draft GitHub Release; publish the draft after verifying the assets.

## 运行时数据

Packaged and development builds store Maju-owned data under `~/.kodex/` (overridable via `KODEX_DATA_ROOT`):

```text
~/.kodex/
  config/
  logs/
  sessions/sessions.db
  workspaces/
  attachments/
```

Workspace source files, git operations, and file edits remain scoped to the selected workspace. Maju does not create workspace-local `.kodex` application data for new workspaces. Existing `{workspace}/.kodex/sessions.db` files are imported into `~/.kodex/sessions/sessions.db` without deleting the original file.

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
