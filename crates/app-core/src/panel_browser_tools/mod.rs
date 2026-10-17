//! 原生浏览器工具:agent 的浏览器工具直接驱动右侧面板的页面。
//!
//! Windows 上面板 webview 自带 CDP,工具走 `@playwright/mcp` provider 驱动它;
//! macOS 的 WKWebView 没有任何自动化协议,provider 只能驱动一个用户看不见的
//! 后台 Chromium —— 「面板里那个 tab 就是 agent 在操作的页面」这条契约就断
//! 了。这里的实现把契约接上:工具名与 `@playwright/mcp` 保持兼容,但每个
//! 工具都落到 [`browser_service::panel_driver::PanelDriver`] 上 —— 导航面板
//! 的 tab、在面板页面里跑 [`engine.js`]。
//!
//! 外部注入的页面脚本只做两件事:产出带 ref 的可访问性快照,以及对 ref 指向
//! 的元素合成输入事件。快照的 ref 只在最近一次快照内有效,与 playwright
//! 的快照用法一致。

use std::sync::OnceLock;
use std::time::Duration;

use browser_service::catalog::ExposedTool;
use browser_service::panel_driver::{PanelDriver, is_transport_error};
use serde_json::{Value, json};

use crate::browser_mcp::{BrowserCallError, BrowserMcpService, BrowserToolResult};

/// 注入页面的引擎:每次求值都带着它,已安装时它自己立刻返回,所以这份成本
/// 是一次空转;ref 表装在页面里,因此 snapshot 之后 click 还能找到元素。
const ENGINE: &str = include_str!("engine.js");

/// 工具名保持与 provider 完全一致:下游(dsh 的工具命名、agent 的使用习惯)
/// 不需要知道实现换了。
const NATIVE_PREFIX: &str = "mcp__playwright-mcp__";

/// 结果里快照的长度上限:再长的树也不值得占满模型的上下文。
const MAX_SNAPSHOT_CHARS: usize = 16_000;
/// `browser_evaluate` 返回值的长度上限。
const MAX_EVAL_CHARS: usize = 20_000;
/// 一次引擎命令(快照/点击/输入/读文本)的应答上限。这些命令在页面上是毫秒
/// 级;等不到这个时间的应答只意味着页面正忙或正在跳转,没有理由等得更久。
const EVAL_OP_TIMEOUT: Duration = Duration::from_secs(8);
/// `browser_evaluate` 跑的是 agent 自己的代码,合法地可以更久。
const EVAL_USER_TIMEOUT: Duration = Duration::from_secs(20);
/// `browser_wait_for` 的最长等待。必须低于 dsh 侧 45s 的工具超时。测试里
/// 走同一条超时路径,只是不等满长度。
#[cfg(not(test))]
const WAIT_DEADLINE: Duration = Duration::from_secs(30);
#[cfg(test)]
const WAIT_DEADLINE: Duration = Duration::from_secs(2);
const WAIT_POLL: Duration = Duration::from_millis(250);
/// 一次工具调用的总预算。低于 dsh 侧 45s 的工具超时,保证 agent 收到的是
/// 我们带上下文的总超时错误,而不是传输层一句话的截断。
const CALL_DEADLINE: Duration = Duration::from_secs(38);

