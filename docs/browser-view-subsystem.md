# 内置浏览器子系统（Browser View Subsystem）

## 目标

右侧面板的「浏览器 tab」与 browser-use MCP 工具（`@playwright/mcp`）**共用同一个
浏览器**：

1. 点击 UI 中的 http(s) 链接 → 在右侧面板开一个 tab，显示该 URL 的**实时页面**。
2. agent 的 browser-use 工具控制的就是右侧这个浏览器；工具的导航/点击会实时
   反映在右侧 tab 里。
3. 用户可以直接在右侧 tab 里点击、输入、滚动，等价于一个 Chrome 页面 tab。

## 当前实现：面板自己就是浏览器（每 tab 一个 WebView2）

右侧面板不再投屏受管 Chromium，而是**自己就是那个浏览器**：每个 tab 是应用主窗口的
一个 WebView2 子 webview（`Window::add_child`），有自己的 profile
（`~/.kodex/browser/panel`，登录态跨重启保留），启动参数带
`--remote-debugging-port=<本次运行选定的端口>`。渲染、滚动、光标、输入法全部是平台
自己的：页面里的输入框就是输入框 —— 之前「打字跟不上手」「候选窗位置别扭」都源于把
页面画成图片再回灌按键。

```
WebView2（面板的 tab：每 tab 一个 child webview，共用一份 profile）
   ↑ bounds / activate / hide
Tauri 命令 browser_panel_*  ←→  EmbeddedBrowser.tsx（只画工具栏和那个「洞」）
   ↑ --remote-debugging-port=<面板端口>
agent 的 browser-use 工具（@playwright/mcp --cdp-endpoint http://127.0.0.1:<面板端口>）
```

契约（实现见 `apps/desktop/src-tauri/src/browser_panel.rs`）：

| 命令 | 参数 | 说明 |
|---|---|---|
| `browser_panel_state` | — | `{ tabs, active, endpoint }`；`endpoint` 就是 agent 该连的 CDP 地址。 |
| `browser_panel_open` | `url` | 新建 tab（webview 的 label 即 `tab_id`），返回 `{ tab_id, url, title }`。 |
| `browser_panel_close` | `tab_id` | 关 tab 并销毁 webview；profile 与登录态保留。 |
| `browser_panel_activate` | `tab_id` | 显示该 tab（连浏览器一起显示），隐藏其余。 |
| `browser_panel_hide` | — | 不关页面，只把 webview 藏起来（面板正在显示别的 tab）。 |
| `browser_panel_navigate` | `tab_id, url` | 裸域名按 `https://` 补全；只接受 http/https/about/file/data/blob。 |
| `browser_panel_reload` | `tab_id` | 重新加载当前 URL。 |
| `browser_panel_history` | `tab_id, direction` | `back` / `forward`。 |
| `browser_panel_bounds` | `x, y, width, height` | 面板上报「洞」的位置，逻辑像素（= CSS 像素）。 |
| `open_external_url` | `url, source` | 只承载面板渲染不了的协议（`mailto:` / `tel:` 等）。http(s) 在这里**被拒**并记 `warn`。 |

| 事件 | payload |
|---|---|
| `browser_panel:tabs` | `{ tabs, active }`：增删、切换、标题变化后的全量列表。 |
| `browser_panel:page` | `{ tab_id, url, event: "started" \| "finished" }` |
| `browser_panel:url` | `{ tab_id, url }`：跳转、重定向、页内点击。 |
| `browser_panel:new_window` | `{ tab_id, url }`：页面要求新窗口（`target="_blank"` / `window.open`）。 |

原生 webview 画在应用自己的 DOM **之上**，于是：

- 「洞」必须保持空：任何画在洞里的东西都会被页面盖住，加载状态只能放工具栏。
- 矩形为 0（面板收起 / `display: none`）时后端隐藏 webview，否则它会浮在别的内容上。
- 面板重新出现时尺寸可能与消失前完全相同（没有任何元素尺寸变化），所以
  `EmbeddedBrowser` 还按 `layoutSignal` 重测一次；`ResizeObserver` 与 window resize
  覆盖其余情况。
