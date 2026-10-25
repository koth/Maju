# 内置浏览器子系统（Browser View Subsystem）

## 目标

右侧面板的「浏览器 tab」是一个**真的浏览器页面**，不是一个网页截图：

1. 点击 UI 中的 http(s) 链接 → 在右侧面板开一个 tab，显示该 URL 的**实时页面**，
   滚动、输入法、粘贴都是系统自己的。
2. 应用启动**不会**弹出任何浏览器窗口。浏览器由第一个 tab 打开，由最后一个 tab
   关闭带它一起结束。
3. **agent 的 browser-use 工具控制的就是右侧这个浏览器**：工具的导航、点击、输入
   实时反映在右侧 tab 里，用户看到的就是 agent 在操作的页面。两个平台都成立，
   只是实现不同（Windows 走 CDP，macOS 走页面内 JavaScript，见下）。

## 当前实现：面板自己就是浏览器（每 tab 一个原生子 webview）

右侧面板**不再投屏，也不再浮一层独立窗口**。每个 tab 是应用主窗口的一个
**子 webview**，两个平台实现同一份契约：

- **Windows**：WebView2 子 webview（`Window::add_child`），用面板自己的 profile
  （`~/.kodex/browser/panel`）与 `--remote-debugging-port=<本次运行选定的端口>` 启动。
- **macOS**：WKWebView 子 webview（同一个 `Window::add_child`，wry 在 macOS 上把
  webview 挂成 content view 的 subview）。

两者共用同一套 `browser_panel_*` 命令与 `browser_panel:*` 事件、同一个
`webview_surface.rs`。页面是窗口内的 subview，不存在 z-order、置顶、全屏 Space、
窗口跟随、拖拽漂移这些问题 —— 这些曾经让「面板」在 macOS 上根本显示不出来。

```
Windows:  WebView2（面板的 tab：每 tab 一个 child webview，共用一份 profile/调试端口）
macOS:    WKWebView（面板的 tab：每 tab 一个 child webview，应用窗口的 subview）
    ↑ bounds / activate / hide
Tauri 命令 browser_panel_*  ←→  EmbeddedBrowser.tsx（工具栏：后退/前进/重载/地址栏/加载中）
    ↓ Windows: --remote-debugging-port → @playwright/mcp（provider 进程，CDP 驱动面板）
    ↓ macOS:   PanelDriver → engine.js 注入面板页面（原生工具,无 provider 进程）
agent 的 browser-use 工具（同一个 kodex_browser MCP server,同一批工具名）
```

### 为什么 macOS 不再是「一组盖在面板上的 Chromium 窗口」

上一版在 macOS 上让面板是一堆 `--app` 窗口，用 Accessibility `AXRaise` 把它们
压在应用窗口之上、跟随 slot 的几何、再用看门狗把漂移拉回来。它在真实机器上必然失
败，且失败方式正好是最坏的那种（`~/.kodex/logs/app.log`）：

```
面板浏览器需要辅助功能权限才能始终保持在应用窗口上方（…）
无法登记面板浏览器窗口: CDP command failed with error -32000: Browser window not found
panel browser process exited; the agent's tools fall back to their configured browser
```

- 辅助功能权限没给（或没生效）→ Chrome 窗口永远在应用窗口**后面**，用户看到的
  右侧 tab 就是一片空白：链接打开了、tab 也在、内容不显示。
- 启动即弹窗：为了在第一次点链接前不等待，`main.rs` 在 setup 里就预热，于是**一
  启动就有一个 Chromium 窗口**。
- 依赖「窗口 id / frame / minimized」这套 CDP + AX 的组合，浏览器版本一变就整串
  报错；全屏、Space、拖动标题栏又会引入新的几何抖动。