/// The native tool surface, in the shape the shared catalog slot holds.
fn native_catalog() -> &'static [ExposedTool] {
    static TOOLS: OnceLock<Vec<ExposedTool>> = OnceLock::new();
    TOOLS.get_or_init(|| {
        use browser_service::catalog::ToolEffect::{Read, Write};
        let mut tools = Vec::new();
        let mut tool = |name: &str, description: &str, schema: Value, effect| {
            tools.push(ExposedTool {
                exposed_name: format!("{NATIVE_PREFIX}{name}"),
                provider_name: name.to_string(),
                description: description.to_string(),
                input_schema: schema,
                effect,
            });
        };
        let target = |extra: &[(&str, Value)]| {
            let mut properties = serde_json::Map::new();
            properties.insert(
                "element".to_string(),
                json!({"type": "string", "description": "用一句话描述目标元素,便于在日志里辨认"}),
            );
            properties.insert(
                "ref".to_string(),
                json!({"type": "string", "description": "目标元素在最近一次 browser_snapshot 里的 ref(如 e12)"}),
            );
            for (key, value) in extra {
                properties.insert((*key).to_string(), value.clone());
            }
            json!({"type": "object", "properties": properties, "required": ["element", "ref"]})
        };
        tool(
            "browser_navigate",
            "把右侧面板里用户看得见的那个浏览器 tab 导航到一个 URL",
            json!({
                "type": "object",
                "properties": {"url": {"type": "string", "description": "要打开的 URL"}},
                "required": ["url"],
            }),
            Write,
        );
        tool(
            "browser_navigate_back",
            "面板浏览器后退一页",
            json!({"type": "object", "properties": {}}),
            Write,
        );
        tool(
            "browser_snapshot",
            "获取面板当前页面的可访问性快照:一棵带 role/name 的树,可操作元素带 ref。后续 click/type 等操作都引用这里的 ref",
            json!({"type": "object", "properties": {}}),
            Read,
        );
        tool("browser_click", "点击面板页面里的一个元素", target(&[]), Write);
        tool(
            "browser_type",
            "在面板页面里的输入框中填写文本(替换已有内容)",
            target(&[
                ("text", json!({"type": "string", "description": "要输入的文本"})),
                ("submit", json!({"type": "boolean", "description": "输入后是否按回车提交"})),
            ]),
            Write,
        );
        tool(
            "browser_press_key",
            "在面板页面里按一个键(如 Enter、Tab、Escape、ArrowDown,或单个字符)",
            json!({
                "type": "object",
                "properties": {"key": {"type": "string"}},
                "required": ["key"],
            }),
            Write,
        );
        tool("browser_hover", "把鼠标悬停在面板页面里的一个元素上", target(&[]), Write);
        tool(
            "browser_select_option",
            "在面板页面里的下拉框中选择选项",
            target(&[(
                "values",
                json!({"type": "array", "items": {"type": "string"}, "description": "要选中的选项值或文本"}),
            )]),
            Write,
        );
        tool(
            "browser_wait_for",
            "等待面板页面里出现(或消失)某段文本,或直接等待若干秒",
            json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "等待出现的文本"},
                    "textGone": {"type": "string", "description": "等待消失的文本"},
                    "time": {"type": "number", "description": "直接等待的秒数"},
                },
            }),
            Read,
        );
        tool(
            "browser_evaluate",
            "在面板页面里执行一段 JavaScript 并返回结果(JSON 序列化)",
            json!({
                "type": "object",
                "properties": {"function": {"type": "string", "description": "表达式或函数源码"}},
                "required": ["function"],
            }),
            Write,
        );
        tool(
            "browser_tabs",
            "列出、新建、切换或关闭面板浏览器的标签页",
            json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["list", "new", "close", "select"]},
                    "index": {"type": "number", "description": "close/select 时的标签序号(来自 list)"},
                },
                "required": ["action"],
            }),
            Write,
        );
        tool(
            "browser_close",
            "关闭面板浏览器当前的标签页",
            json!({"type": "object", "properties": {}}),
            Write,
        );
        tools
    })
}

/// Install the native catalog into the shared slot, once.
///
/// Unlike the provider catalog this list is static, so there is nothing to
/// discover and nothing that can fail; the return shape matches the provider
/// path's `ensure_catalog` so the server cannot tell the difference.
pub async fn ensure_catalog(service: &BrowserMcpService) -> Result<usize, BrowserCallError> {
    service.install_native_catalog(native_catalog().to_vec());
    Ok(native_catalog().len())
}

/// Whether this exposed tool name belongs to the native surface.
fn is_native_tool(exposed_name: &str) -> bool {
    native_catalog()
        .iter()
        .any(|tool| tool.exposed_name == exposed_name)
}

/// Route one tool call to the panel's browser.
pub async fn call(
    service: &BrowserMcpService,
    driver: &std::sync::Arc<dyn PanelDriver>,
    // The caller has already validated the token against its session; the
    // panel is one browser for the whole app, so nothing here re-reads it.
    _token: &str,
    exposed_name: &str,
    arguments: Value,
) -> Result<BrowserToolResult, BrowserCallError> {
    if !is_native_tool(exposed_name) {
        return Err(BrowserCallError::UnknownTool {
            name: exposed_name.to_string(),
        });
    }
    // The catalog is static, but the server asks the provider path to prime
    // before every call; make sure the native list is installed either way.
    let _ = ensure_catalog(service).await;
    // 面板只有一个:任何两个会话的动作落到同一批 tab 上。把调用串行化,
    // 让并发会话彼此看到的是完整动作,而不是互相踩掉的半次点击。
    static CALLS: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    let _guard = CALLS.get_or_init(|| tokio::sync::Mutex::new(())).lock().await;

    let leaf = exposed_name
        .strip_prefix(NATIVE_PREFIX)
        .unwrap_or(exposed_name)
        .to_string();
    // 总预算兜底:一次调用是导航等待、页面求值、快照的叠加,任何一段遇上
    // 慢页面都可能拉长;在这里封顶,agent 收到的才是我们的错误,而不是
    // harness 45s 的传输层截断。
    match tokio::time::timeout(CALL_DEADLINE, dispatch(driver.as_ref(), &leaf, arguments)).await {
        Ok(Ok(text)) => Ok(BrowserToolResult {
            text,
            screenshots: Vec::new(),
            delivery: None,
            is_error: false,
        }),
        Ok(Err(message)) => Err(BrowserCallError::Provider {
            tool: leaf,
            detail: message,
        }),
        Err(_) => Err(BrowserCallError::Timeout { tool: leaf }),
    }
}

