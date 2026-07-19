# Contributing to Maju

感谢你对 Maju 的兴趣。这份指南帮助你在正确的方向上贡献代码。

## 开发环境

- [Rust](https://rustup.rs/) stable toolchain（Edition 2024）
- [Node.js](https://nodejs.org/) v18+
- [Tauri CLI](https://tauri.app/)：`cargo install tauri-cli --version "^2"`
- [CodeBuddy CLI](https://cnb.cool/codebuddy)（可选，用于连接真实 ACP 后端）

## 快速开始

```powershell
# 安装前端依赖
cd apps/desktop/ui && npm install

# 启动开发模式（Vite + Tauri 热重载）
cargo tauri dev

# 运行后端测试
cargo test

# 运行前端测试
cd apps/desktop/ui && npm test
```

## 项目结构

```
apps/desktop/
  src-tauri/       Tauri v2 壳层、命令桥接、事件发射
  ui/              React + TypeScript 前端（Vite、Monaco Editor）
crates/
  workspace-model/ 纯 DTO，无依赖，serde + uuid 即可
  acp-core/        ACP 传输、事件映射、权限代理
  app-core/        应用编排、reducer、session 生命周期
  git-service/     Git 仓库检查与变更（git2）
  session-store/   SQLite 持久化（WAL 模式）
tools/mock-acp-agent/  集成测试用的 mock ACP 子进程
```

## 架构边界（必须遵守）

1. **frontend 只消费 `workspace-model` DTO**，绝不直接接触原始 ACP 类型
2. **backend 绝不依赖 frontend 类型**
3. **`workspace-model` 保持零依赖**（无 ACP、无 Git、无 IO）
4. **ACP 类型停留在边缘**：`acp-core/src/mapping.rs` 负责转换为 `ClientEvent`
5. **Git 操作必须通过 `git-service`**，frontend 不接触 `git2`
6. **Monaco 状态仅为视图层**：canonical 状态由 Rust 服务持有

违反以上边界的 PR 会被要求重写。

## 代码规范

### Rust

- 使用 `cargo fmt` 格式化
- 使用 `cargo clippy` 检查
- 优先使用 `thiserror` 定义错误类型，避免裸字符串错误
- 公共 API 必须写文档注释

### TypeScript / React

- 使用函数组件 + hooks
- Feature-oriented 模块划分（见 `ui/src/features/`）
- Monaco 相关逻辑封装在 `features/editor/` 适配层，不直接散落在组件中

## 测试

- **Rust**：逻辑必须可单元测试，尤其是 `app-core` 中的 reducer 和状态转换
- **前端**：关键 UI 组件使用 `@testing-library/react` + `vitest`
- **集成**：使用 `tools/mock-acp-agent` 做端到端测试，无需真实 ACP 后端

提交前请确保：

```powershell
cargo test
cargo clippy --workspace
cd apps/desktop/ui && npm test
```

## Commit Message

使用简洁的英文描述，格式如下：

```
<scope>: <description>

[optional body]
```

示例：

```
app-core: add session reconnect with exponential backoff
ui: fix composer auto-focus on session switch
```

常见 scope：`app-core`、`acp-core`、`git-service`、`ui`、`tauri`、`docs`

## 变更提案流程

对于非 trivial 的改动（新功能、架构调整、重大重构）：

1. 使用 `openspec-propose` 创建设计提案
2. 评审通过后按 `tasks.md` 逐步实现
3. 完成后使用 `openspec-archive-change` 归档

## 提交 PR

1. 在分支上开发：`feature/your-feature-name`
2. 确保测试通过、clippy 干净
3. 更新相关文档（如架构变更需同步 `docs/architecture.md`）
4. 提交 PR，描述清楚改动动机和范围

## 许可证

Maju 采用 MIT 许可证。提交代码即表示你同意在 MIT 许可证下授权你的贡献。

## 提问与讨论

- 技术问题：开 Issue 讨论
- 架构疑问：先读 `docs/architecture.md` 和 `docs/editor-subsystem-design.md`
- 实时交流：参考项目 README 中的联系方式

---

保持边界清晰，保持测试覆盖，保持文档同步。欢迎贡献。