- 面板切到非 web tab、或整体展开/收起（ReviewPanel 卸载）时，组件卸载 →
  `browser_panel_hide`。
- 页面里 `target="_blank"` 的链接没有第二个窗口可去，wry 默认直接拒绝（点了没反应）；
  `on_new_window` 把请求发成 `browser_panel:new_window`，前端照常开 tab。
- 页面标题来自 `on_document_title_changed`（`browser_panel:tabs` 里带回），拿不到时
  退回 URL 的 host。

### agent 控制接口：面板浏览器 = 工具驱动的浏览器

工具那一侧不新增任何东西，只是**指向面板**：

1. 面板的浏览器进程用 `--remote-debugging-port=<端口>` 启动（端口在本次运行里固定），
   WebView2 因此提供 `http://127.0.0.1:<端口>` 的 CDP 端点（Playwright 官方文档的
   WebView2 接法：`chromium.connectOverCDP(endpoint)` → `contexts()[0].pages()[0]`）。
2. 浏览器一起来，桌面端就把端点发布到 `app_core::panel_browser::publish`
   （`crates/browser-service/src/panel.rs` 的进程级槽位）—— 但要先**自检通过**，
   见下一节。
3. `BrowserFactory::acquire`（`crates/browser-service/src/lib.rs`）**优先**用这个端点：
   有面板浏览器时，无论 settings 写的是 launch / persistent / attach，都不再启动
   受管 Chromium，直接把 `--cdp-endpoint <面板端点>` 交给 playwright-mcp。
   面板浏览器是「一个浏览器、一份可见的 tab 列表」，所以这时注册表按独占处理
   （同一时刻只有一个会话在驱动它）。
4. playwright-mcp 连接时 `Context._initializeBrowserContext` 会把已存在的 page
   target 收成 tab、第一个成为当前 tab，所以工具操作的就是面板里那个页面 ——
   不再需要第二套「agent 的页面」。
5. 由此，**预检不再要求本机装 Chromium**：`check_browser_with` 在有面板浏览器时直接
   Ready（Node 与固定版本的 provider 包仍然必需，因为工具本身还是那个 provider 进程）。
   `executable_path` 这类「由谁启动浏览器」的配置在面板存在时是惰性的。

为了让上面这条链路在任何时刻都成立，面板自己做两件事：

- **保活**：面板始终留一个隐藏的空白页（"warm" 页），它不是一个 tab。这样即使一个
  tab 都没开，浏览器（以及 CDP 端点）也是活的；关掉最后一个 tab 时立刻再补一个。
  应用启动时若 `browser.enabled` 为真就预热（`main.rs::warm_panel_browser`），
  在设置里打开 browser-use 时也会预热。
- **上位**：空白页里第一次出现真实 URL（agent 导航，或用户点链接）时，它就地变成
  一个 tab 并广播 `browser_panel:tabs` —— agent 打开的页面因此出现在面板里，用户能
  看到 agent 在看什么。前端只在该面板正显示 web tab 时跟随切换，不抢正在看的 diff。

### 端点只有在连得上时才发布

发布端点是**有代价**的：`acquire` 一旦看到面板端点，就再也不会回退到 settings 里
那个浏览器。所以「发布了一个没人应答的地址」不等于「工具还能用别的浏览器」，而是
等于「工具全挂」。因此发布前先自检，见 `app_core::panel_browser::probe`：

- 用 `crates/browser-service/src/cdp.rs` 的 `CdpClient` 连上去，`Browser.getVersion`
  拿到产品串（WebView2 是 `Edg/...`），`Target.getTargets` 数出一共有几个 `page`
  target。
- 连上去要同时满足两件事才会发布：**DevTools 有应答**，且**至少有一个 `page`
  target**。第二件同样必要 —— WebView2 没有 `Target.createTarget`，把一个没有页面的
  端点交给工具，等于让工具连上一个开不了新页的浏览器。