// ---- 派发 ------------------------------------------------------------------

async fn dispatch(driver: &dyn PanelDriver, tool: &str, args: Value) -> Result<String, String> {
    match tool {
        "browser_navigate" => {
            let url = required_str(&args, "url")?;
            let tab = current_or_open(driver, Some(&url)).await?;
            driver.wait_settled(&tab).await?;
            let snap = snapshot_or_note(driver, &tab).await;
            Ok(format!("已导航到 {url}\n\n{snap}"))
        }
        "browser_navigate_back" => {
            let tab = current_tab(driver).await?;
            // 后退本身就是一次跳转:它的应答极容易随旧文档一起丢失。传输层
            // 失败不等于没有后退,落定后看页面即可。
            let back = exec(
                driver,
                &tab,
                &json!({"cmd": "evaluate", "source": "history.back()"}),
                EVAL_OP_TIMEOUT,
            )
            .await;
            if let Err(error) = back {
                if !is_transport_error(&error) {
                    return Err(error);
                }
            }
            driver.wait_settled(&tab).await?;
            let snap = snapshot_or_note(driver, &tab).await;
            Ok(format!("已后退\n\n{snap}"))
        }
        "browser_snapshot" => {
            let tab = current_tab(driver).await?;
            snapshot_of(driver, &tab).await
        }
        "browser_click" => {
            let tab = current_tab(driver).await?;
            let outcome = exec_action(
                driver,
                &tab,
                &json!({"cmd": "click", "ref": required_str(&args, "ref")?}),
            )
            .await?;
            driver.wait_settled(&tab).await?;
            let snap = snapshot_or_note(driver, &tab).await;
            Ok(match outcome {
                Some(outcome) => format!("已点击 {}\n\n{snap}", describe(&outcome, "clicked")),
                None => action_uncertain("点击", &snap),
            })
        }
        "browser_type" => {
            let tab = current_tab(driver).await?;
            let outcome = exec_action(
                driver,
                &tab,
                &json!({
                    "cmd": "type",
                    "ref": required_str(&args, "ref")?,
                    "text": args.get("text").and_then(Value::as_str).unwrap_or_default(),
                    "submit": args.get("submit").and_then(Value::as_bool).unwrap_or(false),
                }),
            )
            .await?;
            driver.wait_settled(&tab).await?;
            let snap = snapshot_or_note(driver, &tab).await;
            Ok(match outcome {
                Some(outcome) => format!("已输入 {}\n\n{snap}", describe(&outcome, "into")),
                None => action_uncertain("输入", &snap),
            })
        }
        "browser_press_key" => {
            let tab = current_tab(driver).await?;
            let key = required_str(&args, "key")?;
            let outcome = exec_action(driver, &tab, &json!({"cmd": "press", "key": key})).await?;
            driver.wait_settled(&tab).await?;
            let snap = snapshot_or_note(driver, &tab).await;
            Ok(match outcome {
                Some(_) => format!("已按下 {key}\n\n{snap}"),
                None => action_uncertain(&format!("按下 {key}"), &snap),
            })
        }
        "browser_hover" => {
            let tab = current_tab(driver).await?;
            let outcome = exec_action(
                driver,
                &tab,
                &json!({"cmd": "hover", "ref": required_str(&args, "ref")?}),
            )
            .await?;
            let snap = snapshot_or_note(driver, &tab).await;
            Ok(match outcome {
                Some(outcome) => format!("已悬停 {}\n\n{snap}", describe(&outcome, "hovered")),
                None => action_uncertain("悬停", &snap),
            })
        }
        "browser_select_option" => {
            let tab = current_tab(driver).await?;
            let values: Vec<String> = args
                .get("values")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let outcome = exec_action(
                driver,
                &tab,
                &json!({"cmd": "select", "ref": required_str(&args, "ref")?, "values": values}),
            )
            .await?;
            let snap = snapshot_or_note(driver, &tab).await;
            Ok(match outcome {
                Some(outcome) => format!("已选择 {}\n\n{snap}", describe(&outcome, "selected")),
                None => action_uncertain("选择", &snap),
            })
        }
        "browser_wait_for" => {
            let tab = current_tab(driver).await?;
            wait_for(driver, &tab, &args).await?;
            let snap = snapshot_or_note(driver, &tab).await;
            Ok(format!("等待结束\n\n{snap}"))
        }
        "browser_evaluate" => {
            let tab = current_tab(driver).await?;
            let outcome = exec(
                driver,
                &tab,
                &json!({"cmd": "evaluate", "source": required_str(&args, "function")?}),
                EVAL_USER_TIMEOUT,
            )
            .await?;
            let result = outcome
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or("null");
            let capped = if result.len() > MAX_EVAL_CHARS {
                format!("{}…(已截断)", &result[..MAX_EVAL_CHARS])
            } else {
                result.to_string()
            };
            Ok(format!("### 结果\n{capped}"))
        }
        "browser_tabs" => tabs_action(driver, &args).await,
        "browser_close" => {
            let tab = current_tab(driver).await?;
            driver.close_tab(&tab).await?;
            Ok("已关闭当前标签页".to_string())
        }
        other => Err(format!("未实现的工具:{other}")),
    }
}