所以整套 `macos.rs`（进程 + CDP + 窗口几何 + 看门狗）、`macos_ax.rs`（AX 置顶）、
`core-foundation` 依赖、`chrome_height` 契约、面板窗口的 `Moved/Resized/Focused`
事件钩子全部删除，换成一行跨平台的 `add_child`。代价只有一条：WKWebView 没有
CDP，macOS 上「agent 驱动面板页面」做不到 —— 这条能力此前靠一个被挡在应用窗口
后面、用户根本看不见的窗口来假装成立。

### 面板的工具栏

`chrome_height`（macOS 原生标题栏高度）随浮窗方案一起删除：面板页面不再有自己
的窗口外壳，工具栏永远在面板里。工具栏上多了一个**加载中…** 状态，来自
`browser_panel:page`：页面在 webview 里加载时「洞」是空的，没有提示时看起来就像
面板坏了。

契约（实现见 `apps/desktop/src-tauri/src/browser_panel/`，共享宿主在 `mod.rs`）：

| 命令 | 参数 | 说明 |
|---|---|---|
| `browser_panel_state` | — | `{ tabs, active, endpoint }`；`endpoint` 就是 agent 该连的 CDP 地址（macOS 恒为 `null`）。 |
| `browser_panel_open` | `url` | 新建 tab，返回 `{ tab_id, url, title }`。Windows 上有 warm 页时**就地**变成 tab（agent 正连着的页不断线）。 |
| `browser_panel_close` | `tab_id` | 关 tab 并销毁 webview；profile 与登录态保留。 |
| `browser_panel_activate` | `tab_id` | 显示该 tab（连 webview 一起显示），隐藏其余。 |
| `browser_panel_hide` | — | 不关页面，只把 webview 藏起来（面板正在显示别的 tab）。 |
| `browser_panel_navigate` | `tab_id, url` | 裸域名按 `https://` 补全；只接受 http/https/about/file/data/blob。 |
| `browser_panel_reload` | `tab_id` | 重新加载当前 URL。 |
| `browser_panel_history` | `tab_id, direction` | `back` / `forward`。 |
| `browser_panel_bounds` | `x, y, width, height` | 面板上报「洞」的位置，逻辑像素（= CSS 像素）。 |
| `open_external_url` | `url, source` | 只承载面板渲染不了的协议（`mailto:` / `tel:` 等）。http(s) 在这里**被拒**并记 `warn`。 |

| 事件 | payload |
|---|---|
| `browser_panel:tabs` | `{ tabs, active }`：增删、切换、标题变化后的全量列表。 |
| `browser_panel:page` | `{ tab_id, url, event: "started" \| "finished" }`：工具栏的「加载中…」。 |
| `browser_panel:url` | `{ tab_id, url }`：跳转、重定向、页内点击。 |
| `browser_panel:new_window` | `{ tab_id, url }`：普通新窗口请求回退为右侧 tab；认证弹窗不经过这里。 |

原生页面画在应用自己的 DOM **之上**（它是窗口的子视图 / 子窗口），于是：

- 「洞」必须保持空：任何画在洞里的东西都会被页面盖住，加载状态只能放工具栏。
- 矩形为 0（面板收起 / `display: none`）时后端隐藏页面（webview hide），否则它会
  浮在别的内容上。唯一的短时例外是 agent 调用持有 paint lease：页面移到屏外但
  保持 compositor 可绘制，调用结束立即恢复隐藏，见下方 Windows CDP 说明。
- 面板重新出现时尺寸可能与消失前完全相同（没有任何元素尺寸变化），所以
  `EmbeddedBrowser` 还按 `layoutSignal` 重测一次；`ResizeObserver` 与 window resize
  覆盖其余情况。
- 面板切到非 web tab、或整体展开/收起（ReviewPanel 卸载）时，组件卸载 →
  `browser_panel_hide`。
- 应用窗口移动/缩放**不需要**后端介入：页面是窗口的子视图，跟着窗口走。
- 普通 `target="_blank"` 链接没有第二个窗口可去，`on_new_window` 把请求发成
  `browser_panel:new_window`，前端照常开 tab。带尺寸的 `window.open` 和 Google /
  Apple / Microsoft 认证域名则创建真正的独立 WebView 弹窗：OAuth 依赖
  `window.opener` 回传结果并自行关闭，降级成普通 tab 会让 X 等站点的登录一直等待。
  应用生命周期只归标签为 `main` 的主窗口：关闭认证弹窗不能触发 `shutdown_all`，
  否则项目与会话状态会在主窗口仍打开时被清空。