- 连不上就**不发布**，并按 200ms 一次、最多 5s 重试 —— `add_child` 返回时 webview
  已存在，但背后的浏览器进程还要一点时间才开监听。
- 自检发生在三个时刻：应用启动预热时、每次**打开页面**时、以及每次**把某个 tab
  显示出来**时（`browser_panel_activate`）。已经发布过就立刻返回（一次字符串比较），
  所以这是免费的；没发布过就再试一次 —— 否则「启动那 5 秒没赶上」或「那一刻还没有
  页面」会变成「整个运行期都失去面板」。
- 始终不满足 → 不发布，工具按 settings 走原来的路（仍然可用，只是不在面板里），
  并记一条 `warn` 说明是哪一件不满足。
- 发布时日志里带上 `product` 与 `pages`；不发布时那两行 `warn` 就是「agent 为什么
  没在驱动面板」的全部答案。
- 自检结果只是诊断，不影响面板本身：面板照常工作，用户该看到什么还是看到什么。

`~/.kodex/logs/app.log` 里对应的几行（`target: "browser_panel"`）：

| 日志 | 含义 |
|---|---|
| `panel browser is warm` | 预热页建好了，带 `endpoint`。 |
| `panel browser answers DevTools; the agent's tools drive it` | 自检通过并已发布，带 `product` / `pages`。 |
| `panel browser answers DevTools but lists no page; ...` | 连得上但没有可驱动的 page，未发布。 |
| `panel browser never answered DevTools; ...` | 端点不可达，未发布，工具仍按 settings 走。 |

已知限制（WebView2 的 CDP 面比 Chromium 窄）：

- `Target.createTarget` 不被支持，所以工具自己「新建标签页」会失败；让它用
  `browser_navigate` 走当前页面即可（页内 `target="_blank"` 链接不受影响，见上）。
- 端点由面板的浏览器进程提供，进程随窗口关闭而消失；面板保证「至少有一个 page」，
  所以只要应用在跑，工具就有页面可操作。

### 观测：`browser_observe`（我们自己的，不是 provider 的）

provider 的 `browser_snapshot` 返回的是 Playwright 的 aria snapshot：有结构、没有
地址、也没有「和上次比变了什么」。模型因此每步都要重读整页，而且没法用序号精确
指代元素。`browser_observe` 补的就是这一层：

```
0 AXWebArea Example Domain, URL: example.com/path
	1 heading Example Domain
	2 text field Description: Email, Value: a@b.c
	3 button Sign In
```

- **行 = `("\t" × depth) + 序号 + " " + role + " " + name`**，可选 `Help:` /
  `Value:` / `URL:` 后缀。role 走一张映射表（`RootWebArea → AXWebArea`、
  `textbox → text field`、`StaticText → text`）；`nameSource: attribute` 的 name 与
  所有 link 的 name 降级成 `Description:`；只渲染 `checked`/`selected`，
  `disabled`/`focused`/`level` 一律不渲染；`InlineTextBox`、空文本、以及重复父级
  名字的文本子节点都不占行。
- **序号是元素的，不是行位置的**：元素第一次出现时拿到一个号，活着就一直用它
  （根固定是 0）。所以插入一个元素不会让它下面所有行重新编号——这正是 diff 能小
  的前提。原实现每次重排；我们量过代价：位置编号版「填三个字段再提交」要走
  7259 字符，稳定编号版 1796。
- **默认只回变化**：`~` 改、`+` 增、`Removed element IDs: 2-4, 9` 汇总删除；没有变化
  时是一句固定的 "There has been no change in the accessibility tree."。`mode: "full"`
  要全量。diff 省不到阈值（<50% 且 <512B）时自动退回全量——比让模型猜便宜。
- **元素身份用 `backendDOMNodeId`**：结构化路径（role/name/兄弟序号）在一次提交后
  会把 30 多行误判成「删除 + 新增」，因为一个祖先的序号变了就重排整棵子树。没有
  DOM 对应物的合成节点才退回路径。
