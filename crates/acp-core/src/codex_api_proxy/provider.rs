use serde_json::Value;
use std::collections::BTreeMap;

use super::{
    COMMANDCODE_UPSTREAM_CHAT_COMPLETIONS_URL, COMMANDCODE_UPSTREAM_MESSAGES_URL,
    DEEPSEEK_UPSTREAM_CHAT_COMPLETIONS_URL, KIMI_UPSTREAM_CHAT_COMPLETIONS_URL,
    KIMI_UPSTREAM_MESSAGES_URL, MIMO_UPSTREAM_CHAT_COMPLETIONS_URL, MIMO_UPSTREAM_MESSAGES_URL,
    PROVIDER_MODEL_ID_PREFIX, TIMIAI_CHAT_COMPLETIONS_URL, TIMIAI_MESSAGES_URL,
};

#[derive(Debug, Clone)]
pub(super) struct ProviderEncodedModel {
    pub(super) provider: String,
    pub(super) model: String,
}

pub(super) fn normalize_proxy_provider(provider: &str) -> &'static str {
    match provider.trim().to_ascii_lowercase().as_str() {
        "timiai" | "timi" | "timi-ai" | "timi_ai" => "timiai",
        "commandcode" | "command-code" | "command_code" => "commandcode",
        "deepseek" => "deepseek",
        "kimi" | "kimi_code" | "kimi-code" => "kimi_code",
        "mimo" | "xiaomi_mimo" | "xiaomi-mimo" => "xiaomi_mimo",
        "custom" | "custom_provider" | "custom-provider" => "custom",
        "byok" => "byok",
        _ => "timiai",
    }
}

pub(super) fn proxy_provider_from_path(path: &str) -> Option<&'static str> {
    let (_, rest) = path.split_once("/providers/")?;
    let provider = rest.split('/').next().unwrap_or_default().trim();
    (!provider.is_empty()).then(|| normalize_proxy_provider(provider))
}

pub(super) fn decode_provider_model_id(model: &str) -> Option<ProviderEncodedModel> {
    let rest = model.trim().strip_prefix(PROVIDER_MODEL_ID_PREFIX)?;
    let (provider, upstream_model) = rest.split_once('/')?;
    let provider = normalize_proxy_provider(provider);
    let upstream_model = upstream_model.trim();
    if upstream_model.is_empty() {
        return None;
    }
    if provider == "byok" {
        if let Some((source_provider, source_model)) = upstream_model.split_once('/') {
            let source_provider = normalize_proxy_provider(source_provider);
            let source_model = source_model.trim();
            if source_provider != "byok" && !source_model.is_empty() {
                return Some(ProviderEncodedModel {
                    provider: source_provider.to_string(),
                    model: source_model.to_string(),
                });
            }
        }
    }
    Some(ProviderEncodedModel {
        provider: provider.to_string(),
        model: upstream_model.to_string(),
    })
}

pub(super) fn replace_payload_model(mut payload: Value, model: &str) -> Value {
    if let Some(object) = payload.as_object_mut() {
        object.insert("model".to_string(), Value::String(model.to_string()));
    }
    payload
}

pub(super) fn mapped_proxy_provider_for_model(
    model: &str,
    model_providers: &BTreeMap<String, String>,
) -> Option<String> {
    let model_key = normalized_model_key(model);
    model_providers
        .get(&model_key)
        .map(|provider| normalize_proxy_provider(provider).to_string())
}

pub(super) fn proxy_provider_for_model(
    model: &str,
    fallback_provider: &str,
    model_providers: &BTreeMap<String, String>,
) -> String {
    if let Some(provider) = mapped_proxy_provider_for_model(model, model_providers) {
        return provider;
    }
    proxy_provider_for_model_heuristic(model)
        .unwrap_or_else(|| normalize_proxy_provider(fallback_provider))
        .to_string()
}

