# 内置浏览器子系统（Browser View Subsystem）

## 目标

右侧面板的「浏览器 tab」与 browser-use MCP 工具（`@playwright/mcp`）**共用同一个
浏览器**：

1. 点击 UI 中的 http(s) 链接 → 在右侧面板开一个 tab，显示该 URL 的**实时页面**。
2. agent 的 browser-use 工具控制的就是右侧这个浏览器；工具的导航/点击会实时
   反映在右侧 tab 里。
3. 用户可以直接在右侧 tab 里点击、输入、滚动（输入回传），等价于一个 Chrome 页面 tab。

## 架构

```
playwright-mcp (provider, MCP tools)  ──┐
                                        ├─ CDP ─→  受管 Chromium（Chrome for Testing）
Kodex CDP client (browser-service)   ──┘           （每会话一个，带 --remote-debugging-port）
        │
        ├─ Page.startScreencast → 帧事件 → tauri event → 右侧 tab（实时画面）
        └─ Input.dispatch*      ← 用户在右侧 tab 的鼠标/键盘
```

- **Attach 模式**（接管用户浏览器）：浏览器由用户启动（必须带
  `--remote-debugging-port`），`settings.endpoint` 就是 CDP 端点，两类连接都指向它。
- **Launch / Persistent 模式**：Kodex 自己启动受管 Chromium（带调试端口 + 独立
  profile），再给 provider 传 `--cdp-endpoint`，由 playwright-mcp attach 上来。
  这样「工具驱动的浏览器」与「右侧显示的浏览器」在进程层面就是同一个。

右侧 tab 与浏览器的 **page target 1:1**：

- 点链接 → `Target.createTarget(url)` → 新 target → 新 tab。
- 关 tab → `Target.closeTarget`。
- agent 打开的页面（targetCreated）也会出现在 tab 栏里。
- tab 被激活 → 只对那个 target `Page.startScreencast`（同时只播一路）。

## 事件契约（Rust → 前端，tauri event）

| 事件 | payload（snake_case 字段） |
|---|---|
| `browser_view:status` | `{ session_id, status: "connecting"\|"live"\|"closed"\|"failed", detail: string \| null }` |
| `browser_view:targets` | `{ session_id, targets: [{ target_id, url, title, type }] }`（type: "page"） |
| `browser_view:meta` | `{ session_id, target_id, url: string \| null, title: string \| null }`（增量） |
| `browser_view:frame` | `{ session_id, target_id, frame: <base64 jpeg>, seq: number }` |

## 命令契约（前端 → Rust，tauri command，全部 async，返回 `Result<_, String>`）

| 命令 | 参数（snake_case） | 说明 |
|---|---|---|
| `browser_view_attach` | `session_id` | 连接 CDP、开始发 targets/frame 事件。幂等。返回 status。 |
| `browser_view_detach` | `session_id` | 停止 screencast 并断开视图连接（浏览器和 provider 不动）。 |
| `browser_view_focus` | `session_id, target_id` | 切换正在直播的 target。 |
| `browser_view_open_page` | `session_id, url` | `Target.createTarget`，返回 `{ target_id }`。 |
| `browser_view_close_page` | `session_id, target_id` | `Target.closeTarget`。 |
| `browser_view_set_size` | `session_id, target_id, width, height` | `Emulation.setDeviceMetricsOverride` + 更新 screencast maxWidth/maxHeight（重启 screencast）。width/height 为逻辑像素。 |
| `browser_view_input` | `session_id, target_id, event` | event 为下述 InputEvent 的 tagged JSON。 |

```rust
// 前端 event 的形状（serde tag = "kind"）
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewInputEvent {
    Mouse { r#type: String, x: f64, y: f64, button: String, click_count: u32,
            delta_x: f64, delta_y: f64, modifiers: u32 },
    Key { r#type: String, key: String, code: String, text: String, modifiers: u32 },
    Text { text: String },
}
```

## R1：`crates/browser-service/src/cdp.rs` + `view.rs`

### cdp.rs — CDP over WebSocket 客户端

依赖：`tokio-tungstenite = "0.27"`（workspace 里已有该版本，加到
`browser-service/Cargo.toml`，无需 tls feature，只连本机 ws）。