- **收敛靠静默，不靠 sleep**：`waitForAXQuiet` 的等价物是 100ms 静默窗（25ms 轮询、
  750ms 上限）+ `readyState`/元素数/文本长度探针，见 `browser-service/src/ax/settle.rs`。
- **它不经过 provider**：自己开一条 CDP 连接到同一个面板端点（CDP 支持多客户端），
  所以 provider 起不来时 `tools/list` 仍只列这一个工具，agent 至少还能看见页面。

实现分布在 `crates/browser-service/src/ax/`（wire / identity / render / diff /
settle / session / action）与 `crates/app-core/src/browser_mcp.rs::observe/act`；
工具名是 `mcp__browser-use__browser_observe` 与 `mcp__browser-use__browser_act`，
`app-core/src/browser_server.rs` 在 prime 之前拦截。设计取舍与它对照的那套实现见
[`browser-use-replication.md`](./browser-use-replication.md)。

### 动作：`browser_act`（按序号点，不是按坐标猜）

序号要能变成节点，才有意义。链路是：**行号 → capture 时记下的 `backendDOMNodeId`
→ `DOM.resolveNode` → 真实输入事件**。

- `click` / `double_click`：先在元素上 `scrollIntoView()`，再读
  `getBoundingClientRect()` 取中心，然后 `Input.dispatchMouseEvent` 按压力/释放成对
  发送。**不用 `DOM.getBoxModel`**：它是文档坐标系，而输入事件是视口坐标系，换算要读
  `Page.getLayoutMetrics` 还要处理滚动偏移；问元素自己要 rect 一次往返且已经在正确
  坐标系里。
- `fill`：focus + `select()`（contenteditable 用 Range 全选），再 `Input.insertText`
  覆盖；`type` 只 focus 后在光标处插入。两者都走 `Input.insertText`——`value =` 赋值
  不触发框架监听的事件，页面不会更新。
- `press`：只支持会推进表单的那几个键（Enter/Tab/Escape/Backspace/Delete/Space/方向/
  Home/End/PageUp/PageDown），键表外的名字直接拒绝并列出可用项，不让模型猜第二次。
  有文本的键用 `keyDown`，纯控制键用 `rawKeyDown`（后者才是浏览器当真实按键处理的那种）。
- **动作默认连观测一起返回**（`observe: true`，可关）。一次动作几乎总会改变页面，所以
  「做了它，并告诉我变了什么」是最省的调用——一次往返而不是两次。只有连续做多个动作、
  中间状态没用时才该关掉。动作成功、随后的观测失败时，结果仍是成功，附一句说明，
  免得模型把已经发生的点击重试一遍。
- **动作绑定在「那次 capture 的那个 tab」上**：`Observer` 记着 `target_id`，所以序号
  不可能落到模型没读过的页面上。

已知约束：元素必须有 `backendDOMNodeId` 才能按序号动作；纯 ARIA 合成节点（没有 DOM 对应）
会被拒绝并提示改按坐标。零尺寸元素（隐藏）也会被拒绝，否则点击会落在页面角落。

### 两种瞄准方式，以及为什么需要第二种

- **按序号**：无障碍树里的一切。默认，也是唯一「可核对」的方式。
- **按坐标**（`x` / `y`，从截图取）：只用于树里根本没有的东西——canvas、自绘控件、
  跨域框。有可用的序号时不要用坐标：坐标是一发猜中的子弹。

坐标路径与序号路径走同一套动作实现（`Target::Element` / `Target::Point`），
所以「点到了没」的判定标准一致。

**滚动不用 `Input.dispatchMouseEvent { type: "mouseWheel" }`**：实测在 headless
Edge 上事件发出去了、页面纹丝不动（`scrollY` 不变）。改用
`Input.synthesizeScrollGesture`——它的距离参数与滚轮增量符号相反（正数向上/向左），
所以在入口翻转一次。实测 `dy: -200` 让 `scrollY` 从 398 变 198，正好 200px。

### 视口：`browser_viewport`

`Emulation.setDeviceMetricsOverride` / `clearDeviceMetricsOverride`，用于响应式自测。
只有在用户点名了尺寸、断点或设备宽度时才该用；覆盖会一直生效到下一个人清掉它，
所以工具描述里强调了收尾要 `reset`——否则之后每一次观测和截图都在那个尺寸下。