// ---- 页面访问 ---------------------------------------------------------------

/// The tab the tools act on, or a clear error when there is no page at all.
async fn current_tab(driver: &dyn PanelDriver) -> Result<String, String> {
    driver
        .active_tab()
        .await
        .ok_or_else(|| "面板浏览器还没有打开任何页面,请先调用 browser_navigate".to_string())
}

/// The current tab, opening one when the panel has none yet.
async fn current_or_open(driver: &dyn PanelDriver, url: Option<&str>) -> Result<String, String> {
    if let Some(tab) = driver.active_tab().await {
        if let Some(url) = url {
            driver.navigate(&tab, url).await?;
        }
        return Ok(tab);
    }
    driver
        .open_tab(url.unwrap_or("about:blank"))
        .await
        .map_err(|error| format!("无法打开页面:{error}"))
}

/// Run one engine command in a tab's page.
///
/// The script is one expression, because the driver awaits it: install the
/// engine (its own guard makes that a no-op once it is there) and evaluate to
/// the command's Promise.
async fn exec(
    driver: &dyn PanelDriver,
    tab_id: &str,
    request: &Value,
    timeout: Duration,
) -> Result<Value, String> {
    let script =
        format!("(function(){{ {ENGINE}\nreturn window.__kodexBrowser.exec({request}); }})()");
    driver.eval(tab_id, &script, timeout).await
}

/// Run a read-only engine command, retrying once across a transport failure.
///
/// The usual transport failure is a navigation that was in flight when the
/// call landed — the page dropped the script, or its answer died with the old
/// document. Settling and asking again is the difference between an answer
/// and a spurious error. Only idempotent commands may come through here:
/// retrying an action would act twice.
async fn exec_read(driver: &dyn PanelDriver, tab_id: &str, request: &Value) -> Result<Value, String> {
    debug_assert!(matches!(
        request.get("cmd").and_then(Value::as_str),
        Some("snapshot" | "text" | "info" | "ping")
    ));
    match exec(driver, tab_id, request, EVAL_OP_TIMEOUT).await {
        Ok(value) => Ok(value),
        Err(error) if is_transport_error(&error) => {
            let _ = driver.wait_settled(tab_id).await;
            exec(driver, tab_id, request, EVAL_OP_TIMEOUT).await
        }
        Err(error) => Err(error),
    }
}