```rust
pub struct CdpClient; // 持有 ws 读写任务
impl CdpClient {
    /// 连接 ws 端点。若给的是 http(s)://host:port，先 GET {base}/json/version
    /// 取 webSocketDebuggerUrl。若 ws url 不含 "/devtools/browser/"，同样
    /// 归一化到 http base 再发现（attach 模式的 settings.endpoint 形如
    /// "ws://127.0.0.1:9222/devtools"）。
    pub async fn connect(endpoint: &str) -> Result<Self, CdpError>;
    /// 发送命令并等待响应。`session` 为 Target.attachToTarget(flatten) 得到的
    /// sessionId（无则不带 sessionId 字段）。返回 result 字段；error 时 Err。
    pub async fn call(&self, method: &str, params: serde_json::Value,
                      session: Option<&str>) -> Result<serde_json::Value, CdpError>;
    /// 事件订阅（含 sessionId 标签）。
    pub fn events(&self) -> tokio::sync::broadcast::Receiver<CdpEvent>;
    pub async fn close(&self);
}
pub struct CdpEvent { pub method: String, pub params: serde_json::Value,
                      pub session_id: Option<String> }
```

实现要点：单写任务 + 按 id 的 oneshot map；读任务分发响应/事件；心跳不需要；
断线后 `call` 返回 `CdpError::Disconnected`。

测试（`#[cfg(test)]` 或 tests/）：起一个本地 `tokio_tungstenite::accept_async`
的假 CDP server（参考 `crates/dsh-bridge/tests/common/mod.rs` 的写法），覆盖：
命令往返（含 sessionId 透传）、事件广播、错误响应、http 端点发现（起一个只回
`/json/version` 的假 http server 可用 tokio TcpListener 手写最小 HTTP）。

### view.rs — 右侧浏览器视图服务

```rust
pub trait BrowserViewSink: Send + Sync + 'static {
    fn status(&self, status: ViewStatus, detail: Option<String>);
    fn targets(&self, targets: Vec<PageTarget>);
    fn meta(&self, target_id: &str, url: Option<String>, title: Option<String>);
    fn frame(&self, target_id: &str, jpeg_base64: &str, seq: u64);
}
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ViewStatus { Connecting, Live, Closed, Failed }
#[derive(Clone, Debug, serde::Serialize)]
pub struct PageTarget { pub target_id: String, pub url: String,
                        pub title: String, pub r#type: String }

pub struct BrowserView; // 每 session 一个
impl BrowserView {
    /// 连 CDP、Target.setDiscoverTargets、推送全量 targets，status → Live。
    pub async fn attach(endpoint: &str, sink: Arc<dyn BrowserViewSink>)
        -> Result<Arc<BrowserView>, CdpError>;
    pub async fn detach(&self);            // 停 screencast、断开；可再次 attach
    pub async fn focus(&self, target_id: &str) -> Result<(), CdpError>;
    pub async fn open_page(&self, url: &str) -> Result<String, CdpError>;
    pub async fn close_page(&self, target_id: &str) -> Result<(), CdpError>;
    /// 逻辑像素。Emulation.setDeviceMetricsOverride（width/height 取整），
    /// 并以新尺寸重启该 target 的 screencast（attach 模式下也做 override，
    /// 用户自己的浏览器窗口尺寸不受影响，只影响该 page 的布局视口）。
    pub async fn set_size(&self, target_id: &str, width: f64, height: f64)
        -> Result<(), CdpError>;
    pub async fn input(&self, target_id: &str, event: &ViewInputEvent)
        -> Result<(), CdpError>;
}
```

协议细节：

- target 发现：`Target.setDiscoverTargets { discover: true }`，维护
  `targetCreated/targetInfoChanged/targetDestroyed` 中 `type == "page"` 的集合，
  变化时 `sink.targets(...)`；`targetInfoChanged` 里 URL/title 变化也走
  `sink.meta(...)`。
- screencast：`Target.attachToTarget { targetId, flatten: true }` → sessionId →
  `Page.enable` → `Page.startScreencast { format: "jpeg", quality: 60,
  maxWidth, maxHeight }`；`Page.screencastFrame` → `sink.frame(...)` 后
  `Page.screencastFrameAck { sessionId: frame.sessionId }`。切换 target 时对旧
  target `Page.stopScreencast` 并 detach。
- 输入映射：Mouse → `Input.dispatchMouseEvent`（type 原样传
  mousePressed/mouseReleased/mouseMoved/mouseWheel，wheel 用 deltaX/deltaY）；
  Key → `Input.dispatchKeyEvent`（type 原样 keyDown/keyUp/char）；Text →
  `Input.insertText`。坐标为逻辑像素，直接作为 CDP 的 x/y（setDeviceMetricsOverride
  后 1:1）。
- `set_size` 需要重启 screencast 才能拿到新分辨率的帧。

测试：用假 CDP server 端到端跑 attach → targets 事件 → focus → screencast 帧 →
ack → input 命令断言 → open_page/close_page。断言帧经 sink 收到、ack 发出。

## R2：受管 Chromium 启动（`managed.rs` + provider/lib 接线）

新文件 `crates/browser-service/src/managed.rs`：