### 引导：工具会说话，但话要省着说

`docs/browser/` 下四篇模型面文档，三篇随 MCP `initialize` 的 `instructions` 一次性
送达（usage / safety / confirmations，约 6.6k 字符 ≈ 1.7k token），一篇
（troubleshooting）只在 `browser_documentation` 里按名索取。这条 `instructions` 通道
是 MCP 标准字段，客户端可以用也可以扔——扔了也不致命，因为 `browser_documentation`
这个工具本身就把「什么时候读哪篇」写在描述里。


## 架构

```
apps/desktop/ui  EmbeddedBrowser.tsx / usePanelBrowserTabs.ts / browserTabs.ts
        ↕ tauri command + event（browser_panel_*）
apps/desktop/src-tauri/src/browser_panel.rs     ← 面板浏览器本体（webview 生命周期、
        │                                          tab 列表、CDP 端口、事件）
        │ publish(endpoint)，先过自检
        ↓
crates/app-core/src/panel_browser.rs            ← 应用侧入口：槽位 + probe 自检
crates/browser-service/src/panel.rs             ← 进程级端点槽位
crates/browser-service/src/lib.rs               ← BrowserFactory::acquire：面板端点优先
crates/browser-service/src/cdp.rs               ← CDP over WebSocket（自检用）
        ↓
@playwright/mcp --cdp-endpoint http://127.0.0.1:<面板端口>
```

各模块的职责边界：

- `crates/browser-service/src/panel.rs`：只存一个 `Arc<RwLock<Option<String>>>`。
  谁发布、谁读，都不认识 Tauri。
- `crates/browser-service/src/lib.rs`：`acquire` 的优先级与独占判断在这里，是
  「面板浏览器 = 工具驱动的浏览器」唯一的落点。
- `crates/app-core/src/panel_browser.rs`：应用侧入口（`publish` / `clear` /
  `endpoint` / `warm_required`）+ 发布前的 `probe`。桌面壳是唯一的发布者：它拥有
  webview，也就只有它知道面板浏览器在不在。
- `crates/app-core/src/browser_preflight.rs`：有面板浏览器时短路掉 Chromium 检查。
- `apps/desktop/src-tauri/src/browser_panel.rs`：webview 与 tab 的唯一真相来源，
  包括端口分配、profile 目录、`on_page_load` / `on_document_title_changed` /
  `on_new_window` 三个回调。
- `apps/desktop/ui/src/features/browser/usePanelBrowserTabs.ts`：把 `browser_panel:*`
  事件收敛成面板的 tab 列表状态。
- `apps/desktop/ui/src/features/browser/browserTabs.ts`：链接分发（面板 or 系统），
  由 `MarkdownBody` / `SearchResults` 调用。

## 历史：为什么不是投屏

第一代实现是**投屏**：面板里的 `<img>` 画受管 Chromium 的 `Page.startScreencast`
帧，用户的鼠标/键盘/输入法经 CDP `Input.dispatch*` 回灌。它有三条绕不过去的毛病，
也正是这次重做的原因：

- **打字要等图片**：每一次按键都得先回到浏览器、再等一帧画面回来，中文输入法尤其
  明显（组字过程也要一个来回）。
- **输入法没有落点**：页面是图片，系统输入法的候选窗只能挂在面板里额外造的一个
  隐藏输入框上，候选窗位置只能靠 `browser_view_caret` 反查页面的光标位置去凑。
- **两次打开很慢**：链接点击要先确保受管 Chromium 起来、CDP 连上、screencast 开播。

内置 WebView2 之后这三条一并消失：没有帧、没有回灌、没有第二套浏览器。这一代删掉的
东西（`browser-service/src/view.rs`、`app-core/src/browser_view.rs`、
`app-core/src/browser_panel.rs`、`commands/browser_view.rs`、
`BrowserLiveView.tsx`、`BrowserPanel.tsx`，以及 `browser_view:*` 事件与
`browser_view_*` 命令、`tests/live_view_smoke.rs`）不再保留。