- 页面标题来自 `on_document_title_changed`（两个平台都有）。

### agent 控制接口：面板浏览器 = 工具驱动的浏览器

`kodex_browser` 这个 MCP server 对 agent 暴露同一批工具名
（`mcp__playwright-mcp__browser_navigate` 等），但两个平台的实现不同。

**Windows：CDP。** 工具那一侧不新增任何东西，只是**指向面板**：

1. 面板的 webview 用 `--remote-debugging-port=<端口>` 启动（端口在本次运行里固
   定），因此提供 `http://127.0.0.1:<端口>` 的 CDP 端点（Playwright 官方文档的
   WebView2 接法：`chromium.connectOverCDP(endpoint)` → `contexts()[0].pages()[0]`）。
2. 浏览器一起来，桌面端就把端点发布到 `app_core::panel_browser::publish`
   （`crates/browser-service/src/panel.rs` 的进程级槽位）—— 但要先**自检通过**。
3. `BrowserFactory::acquire`（`crates/browser-service/src/lib.rs`）**优先**用这个
   端点：有面板浏览器时，无论 settings 写的是 launch / persistent / attach，都不再
   启动受管 Chromium，直接把 `--cdp-endpoint <面板端点>` 交给 playwright-mcp。
   面板浏览器是「一个浏览器、一份可见的 tab 列表」，所以这时注册表按独占处理。
4. playwright-mcp 连接时 `Context._initializeBrowserContext` 会把已存在的 page
   target 收成 tab，所以工具操作的就是面板里那个页面。
5. 由此，**Windows 上预检不再要求本机装 Chromium**：`check_browser_with` 在有面板
   浏览器时直接 Ready。
6. **每次 provider 调用持有短时 paint lease。** WebView2 的 `IsVisible = false`
   不只是肉眼不可见，它会停止 compositor 产出帧；此时 DOM / AX 仍能应答，但
   `Page.captureScreenshot` 会一直等下一帧，最终撞上 Playwright 默认 5s action
   timeout。`BrowserMcpService` 在调用期间通过 `browser-service::panel_paint`
   持有 lease，面板把后台页移到屏外并保持可绘制（不覆盖 UI、不改变 viewport），最后
   一个 lease 释放后立即恢复 `webview.hide()`。引用计数覆盖 warm 页、隐藏面板和
   非活动 tab，也保证重叠调用不会提前把对方的页面隐藏；新建 tab 在发布前重新应用
   当前 lease。原生 placement 由独立互斥锁串行化，并在取锁后读取最新状态，旧的
   show/hide 计划不会覆盖新 lease。
7. **超时不会污染下一次调用。** provider 的迟到响应可能在下一次请求发送后才到；
   stdio MCP 客户端丢弃 id 小于当前请求的过期响应，而不是把它当成当前响应并报
   `response id does not match`。只有大于当前 id 的响应仍是协议错误。读取使用持久
   的 `Lines::next_line`（Tokio cancellation-safe）：deadline 前只收到半帧时，已读
   字节保留在 transport 内，下一次调用先拼完并丢弃旧帧，不会从半帧中间解析 JSON。
8. **截图引用使用持久路径。** provider 可能把截图写进系统临时目录
   `.playwright-mcp` 并把该路径打印给模型；`ScreenshotPipeline` 已保存字节后，
   `BrowserMcpService` 会把这段文本改成持久 `ScreenshotHandle.path`，避免临时文件
   被清理后消息里的图片失效。已经存在于历史消息中的临时引用由桌面端仅对
   `.playwright-mcp` 子目录开放 asset scope 兼容读取，不开放整个系统临时目录。
   Markdown 图片的本地路径还会在 remark 阶段从原始源码恢复：CommonMark 会把
   `Temp\.playwright-mcp` 中的 `\.` 当作转义并删掉反斜杠，只看解析后的 URL 无法
   找回正确文件。

