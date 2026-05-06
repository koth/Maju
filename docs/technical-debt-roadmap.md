# Kodex 技术债整改 Roadmap（2~4 周）

> 目标：在不大幅扩张功能面的前提下，优先补齐状态同步一致性、Git 工作流闭环、核心模块可维护性，以及关键风险点测试与观测能力。

## 背景

当前项目已经具备较好的架构雏形：

- Rust 后端 / Tauri 壳 / React + Monaco 前端分层清晰
- `workspace-model` 作为共享 DTO 层的边界明确
- `app-core` 以 reducer 驱动 `UiSnapshot`，适合会话型 Agent 产品
- `acp-core` 将协议细节收敛在边界层，方向正确

但从继续做大、做稳、做成长期可维护产品的角度，当前更适合优先治理以下问题：

1. 状态同步模型不够统一
2. Git 工作流还没有形成完整闭环
3. 核心文件开始变重，后续演进成本上升
4. 测试和观测能力还没有贴住主要风险点

---

## 本轮整改总目标

本轮整改不追求增加大量新功能，而是优先解决四个根问题：

1. **状态同步模型不统一**
2. **Git 工作流不闭环**
3. **核心模块职责逐渐膨胀**
4. **缺少围绕高风险路径的测试与观测**

整改后希望达到：

- 前后端状态同步更稳定，减少偶发不一致
- Review / Git 面板能够支撑完整工作流
- 核心模块更容易继续拆分和演进
- 出问题时更容易定位、恢复、验证

---

## 建议节奏

建议按 3 个阶段推进：

- **第 1 周：稳状态**
- **第 2 周：补闭环**
- **第 3~4 周：提结构**

如果人力较少，可优先完成前两周内容；如果有两人及以上并行，可将第 3~4 周任务同时展开。

---

# 第 1 周：稳状态

## 目标

优先解决系统稳定性和状态一致性问题。状态层不稳时，新增功能只会持续放大复杂度。

---

## 任务 1：为 `UiSnapshot` 增加 revision/version

### 现状

前端 `Workbench.tsx` 当前通过以下方式判断状态变化：

- `sessionGetState()` 拉取全量状态
- 监听 `ui:snapshot`
- 使用 `JSON.stringify(snapshot)` 做快照比对

### 问题

- 性能会随着状态体积增大而持续变差
- 全量序列化比较不够稳定，也不利于长期维护
- 难以区分“事件未送达”与“状态未变化”
- 消息、工具、diff 增长后会带来大量不必要刷新

### 改造建议

在 `workspace-model` 的 `UiSnapshot` 中增加字段，例如：

```rust
pub revision: u64
```

由 `app-core` 在状态实际发生变更后递增，前端只处理 revision 更高的快照。

### 涉及模块

- `crates/workspace-model`
- `crates/app-core`
- `apps/desktop/ui`

### 验收标准

- 前端不再依赖 `JSON.stringify(snapshot)` 作为主要变更检测方式
- 轮询与事件到达的 snapshot 均可通过 `revision` 判断新旧
- `Workbench.tsx` 中移除大对象深比较逻辑

---

## 任务 2：明确“事件推送 + 轮询校准”的同步策略

### 目标

统一规则，不再让前端同时把 event 与 polling 当作主数据源。

### 建议策略

- **事件推送为主通道**
- **轮询仅作兜底校准**

### 具体规则

前端仅在以下场景调用 `sessionGetState()`：

- workspace 打开后的首次加载
- session 切换后
- reconnect 后
- 权限决策后
- 关键 invoke 操作完成后进行一次校准
- 定时低频校准（建议 5~10 秒，而不是 2 秒高频轮询）

### 涉及模块

- `apps/desktop/ui/src/features/workbench/Workbench.tsx`
- `apps/desktop/ui/src/lib/events.ts`
- `apps/desktop/src-tauri/src/events.rs`

### 验收标准

- 正常流转时 UI 主要依赖事件更新
- 轮询频率明显下降
- workspace 切换、reconnect、session change 场景仍保持可靠

---

## 任务 3：拆分 `Workbench.tsx`

### 现状

`Workbench.tsx` 当前承担了较多职责：

- snapshot 拉取
- 事件订阅
- tab 管理
- theme 初始化
- git refresh
- diff tab 逻辑
- permission 处理
- workspace / session 切换协作

### 目标

将其从“大总控组件”收敛为“布局 + wiring”。

### 建议拆分方式

至少拆出 3 个 hooks：

#### `useSnapshotSync()`
负责：

- 初始化 state
- 监听 `ui:snapshot`
- 低频校准
- revision 去重

#### `useEditorTabs()`
负责：

- open / close / select tab
- diff / editor tab 导航
- search result open

#### `useGitDiffResolution()`
负责：

- session diff 优先
- git diff fallback
- active diff tab 的内容解析

