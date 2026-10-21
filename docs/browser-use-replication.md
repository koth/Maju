# ChatGPT / Codex 「browser use」解剖与复刻方案

> 本文的每一条机制都来自本机安装的 ChatGPT（Codex）桌面版，不来自公开资料或猜测。
> 采集对象：
>
> - 应用：`C:\Program Files\WindowsApps\OpenAI.Codex_26.1016.6573.0_x64__2p2nqsd0c76g0\`
> - 插件缓存：`C:\Users\yvonchen\.codex\plugins\cache\openai-bundled\browser\26.1013.40934\`
> - 关键文件：`scripts/browser-client.mjs`（169,663 B，单行压缩）、
>   `scripts/browser-service.mjs`（1,464,691 B）、`scripts/browser-accessibility.wasm.br`
>   （4,084,148 B 压缩 → 20,208,690 B wasm）、`docs/api.json`（66,565 B）、`docs/*.md`（22 篇）、
>   `docs/documents.json`（注入清单）
>
> 「证据」标注为 `文件:位置` 或字符串原文，便于复核。

---

## 0. 一句话结论

它的 browser use 不是「一堆 JSON 工具」，而是**一个常驻 JS 运行时 + 一棵可 diff 的无障碍树 + 一套按需注入的行为文档**：

- 模型拿到的是 `js` 工具，在里面写 JS 调 `tab.ax.click(42)` / `tab.ax.write()`；
- 观测走**无障碍树渲染成的文本**，带稳定 `element_index` 与 diff；
- 动作之后由 **`waitForAXQuiet`** 而不是固定 sleep 决定「什么时候可以读状态」；
- 能力面（哪些方法存在、哪些文档注入）由**服务端在 setup 时下发**，客户端用 Proxy 物理裁剪；
- 安全靠**策略文档（prompt 层）+ 动作级审批闸门（实现层）**双层，而不是靠工具白名单。

我们要复刻的不是它的代码，是这五条**行为**。其中前三条贡献了 90% 的「好用感」，后两条贡献了 90% 的「敢用感」。

---

## 1. 组件与调用链

```
模型
 │  一次 MCP 工具调用：js(code)
 ▼
node_repl MCP server（宿主提供，非本插件）
 │  globalThis.nodeRepl = { rpc, env, emitImage, nativePipe }
 ▼
browser-client.mjs   ← 只有 1 个导出：setupBrowserRuntime()
 │  返回 Agent → agent.browsers.getDefault() → Browser → browser.tabs → Tab
 │  Tab 上并列 5 个子 API：ax / playwright / dom_cua / cua / content
 │  每个成员被 Proxy 按 disabledMemberIds 裁剪
 ▼  命令对象（{type:"tab_ax_action", ...}）经 rpc 发往
browser-service.mjs  ← 命令执行器：setup / execute / executeWithRecovery
 │  持有 CDP 会话、tab 注册表、AX 缓存、审批闸门、网络拦截
 ▼
Accessibility.getFullAXTree + DOM.* + Input.* + Page.* + Runtime.*（113 个 CDP 方法）
 │
 ▼  把 wire 节点喂给
browser-accessibility.wasm（WASI，Swift 编译）
 │  导出 computer_use_browser_wasm_revision_{create,text_length,copy_text,
 │       identity_length,copy_identity,is_value_settable} 等 9 个函数
 ▼
文本状态 + 截图 → 经 displayBridge 直接进入工具结果
```

三个反直觉的事实：

1. **插件不注册任何 MCP 工具**。`plugin.json` 里没有 `mcpServers`，只有三个 hook（`Stop` / `Interrupt` / `SubagentStop`）都指向 `node_repl.turn_ended`。它寄生在宿主的 `js` 工具里。
2. **API 面是运行时分发的**。`setup` 返回 `{ apiManifest, disabledMemberIds }`，`apiManifest` 就是 `docs/api.json` 的同一份数据；客户端用 Proxy 让被禁成员「不存在」而不是「抛错」。所以同一份文档在 IAB / Chrome 扩展 / 纯 CDP / MCP Apps 上暴露的方法集不同。
3. **AX 渲染器是独立 wasm 模块**，与执行器解耦。可以先怀疑它是 Rust，实际是 **Swift 编译到 wasm**（wasm 字符串里带着 `CFBitVector`/`CFString` 的调试格式与 `Prespecializations library` 的运行时提示）——同一份树渲染代码在 Mac 原生 App 与 Web 侧共用。

---

## 2. 五个真正起作用的机制

### 2.1 code-mode：一次调用 = 一串动作 + 一次观测

`docs/accessibility.md` 的原文要求：

> Batch as many actions as possible and the resulting `tab.ax.write()` into one Node REPL `js` call.

配合 `docs/api-use-behavior.md:3`——REPL 状态跨调用保留，用 `const` 持有句柄、禁止用 `globalThis` 或重复获取。

效果：一个 5 步的表单填写是 **1 次**模型调用、**1 次**观测，而不是 10 次工具往返。这是它与「JSON 工具 + 每步一快照」最本质的差别，也是 token 与延迟差异的主要来源。

配套纪律（同样写进文档，影响行为而不影响代码）：

- 每次动作后用**最便宜的那一次**状态确认（`api-use-behavior.md:6`）：要 locator 真值取 DOM snapshot，要视觉确认取截图，默认不要两个都要。
- 动作无效时不要盲重试、也不要立刻降级到坐标点击，先找遮挡或状态变化（:7）。
- 已在目标 URL 的 tab 不要 `goto` 同一 URL（会 reload 丢用户输入），要刷新用 `tab.reload()`。

### 2.2 AX 文本：wire DOM → 文本树 → 稳定序号 → diff

**输入模型**（从 wasm 的字符串常量复原，即 `ChromeAccessibilityWireNode` / `ChromeAccessibilityWireDOM`）：

```
envelope: { revision, elementCount, tabID, urlShortener, nodes[], bounds?, rawValue? }
node:     { nodeID, parentIndex, childIDs[], chromeRole, name/description/value,
            nameSource, subrole, roleDescription, properties[], ignored,
            backendDOMNodeID, targetID, relatedNodes[],
            isSelected/isSelectable/isFocused/isFocusable/isEnabled/isValueSettable,
            isDisclosable/isDisclosing, actionDescriptions[], placeholderValue,
            valueDescription/valueTypeName, transformedString, domClassList,
            PresentationActions }
dom:      { tagName, attributes, bounds, clickable, identifier, className,
            declaredRole, hasAriaDescription }
ARIA 态:  atomic autocomplete checked controls describedby details disabled editable
          errormessage expanded flowto focusable focused hasPopup invalid keyshortcuts
          labelledby level modal multiline multiselectable orientation placeholder
          pressed radiogroup relevant required roledescription selected settable
          valuemax valuemin valuetext
```

即：把 `Accessibility.getFullAXTree` 的节点、`DOM.getDocument` 的元数据、ARIA 属性三者拼成一个完整的 wire DOM，交给渲染器。渲染器再产出**文本**，并通过 `revision_identity` 给出一份身份串（用于判断「还是不是同一棵树」）。

**diff 语义**（字符串原文，`browser-accessibility.wasm` 常量区）：

> `The following is a diff from the previous accessibility tree with ~ and + representing changed and added elements, respectively. Removed elements are summarized by ID range.`

> `There has been no change in the accessibility tree.`

> `Removed element IDs: ` / `AccessibilityDifferenceLineBudgetExceeded`

所以 diff 的输出形态是：固定 header 一行、`~` 标记内容变化的行、`+` 标记新增的行、**删除按 ID 区间汇总成一行**（不逐行列出被删元素）；没有任何变化时，整段输出被替换成那句固定的 "There has been no change…"。

**精确行格式**（把 wasm 当黑盒跑出来的，不是推测）：

```
0 AXWebArea Example Domain, URL: example.com/path?q=1
\t1 heading Example Domain
\t2 link More information, Description: More information
\t3 text field Description: Email, Value: a@b.c
```

- 行 = `("\t" × depth) + index + " " + role` + 可选 `" " + name` + 可选 `", Help: <description>"` + 可选 `", Value: <value>"`；根行额外 `", URL: <去掉 scheme 与 www. 的 url>"`。
- **缩进是 tab，且缩进在数字之前**；层级靠缩进表达，是 DFS 先序的线性序列。
- **`nameSource: "attribute"` 的 name 与所有 `link` 的 name 会被降级成 `Description: <name>`**（空格分隔，不是逗号）；其余字段用 `", "` 分隔。
- role 走一张映射表：`RootWebArea → AXWebArea`、`textbox → text field`、`StaticText → text`、`searchbox → search text field`、`img → image`。
- 只渲染 `checked`（且只在 checkbox 上变成 `Value: 1`）与 `selected`（变成 `(selectable, settable)`）；**`disabled`/`focused`/`expanded`/`required`/`level` 一律不渲染**。
- name 不转义、不加引号：页面文本里的制表符会直接破坏行结构（它的实现确实如此，我们照抄了「不转义」，因为转义比歧义更贵）。


**diff 是否启用有阈值**：`TINYSKY_AX_TREE_DIFF_MIN_SAVED_RATIO`、`TINYSKY_AX_TREE_DIFF_MIN_SAVED_BYTES`——只有 diff 相对全量省下足够比例/字节才用 diff，否则退回全量。这是「省 token」与「别让模型猜」之间的工程平衡点，值得照抄。

**为什么要有 `element_index`**：模型用它指代元素（`tab.ax.click(42)`）。它**不是稳定 ID**，而是「被渲染出来的行的下标」：从 0 开始、每轮 capture 重排、被丢弃的节点不占号、合并成一行文本的兄弟节点只有一个号（指向第一个的代表元素）。文档因此强制每次动作后重新 `ax.write()`——把「元素失效」从异常变成了协议。

配套的内部身份（wasm 的 `revision_identity(index)`）是 `"{len}:{tabId}|{len}:{targetID}|{len}:{backendDOMNodeID}"`，JS 侧用它反查 `backendNodeId`，diff 也用它判断「哪一行还是原来那一行」。**行号负责指代，身份负责对齐**——这两件事分开，是整个观测层最关键的一个决定。


**渲染器可被独立驱动**（已跑通）：wasm 只导入 34 个 `wasi_snapshot_preview1` 符号，导出 `memory`、`_initialize` 与 9 个 `computer_use_browser_wasm_*` 函数。ABI 如下，逐条实测：

```
allocate(len) -> ptr
revision_create(prevHandle /*0=无*/, jsonPtr, jsonLen, mode /*1=全量, 0=auto*/ , outHandlePtr) -> rc
        rc: 0 ok / 1 invalid args / 2 invalid snapshot / 5 previous revision belongs to another document
