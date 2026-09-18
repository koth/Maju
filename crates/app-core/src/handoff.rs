//! Model-polished handoff briefings.
//!
//! The timeline builds the handoff material locally (see
//! `apps/desktop/ui/src/features/conversation/handoff.ts`): a factual, capped
//! digest of the conversation through the turn the user picked. Turning that into
//! a *readable* handoff note — what was decided and why, what is still open —
//! needs a model, so this module makes exactly one text-only request through the
//! same local `codex_api_proxy` route the image-view fallback uses, against the
//! provider configured for session titles (`settings.session_title`).
//!
//! Every failure is the caller's to absorb: the desktop dialog keeps its local
//! digest and shows the error, so a broken or unconfigured provider degrades to
//! "no polish" rather than "no handoff".

use serde_json::{Value, json};

/// Stable proxy session id for handoff summaries: they are an independent
/// pipeline, not an ACP conversation, so they must not join the active
/// conversation's proxy-retry registry (mirrors `IMAGE_VIEW_PROXY_SESSION_ID`).
const HANDOFF_PROXY_SESSION_ID: &str = "kodex-handoff-summary";

/// Input cap. The digest is already capped by the UI; this bounds a caller that
/// passes something bigger (and keeps one accidental paste from burning the
/// title model's whole window).
const MAX_MATERIAL_CHARS: usize = 60_000;

/// Instruction prepended to the digest.
const HANDOFF_INSTRUCTION: &str = "\
你是一个技术交接助手。下面是上一个会话的结构化记录（用户请求、工具活动、文件改动、结论等）。\
请把它整理成一份交给下一个智能体的交接说明，用中文、Markdown，按以下小节组织：\
1) 任务目标；2) 已完成的工作与关键决策（说明为什么这样定）；3) 改动的文件及其作用；\
4) 未完成或待确认的事项；5) 下一步建议。\
只使用记录中出现的信息，不要补全或猜测；记录里没有的内容写“记录中未体现”。\
不要写客套话，直接给结论。";

/// Rewrite `material` into a handoff briefing with one model call.
pub async fn polish_handoff(
    provider: &str,
    model: &str,
    api_key: Option<&str>,
    material: &str,
) -> Result<String, String> {
    let provider = provider.trim();
    let model = model.trim();
    if provider.is_empty() || model.is_empty() {
        return Err("未配置交接摘要模型（设置 → 会话标题）".to_string());
    }
    let material = material.trim();
    if material.is_empty() {
        return Err("没有可整理的交接内容".to_string());
    }
    let material = cap_material(material);

    // Register the provider's key on the proxy without disturbing the active
    // session's provider: the proxy routes by the provider pinned in the path.
    if let Some(api_key) = api_key {
        acp_core::register_codex_api_proxy_provider_key(provider, api_key);
    }

    let payload = json!({
        "model": model,
        "stream": false,
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": format!("{HANDOFF_INSTRUCTION}\n\n---\n\n{material}")
            }]
        }]
    });
    let url = format!(
        "{}/providers/{provider}/responses",
        acp_core::codex_api_proxy_base_url()
    );
    let response = reqwest::Client::new()
        .post(&url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header("session-id", HANDOFF_PROXY_SESSION_ID)
        .json(&payload)
        .send()
        .await
        .map_err(|error| format!("交接摘要请求失败：{error}"))?;
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .map_err(|error| format!("交接摘要响应无法解析（{status}）：{error}"))?;
    if !status.is_success() {
        return Err(format!(
            "交接摘要调用失败（{status}）：{}。请确认代理在运行、且 `{provider}` 已配置密钥。",
            crate::image_api::error_message(&body)
        ));
    }
    let text = crate::image_api::extract_responses_output_text(&body)
        .ok_or_else(|| "交接摘要没有返回文本".to_string())?;
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("交接摘要返回了空文本".to_string());
    }
    Ok(text)
}

/// Cap the material at [`MAX_MATERIAL_CHARS`] (char-boundary safe).
fn cap_material(material: &str) -> String {
    if material.chars().count() <= MAX_MATERIAL_CHARS {
        return material.to_string();
    }
    material.chars().take(MAX_MATERIAL_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruction_forbids_inventing_facts() {
        // The digest is the only source of truth; a model that "helpfully"
        // completes missing details would corrupt the handoff.
        assert!(HANDOFF_INSTRUCTION.contains("不要补全或猜测"));
        assert!(HANDOFF_INSTRUCTION.contains("记录中未体现"));
        assert!(HANDOFF_INSTRUCTION.contains("下一步建议"));
    }

    #[test]
    fn material_is_capped_and_kept_whole_below_the_cap() {
        // Guards the title model's window against a pathological paste, and
        // keeps ordinary digests byte-for-byte.
        let short = "# 交接说明\n- 改了两个文件";
        assert_eq!(cap_material(short), short);

        let long = "x".repeat(MAX_MATERIAL_CHARS + 5_000);
        assert_eq!(cap_material(&long).chars().count(), MAX_MATERIAL_CHARS);
    }
}