**macOS：原生工具。** WKWebView 没有任何自动化协议（没有 CDP，也没有等价的东
西），provider 进程连不上面板 —— 它只能驱动一个用户看不见的后台 Chromium，那
正是「agent 没法控制右侧 tab」的来源。所以 macOS 上不跑 provider：

1. 桌面端在启动时发布一个 `PanelDriver`（`crates/browser-service/src/panel_driver.rs`
   的进程级槽位，实现在 `apps/desktop/src-tauri/src/browser_panel/driver.rs`）。
2. `browser_mcp` 发现驱动在场，就把 `tools/list` 换成本地静态目录
   （`crates/app-core/src/panel_browser_tools/mod.rs`），把每次 `tools/call` 派发成
   对面板 webview 的操作：导航当前 tab，或在当前页面里执行
   `crates/app-core/src/panel_browser_tools/engine.js`。
3. `engine.js` 是注入页面的引擎：产出带 `ref` 的可访问性快照（role/name/状态树，
   ref 在页面内单调递增、旧快照的 ref 必然失效而不是点错元素），并对 ref 指向的
   元素合成 pointer/mouse/key 事件与原生赋值（React 受控输入走 native setter）。
   页面脚本通过每个 webview 都自带的 Tauri IPC 桥回传结果
   （`browser_panel_eval_result`），所以 `eval` 能拿到页面的应答。
4. 由此，**macOS 上预检同样直接 Ready**（`check_browser_with(..., panel_native)`），
   不需要 Node、provider 包或 Chromium —— 什么进程都不启动。

macOS 上这条路的已知边界：合成事件的 `isTrusted` 为 false，个别站点（极少数登
录/验证码页）会拒绝；`browser_take_screenshot` 等依赖浏览器二进制的能力不在原生
目录里（WKWebView 没有暴露截图 API 给 wry）。需要这些时 agent 会明确看到工具不
存在，而不是调用后神秘失败。

**超时与降级纪律。** 这条链路上的失败分两类，混为一谈就是「渲染超时 / eval 无响
应」的来源：

- **页面自己报告的错误**（脚本运行了，页面说「ref 已过期」）是定论，原样上抛，
  绝不重试。
- **传输层失败**（`[transport]` 前缀，约定在
  `crates/browser-service/src/panel_driver.rs`）：脚本被在途的导航丢弃、应答随旧
  文档销毁、或页面忙到超时——结果*未知*。驱动层在页面 `started` 事件到达的瞬间
  快速失败，而不是等满超时；派发层据此区别对待：只读命令（快照/读文本）落定后
  重试一次，动作命令（点击/输入）不重试（可能已生效，重试就是点两次），降级为
  「动作已派发但未经确认」+ 落定后的最新快照。
- 每条命令有各自的求值上限（引擎命令 8s，`browser_evaluate` 20s），每次工具调用
  有 38s 总预算——低于 dsh 侧 45s 的工具超时，保证 agent 收到的是带上下文的错
  误，而不是传输层的截断。
- 「tab 正在加载」的记录带时间戳：WKWebView 不保证每次 `started` 都有配对的
  `finished`（被取消/失败的导航常常没有），超过 30s 的记录按过期处理，一次丢失
  的回调不会让之后每个动作都白等导航超时。
- 快照遍历有深度（40 层）与访问总量（6000 节点）上界，巨型页面上的快照耗时与页
  面大小脱钩。

Windows 上面板自己做两件事让 CDP 链路在任何时刻都成立：

- **保活**：始终留一个隐藏的空白页（"warm" 页），它不是一个 tab。这样即使一个
  tab 都没开，浏览器（以及 CDP 端点）也是活的；关掉最后一个 tab 时立刻再补一个。
  用户在设置里打开 browser-use 时会预热（`settings.rs`），**应用启动时不预热**
  ——启动时该出现的只有 Maju 自己。
