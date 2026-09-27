//! Commit-message assistant: a throwaway codex-acp session that inspects the
//! staged changes and returns a conventional-commit message. The agent and
//! model are configured in the settings "Commit 助手" pane; the visible
//! conversation is never touched.

use super::*;

/// The generation prompt handed to the throwaway codex session. Kept as a
/// plain constant so the orchestration in `generate_commit_message` stays
/// readable.
const COMMIT_MESSAGE_PROMPT: &str = "你是 commit message 生成器。请直接在回复正文里给我一条 commit message，禁止调用任何写文件/编辑/创建文件的工具。\n\
     \n\
     第一步：用尽量少的只读命令了解已暂存变更。推荐一次性执行：\n\
     - `git diff --staged` 看完整 staged diff（若太长才改用 `--stat` + `--name-status`）\n\
     - 可选 `git log -5 --oneline` 参考提交风格\n\
     不要逐个文件反复跑 `git diff --staged -- <path>`，一次看全即可。\n\
     \n\
     第二步：看完后立即在回复正文输出 commit message，到此就结束，不要再做任何操作。\n\
     \n\
     格式：\n\
     1. 第一行：约定式提交标题（feat/fix/refactor/docs/test/chore: 描述），≤72 字符\n\
     2. 空一行\n\
     3. 正文：2-6 条 `- ` 项目符号，说明改了什么、为什么改、关键影响\n\
     \n\
     铁律（违反则任务失败）：\n\
     - 把 commit message 作为普通文本直接写在回复里，不要调用任何工具来创建、写入或保存文件（包括但不限于 write_file / edit_file / apply_patch / shell 重定向）\n\
     - 不要把结果保存到文件，不要创建 commit_message.txt 之类的文件\n\
     - 不要包在 ``` 代码围栏里，不要加引号、标签、markdown 标题\n\
     - 不要输出思考过程、“Let me…”/“Generated…”等旁白\n\
     - 只允许只读命令，禁止 stage/unstage/commit/push 或任何修改操作\n\
     - 全部内容就是这一条 commit message 本身。";

/// Hard cap on a single generation run; the UI keeps polling heartbeats until
/// this fires, then the task is abandoned instead of hanging forever.
const GENERATE_TIMEOUT: Duration = Duration::from_secs(120);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

impl Application {
    /// Generate a commit-message draft by spinning up a throwaway codex-acp
    /// sub-agent session. The agent inspects the staged changes itself with
    /// read-only commands and returns just the message. The temporary session
    /// is used once and shut down; it never touches the visible conversation.
    /// `progress` receives human-readable status updates as the agent works.
    /// Blocking — call off the UI thread.
    pub fn generate_commit_message(&self, progress: &dyn Fn(&str)) -> Result<String, String> {
        if self.is_remote_workspace() {
            return Err("远程工作区暂不支持 AI 生成提交信息".to_string());
        }

        // The commit assistant always runs on the codex agent, regardless of
        // which agent the visible session uses. The model comes from the
        // settings "Commit 助手" pane when configured, falling back to the
        // visible session's current model.
        let agent_command =
            crate::settings::command_for_agent_with_paths(AgentCliId::CodexAcp, &self.app_paths)
                .ok_or_else(|| {
                    "codex agent 命令不可用，请在设置中检查 codex-acp 安装".to_string()
                })?;
        if !crate::settings::detect_agent_with_paths(&self.app_paths, AgentCliId::CodexAcp)
            .installed
        {
            return Err("codex agent 未安装，请先在设置的智能体页面安装 codex-acp".to_string());
        }

        let model_selection = self
            .commit_assistant_model_selection()
            .or_else(|| self.current_model_for_background_session());
        let config = SessionConfig {
            workspace_root: self.ui.workspace.root.display().to_string(),
            app_data_root: self.app_paths.root().display().to_string(),
            model: model_selection
                .as_ref()
                .map(|(value, _)| value.clone())
                .unwrap_or_else(|| self.ui.session.model.clone()),
            agent_command: agent_command.clone(),
            agent_env: crate::settings::agent_env_for_command(&agent_command, &self.app_paths),
            resume_session_id: None,
            log_id: make_log_id(),
            acp_port: self.acp_port,
            remote_ssh: None,
            mcp_servers: Vec::new(),
            harness_endpoint: None,
            agent_preset: None,
        };

        progress("正在启动 AI 会话…");
        let mut handle =
            SessionHandle::start(config).map_err(|e| format!("无法启动 AI 会话：{e}"))?;
        crate::startup_perf::mark("commit-gen/handle_started", "session handle created");
        push_background_model(&mut handle, model_selection);
        // Full access: 生成任务无权限 UI，plan/readonly 模式下任何触发
        // `Ask` 的命令都会让 broker 无限阻塞等用户回答（死锁，表现为
        // "仍在等待 AI 响应" 永不结束）。Full access 下 broker 直接放行，
        // 只读约束由 prompt（"只允许只读命令"）在 agent 层面保证。
        let _ = handle.set_permission_mode("full-access");

        progress("正在查看已暂存的变更…");
        let collected = match handle.send_prompt_async(COMMIT_MESSAGE_PROMPT) {
            Ok(task) => {
                crate::startup_perf::mark("commit-gen/prompt_dispatched", "prompt sent to worker");
                collect_commit_draft(task, &mut handle, progress)
            }
            Err(e) => Err(format!("AI 生成失败：{e}")),
        };
        handle.shutdown();

        let raw = collected?;
        progress("正在整理提交信息…");
        let message = sanitize_generated_commit_message(&raw);
        if message.is_empty() {
            return Err("AI 没有返回可用的提交信息".to_string());
        }
        Ok(message)
    }