revision_text_length(h) -> bytes        revision_copy_text(h, dst, len) -> written
revision_identity_length(h, i) -> len   revision_copy_identity(h, i, dst, len) -> written
revision_is_value_settable(h, i) -> 0|1|-1      revision_release(h)
```

两个坑：`prevHandle` 必须是数字 `0`（传 `null` 会命中 invalid args）、`outHandlePtr` 必须先写 0；`jsonLen` 是 UTF-8 **字节**数。另外读取缓冲要在 `revision_create` **之前**一次性大块分配并复用——创建之后再 alloc 会读到 Swift 堆里的脏数据（表现为能读出 `AXSlider` 之类的角色常量字符串）。

所以「格式对齐」是可执行的：把同一份 `WireDoc` 同时喂给它的 wasm 和我们的 `render.rs`，逐字节 diff。许可证是 Proprietary，只能用于本地验证，不能随产品分发。

### 2.3 settle：`waitForAXQuiet`，不是 sleep

`browser-service.mjs` 里的真实实现（反编译后的等价代码）：

```js
async pollForAXQuiet(tab, quiet, mode) {
  const start = Date.now(), deadline = start + 750;   // 硬上限 750ms
  const threshold = quiet;                             // 50ms 或 100ms
  let state = tab.navigationState();
  for (;;) {
    this.throwPendingError(tab.tabId);
    const lastUpdate = Math.max(start, state.lastAXUpdateMilliseconds ?? start);
    if (Date.now() - lastUpdate >= threshold || Date.now() >= deadline) return state;
    await sleep(25);                                   // 轮询 25ms
    state = this.navigationState(tab);
  }
}
```

配套还有一个导航探针，直接问页面：

```js
await cdp.evaluateJavascript(tabId, `(() => {
  const body = document.body;
  return { readyState: document.readyState,
           elementCount: body?.querySelectorAll('*').length ?? 0,
           textLength: body?.innerText?.length ?? 0 };
})()`);
```

于是「状态什么时候可信」有三个信号叠加：**AX 树的最后更新时间**、`document.readyState`、以及 DOM 节点数/文本长度的变化。这解释了文档里那句「通常不需要在动作后手工等待，runtime 会自动等待合适的时间」——它不是拍脑袋的 sleep，而是**以无障碍树静默为准的收敛判据**，且硬上限 750ms 保证不会卡死。

补充（实测）：静默窗默认 **100ms**，页面上有 JavaScript 对话框时降到 **50ms**；导航等待与首绘各 5000ms；capture 最多重试 3 次。另外 `Accessibility.enable` + `nodesUpdated` + `loadComplete` 三个 CDP 事件是这套机制的底座：执行器订阅增量 AX 更新，才能算出 `lastAXUpdateMilliseconds`。


### 2.4 分层降级：ax → playwright → dom_cua → cua

四层并列在同一个 `Tab` 对象上，优先级由文档写死、由运行时裁剪兜底：

| 层 | 目标 | 语义强度 | 备注 |
|---|---|---|---|
| `tab.ax` | `element_index` | 最高：角色/名称/次要动作/文本选择 | **默认层** |
| `tab.playwright` | locator（role/text/label/placeholder/testid/css） | 高：可断言、可等待、可批量 | 重复性任务、自测站点 |
| `tab.dom_cua` | `node_id`（`get_visible_dom()` 给出） | 中：能定点，无 locator 语义 | 「不清楚怎么用 locator 时」 |
| `tab.cua` | `x, y` | 最低：纯几何 | AX 不可用时的兜底 |

降级不是文档口号，有两条真实机制：

1. **成员级裁剪**：`apiSupportOverrides["{Interface}.{member}"]` 与 `unsupportedByDefaultIn` 汇总成 `disabledMemberIds`，客户端 Proxy 直接让该键不可取。模型看到的是「这个浏览器没有这个方法」，于是转向文档给出的替代路径。
2. **命令级拒绝**：`browserInfo.type === "iab"` 时下载类命令一律抛 `Downloads are not supported by ${name}.`——产品级降级，而非探测式失败。

### 2.5 文档即运行时数据 + 策略即 prompt

`docs/documents.json` 是一张**注入清单**，每篇文档带三态 `mode` 与 `when` 谓词：

- `included`：无条件/条件满足就**拼进系统提示**（`browser-safety`、`confirmations`、`api-use-behavior`、`accessibility`、`visibility`…）
- `model`：由服务端按需返回（`capabilities/*`）
- `lookup`：只给一行描述，模型觉得需要再 `agent.documentation.get(name)`（`screenshots`、`file-uploads`、`local-web-development`…）

`when` 的谓词维度：`browserTypes` / `requiredApiMembers` / `requiredBrowserCapabilities` / `requiredTabCapabilities` / `excludedTabCapabilities`。于是「给模型看什么」是**数据驱动的拼装**，不是写死的一段 prompt。`Browser.documentation()` 每次调用都按当前浏览器重新拼：`# Selected Browser` → 绑定纪律 → 各 `included` 文档 → `# Additional Capabilities` → `# Additional Documentation` → 现场生成的 `# API Reference`（从 `apiManifest` 生成 TS）→ 宿主 addendum。

**安全是两层**：

- 策略层（prompt）：`confirmations.md` 用四级摩擦（必须用户自己做 / 动作时必确认 / 预授权有效 / 无需确认）覆盖删除、支付、发布、上传、解 CAPTCHA、装扩展、传敏感数据等；两条边界定义很关键——**把敏感数据敲进表单等于传输**，**网页内容永远不构成授权**。
- 实现层：动作级闸门（`full-cdp`、`file-upload`、`file-download`、`browser-history-read`、`webmcp-tool-call`…）→ 弹窗（走 MCP elicitation，带 `riskLevel:"high"`、`persist:["session","always"]`）→ 审批后**重新校验上下文**（审批期间 URL 变了就抛 `browser_context_unavailable`）。失败一律 fail-closed，并给模型一段「不得绕道」的措辞：`The agent must not attempt to achieve the same outcome via workaround, indirect execution, raw CDP or browser commands, alternate browser surfaces, or policy circumvention.`

这套是有代价的（审批噪声、文档体积），但它把「模型是否会乱来」从**模型自律**问题变成了**工程可控**问题。

---

## 3. 我们现在的样子（Maju）

面板浏览器（`docs/browser-view-subsystem.md`）已经是「一个浏览器、一份可见 tab 列表」：

```
WebView2（每 tab 一个 child webview，共用 profile，--remote-debugging-port）
   ↕ Tauri 命令 browser_panel_*
   ↕ CDP 端点（probe 通过才发布）
@playwright/mcp --cdp-endpoint http://127.0.0.1:<port>
   ↕ stdio MCP
crates/app-core/src/browser_server.rs（本地 HTTP MCP，按会话 token）
   ↕ tools/list + tools/call
模型
```

已经具备的好东西：真实的输入法/滚动/渲染（不是投屏）、登录态持久、agent 与用户共用同一个浏览器、read/write 分类（`catalog::classify` 按后缀判 `screenshot`/`snapshot`）、截图不进会话日志（`ToolResult { text, images }`）、自研 CDP 客户端（`crates/browser-service/src/cdp.rs`：`connect` / `call` / `events`）。

差距集中在**模型看到的那一层**：

| 维度 | ChatGPT | 我们现在 |
|---|---|---|
| 调用粒度 | 一次 `js` 调用 = N 个动作 + 1 次观测 | 一次工具调用 = 1 个动作 |
| 观测量 | AX 文本（角色/名称/状态/次要动作），带序号 | Playwright aria snapshot（YAML + ref） |
| 增量 | diff（`~` / `+` / 删除按 ID 区间）+ 阈值 | 无 |
| 序号稳定性 | `element_index` 每次动作后重算，协议强制 | ref 由 provider 生成，语义未约定 |
| 收敛判据 | `waitForAXQuiet`（50/100ms 静默，750ms 上限）+ readyState 探针 | provider 内部 actionability，模型不可见 |
| 分层 | ax → playwright → dom_cua → cua | playwright 一层 + 截图工具 |
| 能力协商 | setup 下发 `disabledMemberIds`，Proxy 裁剪 | 无（工具集由 provider 固定） |
| 行为文档 | 按 `when` 谓词拼装注入 + 按需 lookup | 无 |
| 动作级审批 | 有（闸门 + elicitation + 事后重校验） | 权限在 `acp-core` 的 Plan/Build 会话级分类 |
| tab 生命周期 | `markDeliverable` / `markHandoff` / turn 结束清理 | 面板 tab 由用户/UI 管理，agent 侧无交付语义 |

结论：**我们缺的不是浏览器，是「浏览器与模型之间的协议」**。这也是唯一值得复刻的部分——WebView2 面板、CDP 端点、MCP 桥我们都已有。

---

## 4. 复刻方案

四期，逐期可独立上线、可独立验证。P1 一个人一周量级，P2 两周量级，P3/P4 可以长期慢慢靠近。

### P1：AX 观测层（收益/成本比最高，先做）

**目标**：把「看页面」从 aria snapshot 换成带序号、带 diff、带收敛的 AX 文本。不动动作层，不动 provider。

**新增**

- `crates/browser-service/src/ax/mod.rs` — 子系统入口
- `crates/browser-service/src/ax/wire.rs` — 从 CDP 拉 `Accessibility.getFullAXTree` + `DOM.getDocument(depth:-1, pierce:true)` + `DOM.describeNode`，拼出 wire 节点（字段集合照 §2.2）
- `crates/browser-service/src/ax/render.rs` — 文本渲染：只对有名字或可交互的节点编号；每行 `#index role "name" {states}`；折叠空容器；行预算
- `crates/browser-service/src/ax/diff.rs` — 按 (role, name, 结构位置) 对齐前后两棵树；`~` / `+`；删除按 ID 区间；带 `MIN_SAVED_RATIO` / `MIN_SAVED_BYTES` 阈值
- `crates/browser-service/src/ax/settle.rs` — `wait_for_ax_quiet`：订阅 `Accessibility.nodesUpdated` / `Page.lifecycleEvent`，50/100ms 静默、25ms 轮询、750ms 上限，叠加 `Runtime.evaluate` 的 `readyState`/`elementCount`/`textLength` 探针

**接入**

- `crates/app-core/src/browser_server.rs`：新增工具 `browser_observe`（参数 `{ tab_id?, mode: "state"|"screenshot"|"both", disable_diffing? }`），走我们自己的 `cdp.rs` 连面板端点，**不经过 `@playwright/mcp`**（CDP 支持多客户端，互不干扰）
- 每次工具调用结束把「本会话最后一次 AX 树 + 文档 key」存进 per-session 状态，作为下次 diff 的基线；跨文档（`mainFrameId + loaderId` 变化）自动失效
- `catalog::classify` 需要认识 `observe` 是 read（后缀规则已经覆盖 `snapshot`，把 `observe` 加进 read 集）

**验收**

1. 同一页面：`browser_observe` 的字符数 ≤ aria snapshot 的 50%（用三个真实站点量：新闻页、表单页、SPA 列表页）
2. 点一个按钮后再 observe：输出以 diff 开头，且不含整页重复内容
3. 快速连续改动（注入 3 次 DOM 变更）：observe 在最后一次变更后 ≤ 400ms 返回，且结果包含全部 3 次变更
4. 文档 key 变化（跨页导航）后 observe 退回全量而非错误 diff

**已落地（`crates/browser-service/src/ax/`，50 个单元测试）**

| 文件 | 职责 | 是否需要浏览器 |
|---|---|---|
| `wire.rs` | `Accessibility.getFullAXTree` + `DOM.getDocument` + 页面探针 → `WireDoc`（含 `nameSource`、`ignored`、ARIA 态、tag/class） | 否（纯 JSON 解析） |
| `identity.rs` | 结构化身份 → `u64` 内部号，供 diff 对齐；导航即失效 | 否 |
| `render.rs` | 精确复刻上面的行格式（tab 缩进、行号、role 映射、name 降级、`Help/Value/URL` 后缀、行/字符预算） | 否 |
| `diff.rs` | `~`/`+` 前缀、`Removed element IDs:` 区间压缩、固定 header 与「无变化」句、两个阈值 | 否 |
| `settle.rs` | 100ms 静默 / 25ms 轮询 / 750ms 上限，闭包注入探针（可测） | 否（注入 fake probe） |
| `session.rs` | attach → 探针 → 静默 → AX+DOM+frameTree → 构建 → detach | 是 |
| `mod.rs` | `Observer`：一份 per-tab 的 id 表 + 基线 + `target_id`，决定这次给全量/diff/无变化，并把行号解析回 DOM 节点 | 否 |
| `action.rs` | `parse_action` / `perform`：行号 → `backendDOMNodeId` → `DOM.resolveNode` → `Input.*`；键表、坐标取法、错误分类 | 是（`perform`），表解析与键表否 |

**接线（`app-core`）**

- `browser_mcp.rs`：`BrowserMcpService` 增加 `observers: Mutex<HashMap<token, Observer>>` 与 `observe(token, mode)` / `act(token, index, action, value)`。CDP 连接、静默、抓取都在锁外完成，锁只包住同步的判定——一个慢页面不会把别的会话的工具串行化。`unregister_session` 连同 token 一起丢弃基线，避免后来的会话拿到「它没读过的页面」的 diff。
- `browser_server.rs`：工具名 `mcp__browser-use__browser_observe` 与 `mcp__browser-use__browser_act`（独立 server 段，一眼能看出不是 provider 的），`tools/list` 里追加在 provider 工具之后；`tools/call` 在 **prime 之前**拦截，所以 provider 挂掉也仍能观测并驱动页面。
- `provider` 起不来时 `tools/list` 返回**只含我们这两个工具**的列表，而不是报错：空列表会被客户端缓存，报错会让 agent 连「看一眼页面」都做不到；两种都坏，后者更坏。
- 读/写分类：`catalog::classify` 与 `acp-core` 的 `mcp_leaf_tool_name_is_read_only` 都把 `observe` 归入读，Plan 模式下不弹权限；`act` 落在写类，照常走会话策略。

**动作层（对应 §2.4 的 AX 动作，见 `action.rs`）**

- 链路：行号 → capture 时记下的 `backendDOMNodeId` → `DOM.resolveNode` → `Input.dispatchMouseEvent` / `DOM.focus`+`Input.insertText` / `Input.dispatchKeyEvent`。
- 坐标**不用 box model**：`scrollIntoView()` + `getBoundingClientRect()` 一次往返就拿到视口坐标，省掉「文档坐标 → 视口坐标」的滚动偏移换算。
- `fill` 先 `select()` 再 `insertText`（覆盖），`type` 只插在光标处；两者都走 `Input.insertText`，因为 `value =` 不触发框架监听的事件。
- 键表只收会推进表单的十几个键，表外直接拒绝并列出可用项；有文本的键用 `keyDown`、纯控制键用 `rawKeyDown`。
- 动作绑定 `target_id`：序号不会落到模型没读过的那个 tab 上。


**三处有意偏离**（都能一眼看出，也都写了理由）：

1. **行号是稳定的元素身份，不是行位置。** 它每次 capture 重排行号；我们改成「元素第一次出现时拿到一个号，活着就一直用它」。理由是量出来的：一个页面上「填三个字段再提交」的回合，位置编号版每步都退化成全量（1403/1463/1481/1545 字符），因为一次插入会让下面所有行重新编号；稳定编号版每步只回 diff（110/139/67/113 字符）。同一个回合从 7259 字符降到 1796。
2. **元素身份用 `backendDOMNodeId` 而不是结构化路径。** 它用 `tabId|targetID|backendDOMNodeID`，我们最初用「role/name/兄弟序号」的路径，实测在一次提交后有 30 多行被误判成「删除 + 新增」——一个祖先的序号变了就重排整棵子树。改成 DOM id 后消失。
3. **有预算**（1200 行 / 48K 字符）、**role 映射表只覆盖证据里出现过的几个**（其余透传）、**diff 词头缩短**（长版 172 字符，占单步 diff 的四分之三以上，换成 46 字符的短版）。


### P2：code-mode（复刻的核心动作）

**目标**：给模型一个 `js` 工具，一次调用完成「多个动作 + 一次观测」。

**新增** `tools/browser-repl/`（Node ESM，独立于 Rust 仓库）

- `host.mjs` — stdio MCP server，暴露 `js`（执行代码，返回 stdout + 最后表达式值 + 模型可见的图像）与 `js_reset`
- `sdk/browser.mjs` — `browser` / `browser.tabs` / `Tab`，方法名对齐 §2.4 的 `ax.*` / `playwright.*` / `cua.*`（先实现 `ax` 全套 + `goto/reload/url/title/screenshot`）
- `cdp.mjs` — 极简 CDP over WebSocket 客户端（Node 侧自带 `ws`，或用 Node 22 的全局 `WebSocket`）
- 执行沙箱：`node:vm` 上下文，`timeout`（默认 30s，可传 `timeout_ms`）、stdout 截断（默认 8KB）、未捕获异常转成结构化 `{message, stack}` 回给模型
- 观测自动回传：`ax.write()` 不靠模型 `console.log`，由 SDK 直接调 MCP 的「内容项」通道把文本/图像送进结果（对照它的 `displayBridge`）

**接入**

- `crates/app-core/src/shared_mcp.rs`：面板端点发布后，注册 `browser_repl` 这个 stdio server（传 `KODEX_BROWSER_CDP=<endpoint>`），替代默认的 provider 注入
- 若 provider 仍要保留（本地 WebView2 不支持 `Target.createTarget` 等），则两套并存：`js` 为默认，`browser_*` 保留为逃生口

**风险与对策**

- 模型写出死循环 → 沙箱超时 + 每轮调用次数上限
- 输出污染上下文 → 截断 + 结构化错误
- 句柄失效 → `Tab` 方法失败时抛可读错误，并在错误里提示重新 `tabs.get()`（对照它的 `Tab is not bound to a transport`）
- 这个改动会改变模型的使用习惯 → P1 的 AX 文本必须先到位，否则 `js` 里没有好用的观测原语

**验收**：同一个任务（「在本地起的表单页填 3 个字段并提交，然后告诉我结果」）在旧路径与新路径下的**工具调用次数**与**总 token** 对比；目标是调用次数 1/3 以下、token 1/2 以下。

### P3：分层降级与能力协商

**已落地**

- **坐标动作**：`browser_act` 接受 `index` 或 `x`/`y`，两条路径走同一个动作实现。补上了
  `NoNode` 报错里那句「改用位置点击」——在此之前那句话指向一个不存在的能力。
- **滚动**：`action: "scroll"` + `delta_x`/`delta_y`。实测 `Input.dispatchMouseEvent
  { type: "mouseWheel" }` 在 headless Edge 上不产生滚动（`scrollY` 不变），改用
  `Input.synthesizeScrollGesture` 才生效，且它的距离符号与滚轮增量相反。
- **视口**：`browser_viewport`（`Emulation.setDeviceMetricsOverride` / clear）。
- **未做**：`visibility` 能力。面板的显示/隐藏是 Tauri 侧命令，跨 crate 回调，收益
  （把页面摆到用户面前）不如它带来的耦合大；等有真实需求再补。

**与原生驱动那条路的关系**：合并 `2d54682`（macOS 子 webview + 原生工具驱动）之后，
两套工具面并存但按平台分流——原生驱动只在其 webview 没有 CDP 的平台发布，所以
`panel_driver::get().is_some()` 就是「观察工具在这里永远拿不到端点」的判据。工具列表、
`initialize` 引导文本、以及误调用时的报错，三处都按它切换，详见
`browser-view-subsystem.md` 的「平台差异」一节。

### P4：行为文档与动作级审批

**已落地（文档部分）**

- `docs/browser/` 四篇模型面文档 + `crates/app-core/src/browser_docs.rs`：
  三篇走 `initialize.instructions`（约 6.6k 字符），一篇走 `browser_documentation` 按需读取。
  有测试守住 8k 字符的预算，超了就说明该把某篇降级成 lookup。
- `confirmations.md` 按我们真实的动作面裁剪：只留 click / fill / press / scroll 能造成的
  后果（删除、付款、发送、改权限、提交不可撤销的东西、输入敏感数据），去掉了 WebMCP、
  GSuite 导出、CAPTCHA 之类我们没有的条目。

**未做（以及为什么）**

- 机械闸门（上传 / full-CDP / history 的动作级审批）：我们现在的工具面里没有上传、没有
  原始 CDP、没有历史记录，唯一能做危险动作的是 `browser_act` 点一个按钮——那是策略文档
  该管的，不是闸门该拦的。等这些能力进来时，闸门要和它们一起设计，否则只是给「点一下」
  加一层没人需要的弹窗。

---

## 5. 关键取舍

| 选择 | 理由 |
|---|---|
| 先做 Rust 的 AX 层，再做 Node 的 code-mode | AX 文本是 `js` 里最好用的观测原语；没有它，code-mode 只是把 10 次 JSON 调用变成 10 行 JS，收益有限 |
| `browser_observe` 走我们自己的 CDP，不寄生 provider | 面板端点已知；多客户端互不干扰；避免为了一个观测工具去改 provider 的参数面 |
| 序号「每次重算」而非持久 ID | 持久 ID 会在 DOM 复用下指错元素；重算 + 强制重读是代价最小、语义最清楚的做法 |
| diff 要阈值，不要无条件 diff | 小改动时 diff 比全量还长；阈值让「省 token」不变成「省出歧义」 |
| 策略文档与审批闸门分开做 | 文档便宜且立刻改善行为；闸门要改权限链路，风险更高，值得单独一期 |

**不建议照抄的**：`browser-service.mjs` 那套 113 个 CDP 方法的全集、Chrome 扩展 + native messaging 的双进程桥、`mcpapps` 后端、`content.exportGsuite` 之类的产品特化。我们只有一个后端（WebView2 面板），面铺开是纯负担。

---

## 6. 验证方法（怎么证明「行为一致」而不只是「功能可用」）

### 实测（本机 Edge 148 headless + 一个真实表单页，`cargo run -p browser-service --example ax_probe`）

页面：header/nav（4 链接）/ h1 + 段落 / 6 个表单控件 / 5 行表格。同一页面同样内容：

| 指标 | 今天给模型的 `browser_snapshot`（Playwright aria snapshot, `mode: ai`） | `browser_observe` |
|---|---|---|
| 首次全量 | 2040 字符 / 71 行 | 1367 字符 / 53 行 |
| 填字段后的观测 | 又是一次全量（ref 会失效） | 110 / 139 / 67 字符 |
| 提交后的观测 | 又是一次全量 | 113 字符 |
| 「填 3 个字段 + 提交」整回合 | 8160 字符（4 次全量） | **1796 字符** |

≈ **4.5 倍**。省下来的不只是体积：diff 直接告诉模型「哪一行变了」，它不用再从整页里重新找。

**往返次数**（这才是 P2 要解决的问题）：`browser_act` 默认连观测一起返回，所以常见路径
是**一步一次调用**（动作 + diff 在同一个结果里）；只有连续做多个动作、中间状态没用时
才 `observe: false`。和它的 `js` 调用相比，剩下的差距是「一个调用里能不能放多个动作 +
分支/循环」，不是每次交互的往返数。

测量过程中被这个台子抓到的三个真问题（都修了）：文本节点被渲染三遍（`InlineTextBox`）、`fill` 因为缺少 `Emulation.setFocusEmulationEnabled` 而静默失效、以及元素身份用结构化路径导致提交后大面积误判。

### 其余验证

1. **格式对齐**：把同一个页面的 wire DOM 同时喂给我们的 `render.rs` 与它的 wasm（本地跑，见 §2.2 的导出表），逐行 diff。差异只允许出现在我们**有意**不同的地方（例如不渲染 `PresentationActions`）。
2. **收敛对齐**：同一组动作序列，记录它的 `waitForAXQuiet` 等待时长与我们 `settle.rs` 的等待时长，中位数差距应 < 100ms。
3. **行为对齐**：把它的 `api-use-behavior.md` / `accessibility.md` 的行文改写成我们的版本后，用同一批任务跑 A/B，比较「无效动作次数」与「重复观测次数」——这两个指标比总 token 更能说明行为是否真的被复刻。

---

## 7. 证据索引

| 结论 | 证据 |
|---|---|
| 插件无 MCP 工具面，只有 3 个 hook → `node_repl.turn_ended` | `browser/.codex-plugin/plugin.json` |
| 客户端只导出 `setupBrowserRuntime()` | `scripts/browser-client.mjs`（唯一 `export`） |
| API 面运行时分发、Proxy 裁剪 | `docs/api.json`（`interfaces` 23 个 / `types` 55 个 / `root: "Agent"`）+ `disabledMemberIds` |
| 注入清单三态与 `when` 谓词 | `docs/documents.json` |
| diff 文案与删除区间 | `browser-accessibility.wasm` 字符串常量 |
| diff 阈值 | `TINYSKY_AX_TREE_DIFF_MIN_SAVED_RATIO` / `TINYSKY_AX_TREE_DIFF_MIN_SAVED_BYTES` / `AccessibilityDifferenceLineBudgetExceeded` |
| wire DOM 字段集 | wasm 常量区：`ChromeAccessibilityWireNode/DOM/Value`、`tabID/frameID/nodes/parentIndex/childIDs/backendDOMNodeID`、ARIA 状态词表 |
| `waitForAXQuiet` 参数 | `browser-service.mjs`：`pollForAXQuiet`（静默 50/100ms、轮询 25ms、上限 750ms）+ `probeNavigationState` |
| CDP 面 | `browser-service.mjs` 中 113 个 `"Page.*"/"DOM.*"/"Input.*"/"Accessibility.*"/"Runtime.*"/"Target.*"` 字面量 |
| 闸门与错误分类 | `browser-service.mjs`（`GV`/`KV` 闸门集）+ `browser-client.mjs`（17 个 `decisionSource`/`retryable` 错误码、`guardian_*` 自动复核） |
| 策略文本 | `docs/confirmations.md`、`docs/browser-safety.md`、`docs/accessibility.md`、`docs/api-use-behavior.md`、`docs/visibility.md`、`docs/screenshots.md` |
| wasm 是 Swift 产物、可独立驱动 | wasm 导入 34 个 wasi 符号、导出 9 个 `computer_use_browser_wasm_*`；字符串含 Swift 运行时提示 |