- **上位**：空白页里第一次出现真实 URL（agent 导航，或用户点链接）时，它就地变成
  一个 tab 并广播 `browser_panel:tabs` —— agent 打开的页面因此出现在面板里，用户
  能看到 agent 在看什么。前端发现新的 tab id 时自动激活它并展开右栏一次；已有 tab
  的标题等后续事件不会反复抢回焦点，用户手动切回 Review / Diff 后不受打扰。

macOS 上这两件事都不做：没有 CDP 就没有「保活」的理由（面板的浏览器由 tab 的生死
决定），「上位」则由原生工具的 tab 操作直接完成。

### 端点只有在连得上时才发布

发布端点是**有代价的**：`acquire` 一旦看到面板端点，就再也不会回退到 settings 里
那个浏览器。所以「发布了一个没人应答的地址」不等于「工具还能用别的浏览器」，而是
等于「工具全挂」。因此发布前先自检，见 `app_core::panel_browser::probe`：

- 用 `crates/browser-service/src/cdp.rs` 的 `CdpClient` 连上去，`Browser.getVersion`
  拿到产品串（WebView2 是 `Edg/...`），`Target.getTargets` 数出一共有几个 `page`
  target。
- 连上去要同时满足两件事才会发布：**DevTools 有应答**，且**至少有一个 `page`
  target**。第二件同样必要 —— WebView2 没有 `Target.createTarget`，把一个没有页面
  的端点交给工具，等于让工具连上一个开不了新页的浏览器。
- 连不上就**不发布**，并按 200ms 一次、最多 5s 重试 —— `add_child` 返回时 webview
  已存在，但背后的浏览器进程还要一点时间才开监听。
- 自检发生在每个**打开页面**和每次**把某个 tab 显示出来**时。已经发布过就立刻返
  回（一次字符串比较），没发布过就再试一次。
- 始终不满足 → 不发布，工具按 settings 走原来的路（仍然可用，只是不在面板里），
  并记一条 `warn` 说明是哪一件不满足。
- macOS 根本没有端口，因此这条路径一次都不走，`browser_panel_state().endpoint`
  恒为 `null`——原生工具不需要它。

`~/.kodex/logs/app.log` 里对应的几行（`target: "browser_panel"`）：

| 日志 | 含义 |
|---|---|
| `panel browser is warm` | 预热页建好了，带 `endpoint`（仅 Windows）。 |
| `panel browser answers DevTools; the agent's tools drive it` | 自检通过并已发布，带 `product` / `pages`。 |
| `panel browser answers DevTools but lists no page; ...` | 连得上但没有可驱动的 page，未发布。 |
| `panel browser never answered DevTools; ...` | 端点不可达，未发布，工具仍按 settings 走。 |

已知限制：

- Windows：`Target.createTarget` 不被 WebView2 支持，所以工具自己「新建标签页」会
  失败；让它用 `browser_navigate` 走当前页面即可（页内 `target="_blank"` 链接不受影
  响，见上）。
- macOS：合成事件 `isTrusted=false`，极个别站点会拒绝；`browser_take_screenshot`
  等依赖浏览器二进制的能力不在原生工具目录里（工具直接不出现，而不是调用后失
  败）。
- 端点由面板的浏览器进程提供，进程随应用退出而消失；面板保证「至少有一个 page」，
  所以只要应用在跑，Windows 上工具就有页面可操作。

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

### 平台差异：工具列表跟着能力走，不跟着代码走

两台机器能做的事不一样，所以工具面也不一样：

| | Windows（WebView2，有 CDP） | macOS（WKWebView，无 CDP） |
|---|---|---|
| 谁发布 | `panel::slot()` 的 DevTools 端点 | `panel_driver::slot()` 的原生驱动 |
| agent 看到的页面工具 | `browser_observe` / `browser_act` / `browser_viewport` + provider 工具 | provider 名字的原生工具（`browser_snapshot` / `browser_click` / …），由 `engine.js` 在页面里跑 |
| 我们的观测工具 | 列出 | **不列出** |