    /// Model selection for the commit assistant. When the settings pane has a
    /// provider+model pair configured (and that provider resolves), return the
    /// provider-qualified model value (the same encoding the composer uses)
    /// plus the plain provider id. Returns `None` when unset so the caller can
    /// fall back to the visible session's model.
    fn commit_assistant_model_selection(&self) -> Option<(String, Option<String>)> {
        let settings = crate::settings::load_app_settings(&self.app_paths);
        let provider = settings.commit_assistant.provider.trim().to_string();
        let model = settings.commit_assistant.model.trim().to_string();
        if provider.is_empty() || model.is_empty() {
            return None;
        }
        let qualified = super::config::provider_qualified_model_value(&model, Some(&provider));
        Some((qualified, Some(provider)))
    }
}

/// The throwaway session does NOT inherit the visible session's model —
/// `SessionConfig.model` only carries a display label, so without an explicit
/// model push the agent falls back to the default baked into `config.toml`.
/// That default frequently points at a model the BYOK proxy can't serve,
/// stalling the task with zero events until the 120s timeout
/// ("仍在等待 AI 响应…"). Push the resolved model first: the commit-assistant
/// settings selection when configured, otherwise the visible session's model.
fn push_background_model(
    handle: &mut SessionHandle,
    model_selection: Option<(String, Option<String>)>,
) {
    let Some((model_id, provider)) = model_selection else {
        crate::startup_perf::mark("commit-gen/set_model_skipped", "no model resolved");
        return;
    };
    crate::startup_perf::mark(
        "commit-gen/set_model",
        format!("model={model_id:?} provider={provider:?}"),
    );
    // CodeBuddy/Codex agents expose the model as a config option ("model")
    // and do NOT implement `session/set_model` (returns "Method not found"),
    // so a `set_model` call leaves the session on the `config.toml` default
    // and the agent bails with an empty refusal ("AI 没有返回可用的提交信息").
    // Prefer the config-option path — the same one the main session uses when
    // the user picks a model — and fall back to `set_model` for agents that
    // only support the dedicated method.
    if let Err(config_option_error) =
        handle.set_config_option("model", model_id.clone(), provider.clone())
    {
        crate::startup_perf::mark(
            "commit-gen/set_config_option_failed",
            config_option_error.to_string(),
        );
        if let Err(model_error) = handle.set_model(model_id, provider) {
            crate::startup_perf::mark("commit-gen/set_model_failed", model_error.to_string());
        }
    }
}