### 验收标准

- `Workbench.tsx` 文件体积明显下降
- 状态同步逻辑与 tab 逻辑解耦
- 后续新增功能不再继续堆积在 `Workbench`

---

## 任务 4：补关键测试——先围绕状态同步与恢复

### 本周优先补的测试

#### Rust

- session restore test
- reducer tool lifecycle test
- thinking -> message -> tool -> finish 的状态流测试

#### Frontend

- event 到达后更新 snapshot 的 hook 测试
- session 切换时 tab reset / 保留策略测试

### 验收标准

- 至少新增 6~10 个高价值测试
- 覆盖“恢复”“切换”“事件流”三类高风险场景

---

# 第 2 周：补闭环

## 目标

让 Git / Review 从“可以展示”升级成“可以完成工作”。

---

## 任务 5：实现 `git_unstage`

### 现状

Tauri command 已暴露，但 backend 仍为 TODO。

### 改造建议

在 `git-service` 中增加：

- `unstage(path, paths)`

一般可基于：

- 从 index 回退到 `HEAD`
- 或使用 git2 对 index 执行 reset / restore

### 配套修改

- `app-core` 增加相应 orchestration
- 操作完成后自动 refresh repository snapshot

### 验收标准

- UI 可真正执行 unstage 文件
- staged / unstaged 状态刷新正确
- 对非法路径、缺失路径有明确错误处理

---

## 任务 6：实现 `git_commit`

### 现状

command 已存在，但逻辑为空。

### 最小可用版

支持：

- 提交 staged files
- message 非空校验
- commit 成功后 refresh 状态
- 返回新 commit id 或明确成功结果

### 本轮先不做

- amend
- signoff
- commit template
- 更复杂的多 parent / merge 提交流程

### 验收标准

- 可从 UI 触发 commit
- commit 后 staged 区清空
- branch / head 信息刷新正确

---

## 任务 7：校正 Git diff 模型的一致性

### 背景

项目已有针对 diff 统计不准确问题的 spec，说明该问题已被识别。建议本周进一步推进“Git 数据模型可信度”。

### 建议处理方向

明确区分以下概念：

- staged diff
- unstaged diff
- untracked stats
- session change diff
- git review diff

### 为什么要做

产品里至少同时存在两类变更来源：

1. Git 仓库状态变更
2. agent / session 文件改动

如果模型不尽早区分，后面很容易在 UI 层形成“看起来是一种 diff，实际上来自两套数据源”的混淆。

### 验收标准

- `ReviewPanel` 展示的 Git 变更来源清晰
- session diff tab 与 git diff tab 的行为边界清晰
- 类型命名与数据流不再模糊

---

## 任务 8：为 Git 工作流补测试

### 必补测试

#### Rust

- stage -> unstage test
- commit test
- staged / unstaged stats correctness test
- untracked -> stage -> commit test

#### Frontend

- `ReviewPanel` 触发 stage / unstage 的交互测试
- commit 后列表状态更新测试

### 验收标准

- 核心 Git 流程具备自动验证
- 至少具备一条从 repo 初始化到 commit 的完整流程测试

---

# 第 3 周：提结构

## 目标

开始处理长期演进成本，避免核心模块继续膨胀。

---

## 任务 9：拆分 `reducer.rs`

### 现状

`crates/app-core/src/reducer.rs` 已明显变大，继续增长的维护风险较高。

### 目标

按领域拆分 reducer，而不是仅按文件长度拆分。

### 推荐结构

```text
reducer/
  mod.rs
  session.rs
  messages.rs
  tools.rs
  timeline.rs
  plan.rs
  diff.rs
```

主入口仍保留：

```rust
apply_event(ui, event)
```

内部转发到各子模块处理。

### 收益

- 可读性提升
- review 成本下降
- 测试更容易按领域组织
- 新贡献者更容易理解事件归属

### 验收标准

- `reducer.rs` 主文件仅保留 dispatch 逻辑
- 消息、工具、timeline 等逻辑独立模块化
- 测试按领域归位

---

## 任务 10：收敛 `Application` 的职责

### 现状

`Application` 已是核心协调器，但现在也承载较多恢复、初始化、diff、session 持久化协作逻辑。

### 目标

继续保留 `Application` 作为 orchestration facade，但将部分实现细节下沉为独立子模块。

### 推荐拆分方向

- `session_restore.rs`
- `prompt_runtime.rs`
- `repository_sync.rs`
- `persistence_sync.rs`

### 注意事项

目标不是把 `Application` 过度打碎，而是减少它直接包含的细节密度。

### 验收标准

- `application.rs` 文件体积下降
- bootstrap / restore / prompt progress / persistence 各自职责更清晰

---

## 任务 11：给 `session-store` 增加最小 event log

### 目标

不是全面 event sourcing，而是增加“可审计、可回放”的基础能力。