判据是 `panel_driver::get().is_none()`：驱动只在「面板 webview 没有 CDP」的平台发布
（见 `browser-service/src/panel_driver.rs` 的模块注释），所以它为 `Some` 时，
`browser_observe` 永远拿不到端点——列出来只会让模型反复调用一个恒假的工具。

不列之外还有两道兜底：会话拿着旧列表打过来时，返回的是「这里没有 DevTools 端点，
改用 browser_snapshot」而不是「还没准备好」（后者会让模型重试）；`initialize` 的引导
文本也按平台换一份——macOS 上不塞 observe/act 那套讲编号和 diff 的文字，改成讲
`browser_snapshot` → `ref` 的循环，安全与确认两篇照旧。

## 架构

```
apps/desktop/ui  EmbeddedBrowser.tsx / usePanelBrowserTabs.ts / browserTabs.ts
        ↕ tauri command + event（browser_panel_*）
apps/desktop/src-tauri/src/browser_panel/
        mod.rs            ← 共享宿主：tab 列表、slot、命令分发、Windows 端点发布
        │                    （先过自检）、promote 契约、页面加载事件、eval 应答注册表
        webview_surface.rs ← 两个平台共用的 surface：Window::add_child 创建、
        │                    放置、三个回调（on_page_load / on_document_title_changed
        │                    / on_new_window）、browser_args（仅 Windows）
        driver.rs         ← TauriPanelDriver：导航面板 tab、await 页面加载完成、
        │                    eval 包装（表达式 → __TAURI_INTERNALS__.invoke 应答）
        ↓ Windows: publish(CDP endpoint)   macOS: publish(PanelDriver)
crates/browser-service/src/panel.rs             ← 进程级 CDP 端点槽位（Windows）
crates/browser-service/src/panel_driver.rs      ← 进程级 PanelDriver 槽位（macOS）
crates/app-core/src/panel_browser.rs            ← 应用侧入口：槽位 + probe 自检（Windows）
crates/app-core/src/panel_browser_tools/        ← 原生工具目录 + 派发 + engine.js（macOS）
crates/browser-service/src/lib.rs               ← BrowserFactory::acquire：面板端点优先
        ↓ Windows 才有：
@playwright/mcp --cdp-endpoint http://127.0.0.1:<面板端口>
```

各模块的职责边界：

- `crates/browser-service/src/panel.rs`：只存一个 `Arc<RwLock<Option<String>>>`。
  谁发布、谁读，都不认识 Tauri。
- `crates/browser-service/src/panel_driver.rs`：同一个形状，存的是面板驱动的
  trait object。macOS 上它是 agent 工具到达面板的唯一通道。
- `crates/browser-service/src/lib.rs`：`acquire` 的优先级与独占判断在这里，是
  「面板浏览器 = 工具驱动的浏览器」在 provider 路径上的落点。
- `crates/app-core/src/panel_browser.rs`：应用侧入口（`publish` / `clear` /
  `endpoint` / `warm_required`）+ 发布前的 `probe`。桌面壳是唯一的发布者：它拥有
  webview，也就只有它知道面板浏览器在不在。
- `crates/app-core/src/panel_browser_tools/mod.rs`：原生工具的静态目录（与
  `@playwright/mcp` 兼容的工具名）与每次调用的派发；会话隔离沿用
  `browser_mcp` 的 token 机制，工具调用本身跨会话串行（面板只有一个）。
- `crates/app-core/src/panel_browser_tools/engine.js`：注入面板页面的引擎——可
  访问性快照（带单调递增 ref）与合成输入。由 vitest 在 jsdom 里直接测试
  （`apps/desktop/ui/src/features/browser/panelEngine.test.ts`）。
- `crates/app-core/src/browser_preflight.rs`：有面板端点（Windows）或原生驱动
  （macOS）时短路掉全部安装检查。