/// Drain the prompt task until it finishes, keeping only the assistant text
/// emitted after the latest tool call (earlier narration like "let me
/// inspect…" is discarded so the draft is not polluted with chain-of-thought
/// preamble). Non-blocking poll + timeout + heartbeat: an ACP agent may go
/// silent for a long time (network / stalled model / permission block), and a
/// synchronous wait would freeze the whole task with a stale progress line.
fn collect_commit_draft(
    mut task: PromptTask,
    handle: &mut SessionHandle,
    progress: &dyn Fn(&str),
) -> Result<String, String> {
    let mut text = String::new();
    let mut run_error: Option<String> = None;
    let started_at = Instant::now();
    let mut last_heartbeat = started_at;
    let mut heartbeat_count = 0u32;
    while !task.is_finished() {
        if started_at.elapsed() > GENERATE_TIMEOUT {
            crate::startup_perf::mark(
                "commit-gen/timeout",
                format!(
                    "no response after {}s, collected_len={}",
                    GENERATE_TIMEOUT.as_secs(),
                    text.len()
                ),
            );
            run_error = Some(format!(
                "生成超时（{} 秒无响应），请检查 AI 服务后重试",
                GENERATE_TIMEOUT.as_secs()
            ));
            break;
        }
        match task.collect_ready_events(handle) {
            Ok(events) => {
                for event in &events {
                    match event {
                        ClientEvent::MessageChunk {
                            role: workspace_model::MessageRole::Assistant,
                            content,
                        } => {
                            text.push_str(content);
                            crate::startup_perf::mark(
                                "commit-gen/chunk",
                                format!("len={}", text.len()),
                            );
                        }
                        ClientEvent::ToolStarted { name, summary, .. } => {
                            text.clear();
                            let label = if summary.is_empty() {
                                name.clone()
                            } else {
                                summary.clone()
                            };
                            crate::startup_perf::mark("commit-gen/tool", label.clone());
                            progress(&format!("正在执行：{label}"));
                            last_heartbeat = Instant::now();
                            // The prompt forbids write tools — catch a
                            // misbehaving agent that saves the message to a
                            // file instead of emitting it as text, which would
                            // otherwise stall until timeout with
                            // `collected_len=0`.
                            if is_write_tool(name) {
                                run_error = Some(format!(
                                    "AI 尝试写入文件（{name}）而非直接输出结果，已中止"
                                ));
                            }
                        }
                        ClientEvent::Interrupted { reason } => {
                            crate::startup_perf::mark("commit-gen/interrupted", reason.clone());
                            run_error = Some(reason.clone());
                        }
                        _ => {}
                    }
                }
            }
            Err(e) => {
                run_error = Some(e.to_string());
                break;
            }
        }
        // 心跳：长时间无事件时让 UI 知道任务仍存活
        if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
            heartbeat_count += 1;
            let dots = ".".repeat((heartbeat_count % 3 + 1) as usize);
            progress(&format!(
                "仍在等待 AI 响应{dots}（已等待 {} 秒）",
                started_at.elapsed().as_secs()
            ));
            last_heartbeat = Instant::now();
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    for event in task.into_events() {
        match &event {
            ClientEvent::MessageChunk {
                role: workspace_model::MessageRole::Assistant,
                content,
            } => text.push_str(content),
            ClientEvent::ToolStarted { .. } => text.clear(),
            _ => {}
        }
    }
    match run_error {
        Some(e) => Err(format!("AI 生成失败：{e}")),
        None => Ok(text),
    }
}

/// Normalize an AI-produced commit message into a multi-line draft.
/// Keeps the subject + body, strips thinking preamble / code fences / labels /
/// surrounding quotes.
/// Detect tools that create/modify files. The commit-gen prompt forbids these
/// (the agent must emit the message as assistant text, not save it to disk);
/// returning true lets the loop bail out instead of stalling to timeout.
fn is_write_tool(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    const WRITE_TOOLS: &[&str] = &[
        "write_file",
        "writefile",
        "create_file",
        "createfile",
        "edit_file",
        "editfile",
        "apply_patch",
        "applypatch",
        "str_replace_editor",
        "rewrite_file",
        "write_to_file",
    ];
    WRITE_TOOLS
        .iter()
        .any(|tool| lower == *tool || lower.contains(tool))
}

fn sanitize_generated_commit_message(raw: &str) -> String {
    let mut text = raw.replace("\r\n", "\n").replace('\r', "\n");
    text = text.trim().to_string();

    // Prefer the last fenced block when the model wraps the message (or wraps
    // an intermediate draft). Ignore fences that don't look like a commit
    // message (e.g. code samples in the narration). Fall back to stripping a
    // single leading fence when the whole answer is wrapped.
    if let Some(extracted) = extract_last_fenced_commit_block(&text) {
        text = extracted;
    } else if text.starts_with("```") {
        let mut lines: Vec<&str> = text.lines().collect();
        if lines.first().is_some_and(|line| line.starts_with("```")) {
            lines.remove(0);
        }
        if lines.last().is_some_and(|line| line.trim() == "```") {
            lines.pop();
        }
        text = lines.join("\n").trim().to_string();
    }

    // Drop common leading labels the model sometimes emits (anywhere on a line).
    const LABELS: &[&str] = &[
        "commit message:",
        "commit message：",
        "提交信息:",
        "提交信息：",
        "message:",
        "message：",
    ];
    for label in LABELS {
        let lower = text.to_ascii_lowercase();
        let label_lower = label.to_ascii_lowercase();
        if lower.starts_with(&label_lower) {
            text = text[label.len()..].trim_start().to_string();
            break;
        }
    }

    // If narration precedes the real subject, cut everything before the last
    // conventional-commit subject line.
    if let Some(extracted) = extract_from_commit_subject(&text) {
        text = extracted;
    }

    // Preserve internal blank lines (subject/body separator) while trimming edges.
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let mut start = 0usize;
    let mut end = lines.len();
    while start < end && lines[start].trim().is_empty() {
        start += 1;
    }
    while end > start && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    if start >= end {
        return String::new();
    }

    let mut cleaned = lines[start..end].join("\n");
    // Collapse 3+ blank lines down to a single blank line.
    while cleaned.contains("\n\n\n") {
        cleaned = cleaned.replace("\n\n\n", "\n\n");
    }

    // Strip wrapping quotes around the whole message.
    let trimmed = cleaned.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('`') && trimmed.ends_with('`'))
    {
        return trimmed[1..trimmed.len() - 1].trim().to_string();
    }
    cleaned
}