## 兼容与回退

- **面板浏览器没起来**（WebView2 不可用、端口不可达、自检没过）→ 端点不发布，
  `acquire` 按 settings 走原来的路：launch / persistent 启动受管 Chromium，
  attach 连用户自己的端点。工具仍然可用，只是页面不在面板里；日志里有上面那张表
  对应的一行。
- browser-use 未安装/被禁用/远程工作区 → 链接不跳转（只记日志），也不打开系统浏览器：
  与面板并排弹出的第二个浏览器窗口正是面板要替代的东西，且会掩盖失败。
  仅 `mailto:`/`tel:` 等面板无法承载的协议交给系统处理（`open_external_url`）。
- 关掉面板里最后一个浏览器 tab **不能杀掉浏览器**：Chromium 在最后一个页面关闭时会
  退出进程，而这个浏览器同时是 agent 工具在用的那一个；它一死，之后每次点击都只剩
  `connection refused`，界面表现成「点了没反应」。所以 `browser_panel_close` 关掉最后
  一个 tab 后立刻补一个空白页，空白页不占 tab。
- 面板浏览器是**共享资源**：同一时刻只有一个会话能驱动它（注册表按独占处理），
  别的会话要等当前会话释放。
- 面板工具栏不再有「在系统浏览器中打开」按钮：它是应用内最后一条通向 Chrome 的
  路径，与「链接只在右侧面板渲染」相冲突。
- 事件监听进 `lib/events.ts`，命令包装进 `lib/tauri.ts`，类型进 `src/types/`。

## 装完怎么验证

面板浏览器的核心主张是「**面板里那个浏览器，就是 agent 工具驱动的那个**」。从外面
能看到的所有证据都在下面这张清单里，按顺序做完就能判定：

1. **日志给出端点。** `~/.kodex/logs/app.log`（时间是 UTC）里搜 `browser_panel`，
   应有两行：
   - `panel browser is warm endpoint=http://127.0.0.1:<port>`
   - `panel browser answers DevTools; the agent's tools drive it`（带 `product=Edg/...`
     与 `pages=1`）
   若第二行缺失，看是同 target 下的哪条 `warn`，它直接说明原因：连不上、或连上但
   一个 page 都没有（此时工具留在 settings 指定的浏览器上，不会瞎连）。
2. **端点确实是个 DevTools 服务。** `curl http://127.0.0.1:<port>/json/version`
   返回 JSON，`product` 以 `Edg/` 开头（WebView2 就是 Edge）；`/json/list` 里至少
   有一个 `"type": "page"`。
3. **Playwright 能连上并且看得见那个页面。**
   `chromium.connectOverCDP("http://127.0.0.1:<port>")` → `contexts()[0].pages()` 非空。
   这正是 `@playwright/mcp --cdp-endpoint` 走的那条路。
4. **agent 驱动的是面板里那一页。** 让 agent 调一次 `browser_navigate`，然后看面板：
   该页面应以一个 tab 出现（tab 标题是页面的 `<title>`，取不到时退化成 host）。
5. **没有第二个浏览器冒出来。** 全过程机器上不应出现新的 `chrome.exe` / Chromium
   进程：受管 Chromium 这条路上不应该有人走。
6. **亲手试输入法。** 在面板的页面里点进一个输入框，用中文输入法打字：候选窗应出现在
   光标处，打字不应有延迟。这是这次重做要解决的第一件事，也是最容易被「看起来正常」
   骗过去的一件事。

第 3 步之前的决策链已经有测试覆盖（`crates/browser-service/src/lib.rs` 的
`the_panel_browser_is_the_browser_the_tools_drive`：面板端点发布后，provider 参数必须
是 `--cdp-endpoint <面板端点>` 且不带 `--user-data-dir`；
`an_unpublished_panel_leaves_the_settings_in_charge`：没发布时按 settings 走）。
第 1–6 步里真正需要人来判断的只有 4 和 6。