- `apps/desktop/src-tauri/src/browser_panel/mod.rs`：tab 与 slot 状态的唯一真相来
  源，命令分发、端点发布、promote 契约；平台差异只剩 `#[cfg(windows)]` 的
  DevTools 端口与 warm 页。
- `apps/desktop/src-tauri/src/browser_panel/webview_surface.rs`：面板 webview 的
  创建与放置、三个回调；`browser_args` 只在 Windows 编译（wry 会整体替换默认参数，
  故保持精确串）。
- `apps/desktop/src-tauri/src/browser_panel/driver.rs`：`PanelDriver` 的唯一实现：
  复用面板自己的命令开/关/切 tab，用广播的页面加载事件等待导航完成，用
  `browser_panel_eval_result` 配对页面脚本的应答。
- `apps/desktop/ui/src/features/browser/usePanelBrowserTabs.ts`：把 `browser_panel:*`
  事件收敛成面板的 tab 列表状态。
- `apps/desktop/ui/src/features/browser/browserTabs.ts`：链接分发（面板 or 系统），
  由 `MarkdownBody` / `SearchResults` 调用。

## 历史：为什么不是投屏，也不是浮窗

**第一代：投屏。** 面板里的 `<img>` 画受管 Chromium 的 `Page.startScreencast` 帧，
用户的鼠标/键盘/输入法经 CDP `Input.dispatch*` 回灌。三条绕不过去的毛病：打字要等
图片、输入法没有落点（候选窗只能挂在额外造的隐藏输入框上）、两次打开很慢。

**第二代：浮窗（仅 macOS）。** 面板是一堆 `--app` Chromium 窗口，靠 AX 置顶和几何
跟随「嵌」在面板里。除了本文开头那三条日志里的失败，它还要求用户授予辅助功能权限
才能用，并让「第一次点链接」在启动时就变成「一启动就弹窗」。

**第三代（现在）：子 webview + 双路驱动。** wry 的 `Window::add_child` 在 Windows 和
macOS 上都成立（分别是 WebView2 子窗口和 NSView subview）。同一份代码、同一套命令、
同一个没有 `chrome_height` 的工具栏，没有置顶、没有几何跟随、没有看门狗、没有第二
个浏览器进程浮在应用外面。agent 驱动面板在 Windows 上走 CDP，在 macOS 上走注入页面
的 `engine.js` —— 因为 WKWebView 什么自动化协议都没有，而「工具驱动一个用户看不见
的浏览器」等于这条功能不存在。

第一代删掉的东西（`browser-service/src/view.rs`、`app-core/src/browser_view.rs`、
`app-core/src/browser_panel.rs`、`commands/browser_view.rs`、`BrowserLiveView.tsx`、
`BrowserPanel.tsx`，以及 `browser_view:*` 事件与 `browser_view_*` 命令、
`tests/live_view_smoke.rs`）不再保留；第二代又删掉了 `macos.rs`、`macos_ax.rs`、
`windows_surface.rs`、`chrome_height` 契约与面板的窗口事件钩子。

## 兼容与回退

- **面板浏览器没起来**（WebView2 不可用、端口不可达、自检没过）→ 端点不发布，
  `acquire` 按 settings 走原来的路：launch / persistent 启动受管 Chromium，attach
  连用户自己的端点。工具仍然可用，只是页面不在面板里；日志里有上面那张表对应的
  一行。
- **面板本身打不开页面** → `browser_panel_open` 直接报错，`browserTabs.ts` 用
  `console.error` 记下并**不**退回系统浏览器：与面板并排弹出的第二个浏览器窗口正
  是面板要替代的东西，且会掩盖失败。仅 `mailto:`/`tel:` 等面板无法承载的协议交给
  系统处理（`open_external_url`）。