/// Return the content of the last ``` ... ``` fenced block, if any.
fn extract_last_fenced_commit_block(text: &str) -> Option<String> {
    let mut last: Option<String> = None;
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if !line.trim_start().starts_with("```") {
            continue;
        }
        let mut body: Vec<&str> = Vec::new();
        while let Some(inner) = lines.next() {
            if inner.trim() == "```" {
                break;
            }
            body.push(inner);
        }
        let content = body.join("\n").trim().to_string();
        if !content.is_empty() && content.lines().any(looks_like_commit_subject) {
            last = Some(content);
        }
    }
    last
}

/// Conventional-commit subject: `type(scope)!: description`
fn looks_like_commit_subject(line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() || line.len() > 100 {
        return false;
    }
    // Reject obvious prose / bullets / markdown.
    if line.starts_with('-')
        || line.starts_with('*')
        || line.starts_with('#')
        || line.starts_with('>')
        || line.starts_with('`')
    {
        return false;
    }
    const TYPES: &[&str] = &[
        "feat", "fix", "refactor", "docs", "test", "chore", "style", "perf", "build", "ci",
        "revert",
    ];
    let lower = line.to_ascii_lowercase();
    for ty in TYPES {
        if !lower.starts_with(ty) {
            continue;
        }
        let rest = &lower[ty.len()..];
        // type: / type!: / type(scope): / type(scope)!:
        if rest.starts_with(':') || rest.starts_with("!:") {
            let desc = rest.trim_start_matches('!').trim_start_matches(':').trim();
            return !desc.is_empty();
        }
        if let Some(after_scope) = rest
            .strip_prefix('(')
            .and_then(|s| s.find(')').map(|i| &s[i + 1..]))
        {
            if after_scope.starts_with(':') || after_scope.starts_with("!:") {
                let desc = after_scope
                    .trim_start_matches('!')
                    .trim_start_matches(':')
                    .trim();
                return !desc.is_empty();
            }
        }
    }
    false
}