### 最小方案

新增表：

```sql
session_events (
  id,
  session_id,
  seq,
  event_type,
  payload_json,
  created_at
)
```

### 初期记录事件建议

- message appended
- tool started / updated / completed / failed
- turn finished
- permission requested / resolved
- file change tracked

### 收益

- 后续可以做回放 / 调试
- 能追查恢复异常
- 模型演进和迁移更稳

### 验收标准

- 新 session 流程会写 event log
- 能按 session + seq 查询
- 不影响现有 snapshot 恢复逻辑

---

# 第 4 周：补观测与边界

## 目标

提升系统可维护性、可诊断性，而不是继续扩展表层功能。

---

## 任务 12：引入结构化日志与 tracing

### 现状

当前主要具备 panic log，尚不足以支撑复杂状态问题定位。

### 改造建议

增加以下日志：

- app startup / shutdown
- workspace open / close / switch
- session resume / reconnect
- prompt turn lifecycle
- tool lifecycle
- git refresh / stage / unstage / commit
- DB migration / open / import

### 建议携带字段

- workspace root
- session id
- acp session id
- tool call id
- turn id
- elapsed ms

### 验收标准

当出现以下问题时，可通过日志定位：

- 状态不一致
- 会话恢复异常
- 工具卡住
- workspace 切换变慢

---

## 任务 13：规范 DB migration

### 现状

当前通过运行时检查列是否存在再 `ALTER TABLE`，适合 MVP，但版本演进后会越来越难维护。

### 改造建议

引入最小规范：

- schema version 表
- migration 编号
- 每个 migration 独立函数
- migration 测试

### 注意事项

本轮重点是建立规范，不要求一次性重写所有历史逻辑。

### 验收标准

- 新增 migration 具备明确版本号
- 旧库升级路径可以自动化测试

---

## 任务 14：梳理 `workspace-model` 的边界

### 目标

防止共享模型层持续“前端化”。

### 建议

至少在文档和类型组织上区分：

- domain / shared DTO
- UI projection model

### 可执行动作

- 新增 `docs/model-boundaries.md`
- 在 `workspace-model` 中按模块分层
- 标记哪些结构是“跨端事实”，哪些是“UI 投影”

### 验收标准

- 新增字段时能明确知道应落在哪一层
- 减少将 UI 语义直接塞入共享模型层的倾向

---

# 并行安排建议

## 单人开发：建议 2 周版

### Week 1

- snapshot revision
- 同步策略统一
- `Workbench.tsx` 拆分
- 补状态同步测试

### Week 2

- `git_unstage`
- `git_commit`
- Git 流程测试
- `reducer` 初步拆分

---

## 双人并行：建议按工程线拆分

### 工程线 A：状态 / 结构

- revision
- `Workbench` 拆分
- `reducer` 拆分
- session restore / reducer tests
- tracing

### 工程线 B：Git / 闭环

- `git_unstage`
- `git_commit`
- `ReviewPanel` 交互补齐
- Git tests
- diff 模型清理

---

# 优先级排序

## P0：必须本轮完成

1. `UiSnapshot.revision`
2. 统一 event / poll 同步策略
3. 拆 `Workbench.tsx`
4. 实现 `git_unstage`
5. 实现 `git_commit`
6. 补 session / reducer / Git 核心测试

## P1：强烈建议完成

7. 拆 `reducer.rs`
8. 给 `Application` 降职责
9. 引入 tracing
10. session event log

## P2：可以顺延

11. migration 规范化
12. `workspace-model` 边界文档化
13. 更进一步的 diff 模型重构

---

# 里程碑验收标准

## M1：状态稳定

达到以下条件：

- 前端不再依赖 stringify 做状态判重
- workspace / session 切换后状态明显更稳
- event + polling 职责清晰
- 有一批恢复 / 同步相关自动化测试

## M2：工作流闭环

达到以下条件：

- 用户可以完成 stage / unstage / commit
- `ReviewPanel` 不只是展示，还能完成动作
- Git 刷新与变更统计可信

## M3：结构可持续

达到以下条件：

- `Workbench.tsx` 与 `reducer.rs` 不再继续膨胀
- `Application` 职责更清晰
- 问题可以通过 tracing 和 event log 排查

---

# 本轮不建议做的事情

为避免整改范围失控，本轮建议暂时**不做**：

- 不急着全面重构前端状态管理
- 不急着做 hunk-level patch UI
- 不急着支持更多 agent backend
- 不大改整个 persistence 模型
- 不同时推进过多新增功能

本轮重点应聚焦于：

> **先把底座夯实，再加功能。**

---

# 一句话总结

如果只保留一句 roadmap：

> 先在 2 周内完成“状态同步统一 + Git 闭环 + 核心测试补齐”，再用后 1~2 周完成 `reducer` / `Application` / `persistence` 的结构化收敛。