- 关掉面板里最后一个浏览器 tab **不能连带影响 agent**：Windows 上 WebView2 在最后
  一个页面关闭时会退出进程，而这个浏览器同时是 agent 工具在用的那一个；它一死，
  之后每次点击都只剩 `connection refused`。所以 `browser_panel_close` 关掉最后一个
  tab 后立刻补一个空白页（空白页不占 tab）。macOS 上没有这个问题：面板的浏览器只
  属于面板，随最后一个 webview 一起结束。
- 面板浏览器是**共享资源**：同一时刻只有一个会话能驱动它（Windows 注册表按独占处
  理；macOS 上工具调用跨会话串行，面板本身是全应用共享的）。
- **受管 Chromium（工具在 provider 路径上时的浏览器）也有自己的事故史**，两件事
  要成立：
  - Kodex 崩溃或被强杀时，那个浏览器会被重新挂到 init 上并**继续占着配置档**。
    `browser_service::orphan` 的清扫按 `KODEX_BROWSER_OWNER` 标记回收它 ——
    受管 Chromium 现在也带这个标记（以前只有 node provider 带，所以「崩溃一次、
    之后每次工具调用都失败」是常客）。
  - 配置档被**活着的**别的浏览器占着时，启动会在开浏览器之前就拒绝，并报出占用者的
    pid 与配置档路径。Chromium 对同一个 `--user-data-dir` 的第二次启动，是把参数
    交给已有实例后自己退出；不提前拒绝的话，只会在 15 秒后得到一句没有原因的
    「没有开放 CDP 端点」。
  macOS 走原生工具后不再启动受管 Chromium，这两条只为 Windows 的 provider 回退路
  径保留。
- 面板工具栏不再有「在系统浏览器中打开」按钮：它是应用内最后一条通向 Chrome 的
  路径，与「链接只在右侧面板渲染」相冲突。
- 事件监听进 `lib/events.ts`，命令包装进 `lib/tauri.ts`，类型进 `src/types/`。

## 装完怎么验证

面板浏览器的核心主张是「**点链接，右侧 tab 里就是一个能用的网页；agent 的工具操
作的就是这个网页**」。按顺序做完就能判定：

1. **启动不弹窗。** 打开 Maju，机器上不应多出任何浏览器进程/窗口。
2. **链接能出内容。** 在会话里点一个 http(s) 链接：右侧出现一个 tab，工具栏是后
   退/前进/重载/地址栏，页面在「洞」里出现；慢页面时工具栏出现「加载中…」。
3. **页面是活的。** 在页面里滚轮滚动、用中文输入法打字、粘贴：都是页面自己的响应，
   没有一帧一帧的画面感（这一条正是投屏方案过不去的）。
4. **标签之间能切。** 再点一个链接得到第二个 tab，来回切换，各自保留自己的页面。
5. **日志干净。** `~/.kodex/logs/app.log` 搜 `browser_panel`：macOS 上应当**没有**
   任何窗口/AX/置顶相关的报错；Windows 上能看到 `panel browser answers DevTools;
   the agent's tools drive it`（带 `product=Edg/...` 与 `pages=1`），且
   `curl http://127.0.0.1:<port>/json/list` 里至少有一个 `"type": "page"`。
6. **agent 驱动的是面板里那一页。** 让 agent 调一次 `browser_navigate`：右侧 tab 应
   当着你的面跳转过去（macOS 上没有新的浏览器进程出现；Windows 上工具报
   `product=Edg/...`）。再让它 `browser_snapshot` 后 `browser_click` 一个链接，页
   面应真的跳走。

第 6 步之前的决策链有测试覆盖（`crates/browser-service/src/lib.rs` 的
`the_panel_browser_is_the_browser_the_tools_drive`：面板端点发布后，provider 参数
必须是 `--cdp-endpoint <面板端点>` 且不带 `--user-data-dir`；
`an_unpublished_panel_leaves_the_settings_in_charge`：没发布时按 settings 走；
`crates/app-core/src/panel_browser_tools/mod.rs` 的派发测试与
`apps/desktop/ui/src/features/browser/panelEngine.test.ts` 的引擎测试）。第 1–6 步
里真正需要人判断的只有 3。