```rust
/// settings.executable_path 非空用之；否则用 node + playwright 解析：
/// `<node> -e "process.stdout.write(require('<package_root>/node_modules/playwright-core').chromium.executablePath())"`
/// （package_root 为 provider 安装根；失败则回退 require('playwright')）。
pub fn resolve_browser_executable(node: &Path, package_root: &Path,
                                  settings: &BrowserSettings) -> Result<PathBuf, String>;

pub struct ManagedBrowser { /* child, port, ws_endpoint, temp_profile: Option<PathBuf> */ }
impl ManagedBrowser {
    /// 启动 chrome：
    /// `<exe> --remote-debugging-port=<port> --user-data-dir=<dir>
    ///   --no-first-run --no-default-browser-check [--headless=new] about:blank`
    /// port 用 TcpListener bind "127.0.0.1:0" 取空闲端口。Launch 模式用
    /// data_root/tmp/browser/<uuid> 作 profile（release 时删）；Persistent 模式
    /// 用 provision::profile_dir(...)。等待 `http://127.0.0.1:<port>/json/version`
    /// 可达（轮询 ≤ 15s）后返回；ws_endpoint 取响应里的 webSocketDebuggerUrl。
    pub async fn launch(...) -> Result<ManagedBrowser, String>;
    pub fn ws_endpoint(&self) -> &str;
    pub async fn shutdown(self); // kill child + 清理临时 profile
}
```

接线（`provider.rs`、`lib.rs`）：

- `build_launch(..., cdp_endpoint: Option<&str>)`：`Some` 时（Launch/Persistent
  改为受管后也走这条路；Attach 用 settings.endpoint）追加
  `["--cdp-endpoint", endpoint]`，**不再**追加 `--isolated` / `--headless` /
  `--executable-path` / `--user-data-dir` / `--browser`（浏览器已由我们启动，
  这些参数与 attach 模式一致地省略）。更新 provider.rs 现有测试并补新断言。
- `BrowserFactory::acquire`：Launch/Persistent 模式先
  `resolve_browser_executable` + `ManagedBrowser::launch`，再
  `build_launch(..., Some(managed.ws_endpoint()))`；`BrowserResource` 增加
  `managed: Option<ManagedBrowser>` 与 `cdp_endpoint: Option<String>`（Attach
  模式 = settings.endpoint）；`release` 时 `managed.shutdown()`。
- `BrowserService`/`BrowserResource` 暴露 `pub fn cdp_endpoint(&self) -> Option<&str>`
  以及在 registry 上取活资源的访问器（如缺，给
  `session-resource` 的 `SessionResourceRegistry` 加 `pub fn with_resource<R>(
  &self, session_id, f: impl FnOnce(Option<&Resource>) -> R) -> R` 或同等只读
  访问，保持线程安全语义）。

测试：resolve 的 argv 单测（ScriptedRunner 风格照抄 installer.rs）、build_launch
的 cdp-endpoint 断言、managed launch 的参数组装（把 spawn 抽成可注入的 runner
以便单测）。

## R3：桌面命令层（apps/desktop/src-tauri）— 由主线完成

`commands/browser_view.rs`：实现上表命令。`AppState` 持有
`browser_views: Mutex<HashMap<String, Arc<BrowserView>>>`；`browser_view_attach`
从 `BrowserService` 取该 session 的 `cdp_endpoint`（无则先触发浏览器就绪：复用
`PanelBrowser`/registry 的懒启动路径，或返回 "browser not ready"），sink 实现为
tauri event emitter（`AppHandle::emit` 上表四个事件）。

## R4：前端（apps/desktop/ui）— 由主线完成

- `features/browser/BrowserLiveView.tsx`：帧渲染（`<img src="data:image/jpeg;base64,...">`）
  + 指针/滚轮/键盘捕获（focusable，keydown 用 key/code/text 映射）+ ResizeObserver
  → `browser_view_set_size`；挂载时 `browser_view_attach`，卸载时 `browser_view_detach`。
- ReviewPanel 的 web tab ↔ page targets（tab 标题用 title 或 host）；激活 tab →
  `browser_view_focus`；关 tab → `browser_view_close_page`。
- 链接点击（MarkdownBody 外链、SearchResults 外链）→ `browser_view_open_page`；
  浏览器能力不可用时回退 `openExternalUrl`。
- 事件监听进 `lib/events.ts`，命令包装进 `lib/tauri.ts`，类型进 `src/types/`。

## 兼容与回退

- browser-use 未安装/被禁用/远程工作区 → 链接回退系统浏览器打开（现状）。
- Attach 模式同样可投屏（只读其 `settings.endpoint`），但视口 override 只作用于
  该 page 的布局，不改用户浏览器窗口大小。
- 旧的 child-webview 方案（`commands/webview_tabs.rs`）废弃删除。