pub(super) fn proxy_provider_for_model_heuristic(model: &str) -> Option<&'static str> {
    let normalized = normalized_model_key(model);
    if normalized.starts_with("qwen/")
        || normalized.starts_with("minimaxai/")
        || normalized.starts_with("moonshotai/")
        || normalized.starts_with("zai-org/")
        || normalized.starts_with("stepfun/")
        || normalized.starts_with("google/")
        || normalized.starts_with("openai/")
    {
        Some("commandcode")
    } else if normalized.contains("deepseek") {
        Some("deepseek")
    } else if normalized.contains("kimi") {
        Some("kimi_code")
    } else if normalized.contains("mimo") {
        Some("xiaomi_mimo")
    } else {
        None
    }
}

pub(super) fn normalized_model_key(model: &str) -> String {
    model.trim().to_ascii_lowercase()
}

pub(super) fn upstream_chat_completions_url(provider: &str) -> &'static str {
    match normalize_proxy_provider(provider) {
        "timiai" => TIMIAI_CHAT_COMPLETIONS_URL,
        "commandcode" => COMMANDCODE_UPSTREAM_CHAT_COMPLETIONS_URL,
        "deepseek" => DEEPSEEK_UPSTREAM_CHAT_COMPLETIONS_URL,
        "kimi_code" => KIMI_UPSTREAM_CHAT_COMPLETIONS_URL,
        "xiaomi_mimo" => MIMO_UPSTREAM_CHAT_COMPLETIONS_URL,
        "custom" => DEEPSEEK_UPSTREAM_CHAT_COMPLETIONS_URL,
        _ => DEEPSEEK_UPSTREAM_CHAT_COMPLETIONS_URL,
    }
}

pub(super) fn with_timiai_headers(
    request: reqwest::RequestBuilder,
    api_key: &str,
    session_id: &str,
) -> reqwest::RequestBuilder {
    let key = timiai_authorization_header_value(api_key);
    request
        .header("Authorization", key.clone())
        .header("x-api-key", key)
        .header("X-Session-Id", session_id)
}

pub(super) fn timiai_authorization_header_value(api_key: &str) -> String {
    api_key.trim().to_string()
}

pub(super) fn timiai_authorization_log_state(api_key: &str) -> &'static str {
    let trimmed = api_key.trim();
    if trimmed.is_empty() {
        "empty"
    } else if trimmed
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("bearer "))
    {
        "bearer_value"
    } else {
        "raw_value"
    }
}

pub(super) fn upstream_messages_url(provider: &str) -> &'static str {
    match normalize_proxy_provider(provider) {
        "timiai" => TIMIAI_MESSAGES_URL,
        "commandcode" => COMMANDCODE_UPSTREAM_MESSAGES_URL,
        "kimi_code" => KIMI_UPSTREAM_MESSAGES_URL,
        "xiaomi_mimo" => MIMO_UPSTREAM_MESSAGES_URL,
        "custom" => KIMI_UPSTREAM_MESSAGES_URL,
        _ => KIMI_UPSTREAM_MESSAGES_URL,
    }
}

pub(super) fn should_bridge_anthropic_messages_to_chat_completions(
    provider: &str,
    model: &str,
) -> bool {
    match normalize_proxy_provider(provider) {
        "kimi_code" => false,
        "commandcode" | "deepseek" | "xiaomi_mimo" => !is_claude_family_model(model),
        "timiai" => !is_claude_family_model(model),
        _ => false,
    }
}

pub(super) fn upstream_native_anthropic_model<'a>(provider: &str, model: &'a str) -> &'a str {
    match (normalize_proxy_provider(provider), model) {
        ("xiaomi_mimo", "MiMo-V2.5-Pro") => "mimo-v2.5-pro",
        ("xiaomi_mimo", "MiMo-V2.5") => "mimo-v2.5",
        _ => model,
    }
}

pub(super) fn is_claude_family_model(model: &str) -> bool {
    let normalized = model.trim().to_ascii_lowercase();
    normalized.starts_with("claude-")
        || normalized.starts_with("anthropic/claude-")
        || normalized.contains("/claude-")
}

pub(super) fn upstream_chat_completion_model<'a>(provider: &str, model: &'a str) -> &'a str {
    match (normalize_proxy_provider(provider), model) {
        ("xiaomi_mimo", "MiMo-V2.5-Pro") => "mimo-v2.5-pro",
        ("xiaomi_mimo", "MiMo-V2.5") => "mimo-v2.5",
        _ => model,
    }
}
