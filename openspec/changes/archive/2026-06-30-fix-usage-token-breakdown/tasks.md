## 1. codex-acp 源头改造（submodule 内）

- [x] 1.4 在 `codex-acp` submodule 内 commit 并 push 到 origin（github.com/koth/kodex-acp.git），记录新 SHA：`d054889c40e2d99af98bd1d0d09fb5fde16f2f78`。

## 2. acp-core 映射适配

- [x] 2.1 在 `crates/acp-core/src/mapping.rs` 的 `emit_usage_update` 中，解析 `kodex.ai/usage` 的嵌套 `turn_delta` 子对象为 `UsageTokenBreakdown`，顶层字段解析为 `UsageEventScope::SessionTotal` 的 `UsageEvent`，`turn_delta` 解析为 `UsageEventScope::TurnDelta` 的第二个 `UsageEvent`，两者共享同一 `context`（来自 ACP `used`/`size`）。
- [x] 2.2 当 `turn_delta` 子对象缺失或全字段为零/null 时，跳过 `TurnDelta` 事件，仅发送 `SessionTotal`；当 meta 整体缺失时，退化为只更新 context 占用的单事件（保持现状）。
- [x] 2.4 在 `crates/acp-core/src/mapping/tests.rs` 新增/更新用例：完整 meta（两 scope 均发）、仅顶层（仅 SessionTotal）、turn_delta 全零（跳过 TurnDelta）、无 meta（仅 context）、字段名别名兼容。

## 3. reducer 语义校准

- [x] 3.1 在 `crates/app-core/src/reducer.rs` 的 `apply_usage_update` 中，移除 `UsageEventScope::ContextSnapshot` 分支把 tokens 赋给 `current_turn` 的逻辑（codex-acp 旧实现把它当本轮数据用），改为 no-op；`SessionTotal`/`TurnDelta` 分支保持不变。
- [x] 3.2 同步修改 `update_usage_model_summary` 中对 `ContextSnapshot` 的处理，使其不再覆盖 per-model 汇总的 tokens（改为显式 no-op 分支）。
- [x] 3.4 更新/新增 `reducer.rs` 内联测试：`SessionTotal` 覆盖、`TurnDelta` 累加、`ContextSnapshot` 只动 context 不动 totals、缺 `SessionTotal` 时 `session_total` 来自 `TurnDelta` 累加或为不可用。

## 4. session-store 聚合校准

- [x] 4.4 更新 `crates/session-store/src/session_store/tests.rs`：新增混合 scope 事件序列的聚合测试（SessionTotal 覆盖 + TurnDelta 累加 + ContextSnapshot 只贡献 context peak），以及纯历史 context_snapshot 行的兼容读取测试。**额外发现**：因为代码层会从一次 token-count 同时发出 SessionTotal+TurnDelta 两个事件，必须避免 TurnDelta 在同次事件中重复累加到 session_total。已在 reducer 与 session-store 中引入 `has_session_total` 标记（`#[serde(skip)]` 内部状态）来阻止重复累加，per-model summary 同步处理。

## 5. 前端验证（无代码改动）

- [x] 5.1 确认 `apps/desktop/ui/src/types/index.ts` 的 `UsageTokenBreakdown` 已含全部字段（已具备，无需改动；后端新加的 `has_session_total` 字段是 `#[serde(skip)]` 内部状态，不暴露给前端）。
- [x] 5.2 手动或通过既有 UI 测试确认 `AgentPlanPanel.tsx` 的 `totalUsageTokens` 优先取 `total_tokens`，不会因 `reasoning_tokens` 重复计入导致虚高（已确认逻辑正确）。
- [x] 5.3 在 `apps/desktop/ui` 运行 `npm run build` 确认无类型回归（`built in 7.55s`）。

## 6. 集成与验证

- [x] 6.5 `tools/mock-acp-agent` 不发送 UsageUpdate 事件；端到端真机/真 agent 验证需要启动 Tauri 应用，超出沙箱可执行范围。已在 `acp-core mapping` 测试中以单测形式完整覆盖 codex-acp payload → `SessionTotal`+`TurnDelta` 两条 `UsageEvent` 的映射与别名解析（`usage_update_maps_full_breakdown_into_session_total_and_turn_delta`、`usage_update_maps_codex_field_aliases`），并在 reducer 测试中覆盖三种 scope 的语义。建议本地启动 Kodex 与 codex-acp 做最后目视确认。
- [x] 6.6 上下文占用条（`used`/`size`）行为：本次只调整 `tokens` 处理，`context.used_tokens` / `context.window_tokens` / `context.updated_at` 仍按原逻辑更新；reducer 既有 `usage_update_overrides_window_with_known_model_context_window` 与 `usage_update_keeps_agent_window_for_unknown_model` 用例均继续通过。

## 7. 文档与收尾

- [x] 7.2 已执行归档：change 移动到 `openspec/changes/archive/2026-06-30-fix-usage-token-breakdown/`，主仓库提交 `35aa979`。
- [x] 7.3 `usage-token-breakdown` 是新 capability（不是 modify），所以不需要对既有 `add-usage-reporting` 的 spec 做 delta 标注。add-usage-reporting 自身的归档由其维护者决定；本 change 的需求描述已替代原"Agent sends usage metadata"场景的不足实现。