/// If the text contains narration before a conventional-commit subject, return
/// the slice starting at the last such subject. When the first non-empty line
/// is already a subject, return None (nothing to strip).
fn extract_from_commit_subject(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut subject_indexes: Vec<usize> = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if looks_like_commit_subject(line) {
            subject_indexes.push(idx);
        }
    }
    let subject_idx = *subject_indexes.last()?;

    // Nothing to strip if the message already starts at (or is only blank lines
    // before) the subject.
    let leading_nonempty = lines[..subject_idx]
        .iter()
        .any(|line| !line.trim().is_empty());
    if !leading_nonempty {
        return None;
    }

    Some(lines[subject_idx..].join("\n").trim().to_string())
}

#[cfg(test)]
mod generate_commit_message_tests {
    use super::sanitize_generated_commit_message;

    #[test]
    fn keeps_multiline_commit_message_body() {
        let raw = "feat: improve commit drafts\n\n- keep the subject concise\n- expand the body with details\n";
        let message = sanitize_generated_commit_message(raw);
        assert_eq!(
            message,
            "feat: improve commit drafts\n\n- keep the subject concise\n- expand the body with details"
        );
    }

    #[test]
    fn strips_code_fences_and_labels() {
        let raw =
            "```\nCommit message:\nfix: repair dialog layout\n\n- switch input to textarea\n```\n";
        let message = sanitize_generated_commit_message(raw);
        assert_eq!(
            message,
            "fix: repair dialog layout\n\n- switch input to textarea"
        );
    }

    #[test]
    fn collapses_excess_blank_lines() {
        let raw = "chore: tidy\n\n\n\n- one\n\n\n- two\n";
        let message = sanitize_generated_commit_message(raw);
        assert_eq!(message, "chore: tidy\n\n- one\n\n- two");
    }

    #[test]
    fn strips_thinking_preamble_before_subject() {
        let raw = "\
Now let me look at the remaining diff sections that were truncated:\
I now have a comprehensive view of the entire diff. Let me summarize the changes and produce the commit message.\n\
\n\
refactor: unified steel-blue accent across all themes with token-driven CSS\n\
\n\
- Replaces the previously split warm-teal accent pair with a single steel-blue family.\n\
- Migrates hardcoded color literals to semantic CSS custom properties.\n";
        let message = sanitize_generated_commit_message(raw);
        assert_eq!(
            message,
            "refactor: unified steel-blue accent across all themes with token-driven CSS\n\n\
- Replaces the previously split warm-teal accent pair with a single steel-blue family.\n\
- Migrates hardcoded color literals to semantic CSS custom properties."
        );
    }

    #[test]
    fn strips_preamble_and_keeps_scoped_subject() {
        let raw = "Looking at the staged diff now.\n\nfeat(ui): polish commit dialog\n\n- widen textarea\n";
        let message = sanitize_generated_commit_message(raw);
        assert_eq!(
            message,
            "feat(ui): polish commit dialog\n\n- widen textarea"
        );
    }

    #[test]
    fn prefers_last_fenced_commit_block() {
        let raw = "\
drafting…\n\
```\nnot the message\n```\n\
```\nfix: final draft\n\n- keep this one\n```\n";
        let message = sanitize_generated_commit_message(raw);
        assert_eq!(message, "fix: final draft\n\n- keep this one");
    }
}