/// Run an action command. A transport failure is *not* reported as an error:
/// it means the outcome is unknown — the page may have executed the action
/// and navigated away before its answer arrived, or the script may never have
/// run. The caller gets `None` and answers with the settled page instead, so
/// the agent sees what the action landed on rather than a bare failure.
async fn exec_action(
    driver: &dyn PanelDriver,
    tab_id: &str,
    request: &Value,
) -> Result<Option<Value>, String> {
    match exec(driver, tab_id, request, EVAL_OP_TIMEOUT).await {
        Ok(value) => Ok(Some(value)),
        Err(error) if is_transport_error(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// What an action reports when the page never confirmed it.
fn action_uncertain(action: &str, snap: &str) -> String {
    format!(
        "{action}已派发,但页面在应答前开始跳转或没有响应——动作可能已生效,也可能没有。\
         以下是最新页面,请核对后再决定是否重试:\n\n{snap}"
    )
}

/// A fresh snapshot, formatted for the model.
async fn snapshot_of(driver: &dyn PanelDriver, tab_id: &str) -> Result<String, String> {
    let snap = exec_read(driver, tab_id, &json!({"cmd": "snapshot"})).await?;
    let url = snap.get("url").and_then(Value::as_str).unwrap_or_default();
    let title = snap.get("title").and_then(Value::as_str).unwrap_or_default();
    let yaml = snap.get("yaml").and_then(Value::as_str).unwrap_or_default();
    let truncated = snap.get("truncated").and_then(Value::as_bool).unwrap_or(false);
    let mut yaml = yaml.to_string();
    if yaml.len() > MAX_SNAPSHOT_CHARS {
        yaml.truncate(MAX_SNAPSHOT_CHARS);
        yaml.push_str("\n…(快照过长,已截断)");
    }
    if truncated {
        yaml.push_str("\n…(元素过多,仅显示前一部分)");
    }
    Ok(format!("### 页面:{title}\n### URL:{url}\n\n{yaml}"))
}

/// A fresh snapshot, or — when the page cannot answer right now — a note that
/// says so. Used wherever the snapshot accompanies an action that already
/// succeeded: the action must not be reported as failed just because the page
/// went quiet afterwards.
async fn snapshot_or_note(driver: &dyn PanelDriver, tab_id: &str) -> String {
    match snapshot_of(driver, tab_id).await {
        Ok(snap) => snap,
        Err(error) => format!("(页面快照暂不可用:{error})"),
    }
}

/// The human-readable bit of an action's outcome, whichever field it used.
fn describe(outcome: &Value, field: &str) -> String {
    outcome
        .get(field)
        .map(|value| match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .unwrap_or_else(|| "目标元素".to_string())
}

/// `browser_wait_for`: text appears / text is gone / plain sleep.
async fn wait_for(driver: &dyn PanelDriver, tab_id: &str, args: &Value) -> Result<(), String> {
    if let Some(seconds) = args.get("time").and_then(Value::as_f64) {
        let capped = seconds.clamp(0.0, WAIT_DEADLINE.as_secs_f64());
        tokio::time::sleep(Duration::from_secs_f64(capped)).await;
        return Ok(());
    }
    let text = args.get("text").and_then(Value::as_str);
    let gone = args.get("textGone").and_then(Value::as_str);
    if text.is_none() && gone.is_none() {
        return Err("browser_wait_for 需要 text、textGone 或 time 中的一个".to_string());
    }
    let deadline = tokio::time::Instant::now() + WAIT_DEADLINE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            let what = text
                .map(|wanted| format!("「{wanted}」一直没有出现"))
                .or_else(|| gone.map(|unwanted| format!("「{unwanted}」一直没有消失")))
                .unwrap_or_default();
            return Err(format!("等待超时:{what}"));
        }
        // 每轮轮询以剩余预算为上限:一次卡住的求值不能把整个等待拖过截止线。
        let poll = exec(
            driver,
            tab_id,
            &json!({"cmd": "text"}),
            remaining.min(EVAL_OP_TIMEOUT),
        )
        .await;
        match poll {
            Ok(outcome) => {
                let body = outcome
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if text.is_some_and(|wanted| body.contains(wanted)) {
                    return Ok(());
                }
                if gone.is_some_and(|unwanted| !body.contains(unwanted)) {
                    return Ok(());
                }
            }
            // 页面多半正在跳转、这一轮的应答随之丢失:等待的意义就是等页面
            // 变化,所以传输层的一轮失败继续等,而不是把整个 wait_for 打成失败。
            Err(error) if is_transport_error(&error) => {}
            Err(error) => return Err(error),
        }
        tokio::time::sleep(WAIT_POLL).await;
    }
}

/// `browser_tabs`: list / new / close / select.
async fn tabs_action(driver: &dyn PanelDriver, args: &Value) -> Result<String, String> {
    let action = required_str(args, "action")?;
    match action {
        "list" => Ok(tab_list(driver).await),
        "new" => {
            driver.open_tab("about:blank").await?;
            Ok(tab_list(driver).await)
        }
        "select" => {
            let tabs = driver.tabs().await;
            let index = args
                .get("index")
                .and_then(Value::as_u64)
                .ok_or_else(|| "browser_tabs select 需要 index".to_string())? as usize;
            let tab = tabs
                .get(index)
                .ok_or_else(|| format!("没有第 {index} 个标签页(共 {} 个)", tabs.len()))?;
            driver.activate_tab(&tab.tab_id).await?;
            let snap = snapshot_of(driver, &tab.tab_id).await?;
            Ok(format!("{}\n\n{snap}", tab_list(driver).await))
        }
        "close" => {
            let tabs = driver.tabs().await;
            let index = args
                .get("index")
                .and_then(Value::as_u64)
                .ok_or_else(|| "browser_tabs close 需要 index".to_string())? as usize;
            let tab = tabs
                .get(index)
                .ok_or_else(|| format!("没有第 {index} 个标签页(共 {} 个)", tabs.len()))?;
            driver.close_tab(&tab.tab_id).await?;
            Ok(tab_list(driver).await)
        }
        other => Err(format!("未知的标签页操作:{other}")),
    }
}

/// The tab strip, in the shape agents know from the provider's browser_tabs.
async fn tab_list(driver: &dyn PanelDriver) -> String {
    let tabs = driver.tabs().await;
    let active = driver.active_tab().await;
    if tabs.is_empty() {
        return "### 打开的标签页\n(没有打开的标签页)".to_string();
    }
    let mut out = String::from("### 打开的标签页");
    for (index, tab) in tabs.iter().enumerate() {
        let current = active.as_deref() == Some(tab.tab_id.as_str());
        out.push_str(&format!(
            "\n- {index}:{} [{}]({})",
            if current { " (current)" } else { "" },
            tab.title,
            tab.url
        ));
    }
    out
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("缺少参数:{key}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_service::panel_driver::PanelTabInfo;
    use std::sync::Mutex;

    /// A driver that answers from a canned DOM-shaped script log.
    struct FakeDriver {
        tabs: Mutex<Vec<PanelTabInfo>>,
        active: Mutex<Option<String>>,
        scripts: Mutex<Vec<String>>,
        /// What the next `exec` returns, keyed by the request's `cmd`.
        answers: Mutex<std::collections::HashMap<String, Value>>,
        /// How many times a cmd fails with a transport error before its
        /// answer is consulted — a navigation swallowing the call.
        transport_failures: Mutex<std::collections::HashMap<String, usize>>,
        /// Persistent page-reported failures, keyed by cmd: the script ran
        /// and the page said no.
        page_failures: Mutex<std::collections::HashMap<String, String>>,
    }

    impl FakeDriver {
        fn new() -> Self {
            Self {
                tabs: Mutex::new(Vec::new()),
                active: Mutex::new(None),
                scripts: Mutex::new(Vec::new()),
                answers: Mutex::new(std::collections::HashMap::new()),
                transport_failures: Mutex::new(std::collections::HashMap::new()),
                page_failures: Mutex::new(std::collections::HashMap::new()),
            }
        }

        fn with_page(self, url: &str) -> Self {
            self.tabs.lock().unwrap().push(PanelTabInfo {
                tab_id: "tab-1".to_string(),
                url: url.to_string(),
                title: "页面标题".to_string(),
            });
            *self.active.lock().unwrap() = Some("tab-1".to_string());
            self
        }

        fn answer(self, cmd: &str, value: Value) -> Self {
            self.answers.lock().unwrap().insert(cmd.to_string(), value);
            self
        }

        /// The next `count` calls carrying `cmd` fail as if the page never
        /// answered.
        fn fail_transport(self, cmd: &str, count: usize) -> Self {
            self.transport_failures
                .lock()
                .unwrap()
                .insert(cmd.to_string(), count);
            self
        }

        /// Every call carrying `cmd` fails with the page's own error.
        fn fail_page(self, cmd: &str, message: &str) -> Self {
            self.page_failures
                .lock()
                .unwrap()
                .insert(cmd.to_string(), message.to_string());
            self
        }

        /// How many calls carried this cmd.
        fn calls_for(&self, cmd: &str) -> usize {
            self.scripts
                .lock()
                .unwrap()
                .iter()
                .filter(|script| {
                    request_json(script)
                        .and_then(|request| {
                            request.get("cmd").and_then(Value::as_str).map(str::to_string)
                        })
                        .as_deref()
                        == Some(cmd)
                })
                .count()
        }
    }

    #[async_trait::async_trait]
    impl PanelDriver for FakeDriver {
        async fn eval(
            &self,
            _tab_id: &str,
            script: &str,
            _timeout: Duration,
        ) -> Result<Value, String> {
            self.scripts.lock().unwrap().push(script.to_string());
            let request = request_json(script)
                .ok_or_else(|| "script did not carry a request".to_string())?;
            let cmd = request
                .get("cmd")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let mut failures = self.transport_failures.lock().unwrap();
            if let Some(remaining) = failures.get_mut(&cmd) {
                if *remaining > 0 {
                    *remaining -= 1;
                    return Err(browser_service::panel_driver::transport_error(
                        "页面在求值期间开始跳转,脚本应答已随旧文档销毁",
                    ));
                }
            }
            drop(failures);
            if let Some(message) = self.page_failures.lock().unwrap().get(&cmd) {
                return Err(message.clone());
            }
            Ok(self
                .answers
                .lock()
                .unwrap()
                .get(&cmd)
                .cloned()
                .unwrap_or_else(|| json!({})))
        }

        async fn navigate(&self, _tab_id: &str, url: &str) -> Result<(), String> {
            self.tabs.lock().unwrap()[0].url = url.to_string();
            Ok(())
        }

        async fn wait_settled(&self, _tab_id: &str) -> Result<(), String> {
            Ok(())
        }

        async fn open_tab(&self, url: &str) -> Result<String, String> {
            let id = format!("tab-{}", self.tabs.lock().unwrap().len() + 1);
            self.tabs.lock().unwrap().push(PanelTabInfo {
                tab_id: id.clone(),
                url: url.to_string(),
                title: String::new(),
            });
            *self.active.lock().unwrap() = Some(id.clone());
            Ok(id)
        }

        async fn close_tab(&self, tab_id: &str) -> Result<(), String> {
            self.tabs.lock().unwrap().retain(|tab| tab.tab_id != tab_id);
            let mut active = self.active.lock().unwrap();
            if active.as_deref() == Some(tab_id) {
                *active = self.tabs.lock().unwrap().last().map(|tab| tab.tab_id.clone());
            }
            Ok(())
        }

        async fn activate_tab(&self, tab_id: &str) -> Result<(), String> {
            *self.active.lock().unwrap() = Some(tab_id.to_string());
            Ok(())
        }

        async fn tabs(&self) -> Vec<PanelTabInfo> {
            self.tabs.lock().unwrap().clone()
        }

        async fn active_tab(&self) -> Option<String> {
            self.active.lock().unwrap().clone()
        }
    }


    /// Pull the request JSON out of an exec script by brace matching: the
    /// script wraps the call in a function body, so a suffix trim is not
    /// enough.
    fn request_json(script: &str) -> Option<Value> {
        let start = script.rfind("window.__kodexBrowser.exec(")? + "window.__kodexBrowser.exec(".len();
        let tail = &script[start..];
        let mut depth = 0i32;
        let mut end = None;
        for (index, ch) in tail.char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(index + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        serde_json::from_str(&tail[..end?]).ok()
    }

    #[test]
    fn request_json_reads_through_the_wrapper() {
        let script = format!("(function(){{ /* engine */\nreturn window.__kodexBrowser.exec({{}}); }})()");
        assert_eq!(request_json(&script), Some(json!({})));
        let script = format!("(function(){{\nreturn window.__kodexBrowser.exec({}); }})()", json!({"cmd": "type", "values": ["b"]}));
        assert_eq!(request_json(&script), Some(json!({"cmd": "type", "values": ["b"]})));
    }

    fn snapshot_answer() -> Value {
        json!({"url": "https://a.dev/", "title": "页面标题", "yaml": "- link \"x\" [ref=e1]", "truncated": false})
    }

    #[tokio::test]
    async fn navigate_opens_a_tab_when_the_panel_has_none() {
        let driver = std::sync::Arc::new(
            FakeDriver::new().answer("snapshot", snapshot_answer()),
        );

        let text = dispatch(
            driver.as_ref(),
            "browser_navigate",
            json!({"url": "https://a.dev/"}),
        )
        .await
        .unwrap();

        assert_eq!(driver.active_tab().await.as_deref(), Some("tab-1"));
        assert!(text.contains("已导航到 https://a.dev/"), "got {text}");
        assert!(text.contains("- link \"x\" [ref=e1]"), "got {text}");
    }

    #[tokio::test]
    async fn tools_that_need_a_page_say_so_when_there_is_none() {
        let driver = FakeDriver::new();

        let error = dispatch(&driver, "browser_snapshot", json!({}))
            .await
            .unwrap_err();

        assert!(error.contains("browser_navigate"), "got {error}");
    }

    #[tokio::test]
    async fn a_click_carries_the_ref_to_the_page() {
        let driver = std::sync::Arc::new(
            FakeDriver::new()
                .with_page("https://a.dev/")
                .answer("click", json!({"clicked": "link \"x\""}))
                .answer("snapshot", snapshot_answer()),
        );

        let text = dispatch(
            driver.as_ref(),
            "browser_click",
            json!({"element": "x 链接", "ref": "e1"}),
        )
        .await
        .unwrap();

        let scripts = driver.scripts.lock().unwrap();
        assert!(scripts[0].contains(r#""cmd": "click""#) || scripts[0].contains(r#""cmd":"click""#), "got {}", scripts[0]);
        assert!(scripts[0].contains(r#""ref": "e1""#) || scripts[0].contains(r#""ref":"e1""#), "got {}", scripts[0]);
        assert!(text.contains("已点击 link \"x\""), "got {text}");
    }

    #[tokio::test]
    async fn a_click_lost_to_a_navigation_degrades_to_the_settled_page() {
        // 点击触发了跳转:动作可能已生效,但页面在应答前换掉了文档。这不是
        // 失败 —— agent 需要的是落定后的页面,而不是一句超时。
        let driver = std::sync::Arc::new(
            FakeDriver::new()
                .with_page("https://a.dev/")
                .fail_transport("click", 1)
                .answer("snapshot", snapshot_answer()),
        );

        let text = dispatch(
            driver.as_ref(),
            "browser_click",
            json!({"element": "x 链接", "ref": "e1"}),
        )
        .await
        .unwrap();

        assert!(text.contains("可能已生效"), "got {text}");
        assert!(text.contains("- link \"x\" [ref=e1]"), "got {text}");
        // 动作只派发一次:传输层失败意味着结果未知,重试可能点两次。
        assert_eq!(driver.calls_for("click"), 1);
    }

    #[tokio::test]
    async fn a_snapshot_retries_once_across_a_navigation() {
        // 读操作是幂等的:第一次求值撞上在途的跳转,落定后重试应当拿到答案。
        let driver = std::sync::Arc::new(
            FakeDriver::new()
                .with_page("https://a.dev/")
                .fail_transport("snapshot", 1)
                .answer("snapshot", snapshot_answer()),
        );

        let text = dispatch(driver.as_ref(), "browser_snapshot", json!({}))
            .await
            .unwrap();

        assert!(text.contains("- link \"x\" [ref=e1]"), "got {text}");
        assert_eq!(driver.calls_for("snapshot"), 2);
    }

    #[tokio::test]
    async fn a_page_reported_error_is_not_retried() {
        // 页面自己报告的错误是定论:ref 过期、元素不可点……重试只会重复失败,
        // 错误必须原样到达 agent。
        let driver = std::sync::Arc::new(
            FakeDriver::new()
                .with_page("https://a.dev/")
                .fail_page("click", "元素 e1 不存在或已过期(页面已变化)")
                .answer("snapshot", snapshot_answer()),
        );

        let error = dispatch(
            driver.as_ref(),
            "browser_click",
            json!({"element": "x 链接", "ref": "e1"}),
        )
        .await
        .unwrap_err();

        assert!(error.contains("元素 e1 不存在或已过期"), "got {error}");
        assert_eq!(driver.calls_for("click"), 1);
    }

    #[tokio::test]
    async fn a_persistent_transport_failure_surfaces_after_the_retry() {
        let driver = std::sync::Arc::new(
            FakeDriver::new()
                .with_page("https://a.dev/")
                .fail_transport("snapshot", usize::MAX),
        );

        let error = dispatch(driver.as_ref(), "browser_snapshot", json!({}))
            .await
            .unwrap_err();

        assert!(error.contains("求值期间开始跳转"), "got {error}");
        assert_eq!(driver.calls_for("snapshot"), 2);
    }

    #[tokio::test]
    async fn wait_for_tolerates_a_navigation_mid_wait() {
        let driver = std::sync::Arc::new(
            FakeDriver::new()
                .with_page("https://a.dev/")
                .fail_transport("text", 2)
                .answer("text", json!({"text": "加载完成"}))
                .answer("snapshot", snapshot_answer()),
        );

        let text = dispatch(
            driver.as_ref(),
            "browser_wait_for",
            json!({"text": "加载完成"}),
        )
        .await
        .unwrap();

        assert!(text.contains("等待结束"), "got {text}");
    }

    #[tokio::test]
    async fn wait_for_times_out_with_the_missing_text_named() {
        let driver = std::sync::Arc::new(
            FakeDriver::new()
                .with_page("https://a.dev/")
                .answer("text", json!({"text": "别的内容"})),
        );

        let started = std::time::Instant::now();
        let error = dispatch(
            driver.as_ref(),
            "browser_wait_for",
            json!({"text": "加载完成"}),
        )
        .await
        .unwrap_err();

        // The fake answers instantly, so the full deadline is spent polling.
        assert!(started.elapsed() >= WAIT_DEADLINE);
        assert!(error.contains("「加载完成」一直没有出现"), "got {error}");
    }

    #[tokio::test]
    async fn tabs_select_activates_by_index() {
        let driver = std::sync::Arc::new(
            FakeDriver::new()
                .with_page("https://a.dev/")
                .answer("snapshot", snapshot_answer()),
        );
        driver.open_tab("https://b.dev/").await.unwrap();
        // open_tab activated tab-2; select tab-1 by index.
        let text = dispatch(
            driver.as_ref(),
            "browser_tabs",
            json!({"action": "select", "index": 0}),
        )
        .await
        .unwrap();

        assert_eq!(driver.active_tab().await.as_deref(), Some("tab-1"));
        assert!(text.contains("- 0: (current)"), "got {text}");
    }

    #[test]
    fn the_native_catalog_keeps_the_provider_names() {
        let names: Vec<_> = native_catalog()
            .iter()
            .map(|tool| tool.exposed_name.as_str())
            .collect();
        for expected in [
            "mcp__playwright-mcp__browser_navigate",
            "mcp__playwright-mcp__browser_snapshot",
            "mcp__playwright-mcp__browser_click",
            "mcp__playwright-mcp__browser_type",
            "mcp__playwright-mcp__browser_tabs",
        ] {
            assert!(names.contains(&expected), "missing {expected}");
        }
        // Names stay unique, or one of them could never be called.
        let mut deduped = names.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(names.len(), deduped.len());
    }
}
