use std::sync::Arc;
use workspace_model::UserPromptContent;

pub(super) fn prompt_text(prompt: &[UserPromptContent]) -> Option<String> {
    let text = prompt
        .iter()
        .filter_map(|content| match content {
            UserPromptContent::Text { text } => Some(text.trim()),
            UserPromptContent::Image { .. }
            | UserPromptContent::File { .. }
            | UserPromptContent::WorkspaceFile { .. } => None,
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    if text.is_empty() { None } else { Some(text) }
}

pub(super) fn prompt_has_image(prompt: &[UserPromptContent]) -> bool {
    prompt
        .iter()
        .any(|content| matches!(content, UserPromptContent::Image { .. }))
}

pub(super) fn prompt_has_file(prompt: &[UserPromptContent]) -> bool {
    prompt.iter().any(|content| {
        matches!(
            content,
            UserPromptContent::File { .. } | UserPromptContent::WorkspaceFile { .. }
        )
    })
}

/// Degrade image attachments for a text-only model by calling the
/// configured multimodal view model ahead of time and injecting its
/// text description in place of the original image blocks.
///
/// This replaces the older "passive" approach that only appended a
/// `view_image` tool hint and relied on the agent to call the tool
/// itself. Now the interception is automatic: images are cached,
/// sent to the view model synchronously, and the resulting
/// description is injected into the prompt so the text-only model
/// never sees raw image content.
pub(super) fn degrade_prompt_for_image_fallback(
    prompt: &mut Vec<UserPromptContent>,
    config: &crate::image_mcp::ImageMcpConfig,
    view_cache: Arc<std::sync::Mutex<crate::image_api::ViewCache>>,
) {
    let mut attachments: Vec<(String, Option<String>)> = Vec::new();
    let mut kept: Vec<UserPromptContent> = Vec::with_capacity(prompt.len());
    for content in prompt.drain(..) {
        if let UserPromptContent::Image {
            display_url,
            name,
            ..
        } = content
        {
            if let Some(url) = display_url {
                attachments.push((url, name));
            }
        } else {
            kept.push(content);
        }
    }
    *prompt = kept;

    if attachments.is_empty() {
        return;
    }

    // If the view model is not configured, fall back to the old
    // passive tool-hint path so the agent can still try view_image.
    let view_model = config.settings.view.model.trim();
    let view_provider = config.settings.view.provider.trim();
    if view_model.is_empty() || view_provider.is_empty() {
        append_view_image_hint(prompt, &attachments);
        return;
    }

    let api = crate::image_api::ImageApi::new(config.clone(), view_cache);
    let results = run_view_images_sync(&api, &attachments);
    let mut descriptions: Vec<String> = Vec::new();
    for (result, (path, name)) in results.into_iter().zip(attachments.iter()) {
        let label = name.as_deref().unwrap_or("附件");
        match result {
            Ok(result) => {
                if let Some(desc) = result
                    .get("description")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    descriptions.push(format!("[图片 \"{label}\" 的识别结果]\n{desc}"));
                } else {
                    descriptions.push(format!(
                        "[图片 \"{label}\"]\n（识图模型未能返回有效描述，原始路径: {path}）"
                    ));
                }
            }
            Err(error) => {
                descriptions.push(format!(
                    "[图片 \"{label}\"]\n（识图调用失败: {error}，原始路径: {path}）"
                ));
            }
        }
    }

    let mut guidance = String::from(
        "[系统提示] 当前模型不支持直接查看图片，已自动通过识图模型识别以下附件的内容：\n\n",
    );
    for desc in &descriptions {
        guidance.push_str(desc);
        guidance.push_str("\n\n");
    }

    match prompt
        .iter_mut()
        .find(|content| matches!(content, UserPromptContent::Text { .. }))
    {
        Some(UserPromptContent::Text { text }) => {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&guidance);
        }
        _ => prompt.push(UserPromptContent::text(guidance)),
    }
}

/// Synchronous wrapper around `ImageApi::view_image`. Creates a temporary
/// single-thread tokio runtime once and reuses it for all images.
fn run_view_images_sync(
    api: &crate::image_api::ImageApi,
    attachments: &[(String, Option<String>)],
) -> Vec<Result<serde_json::Value, String>> {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            return attachments
                .iter()
                .map(|_| Err(format!("failed to create view runtime: {error}")))
                .collect();
        }
    };
    attachments
        .iter()
        .map(|(path, _name)| {
            let parsed = serde_json::json!({
                "image_path": path,
                "question": ""
            });
            runtime.block_on(api.view_image(&parsed))
        })
        .collect()
}

/// Fallback: append a `view_image` tool hint when the view model is not
/// configured (original passive behavior).
fn append_view_image_hint(
    prompt: &mut Vec<UserPromptContent>,
    attachments: &[(String, Option<String>)],
) {
    let mut guidance = String::from(
        "[系统提示] 当前模型不支持直接查看图片。已将附件缓存到本地。\n\
         如需理解图片内容，请调用 view_image 工具（如果可用），参数 image_path 使用下方提供的路径。\n\
         附件路径：",
    );
    for (url, label) in attachments {
        let label = label.as_deref().unwrap_or("附件");
        guidance.push_str(&format!("\n  - {url} （{label}）"));
    }

    match prompt
        .iter_mut()
        .find(|content| matches!(content, UserPromptContent::Text { .. }))
    {
        Some(UserPromptContent::Text { text }) => {
            text.push_str("\n\n");
            text.push_str(&guidance);
        }
        _ => prompt.push(UserPromptContent::text(guidance)),
    }
}

pub(super) fn prompt_display_body(prompt: &[UserPromptContent]) -> String {
    let parts = prompt
        .iter()
        .filter_map(|content| match content {
            UserPromptContent::Text { text } => {
                let text = text.trim();
                if text.is_empty() {
                    None
                } else {
                    Some(text.to_string())
                }
            }
            UserPromptContent::Image {
                name,
                display_url,
                thumbnail_data,
                thumbnail_mime_type,
                ..
            } => {
                let alt = markdown_image_alt(name.as_deref());
                if let Some(url) = display_url.as_deref().filter(|url| !url.trim().is_empty()) {
                    thumbnail_data.as_ref().map_or_else(
                        || Some(format!("![Image: {alt}]({url})")),
                        |data| {
                            let mime_type = thumbnail_mime_type.as_deref().unwrap_or("image/png");
                            Some(format!(
                                "![Image: {alt}](data:{mime_type};base64,{data} \"{url}\")"
                            ))
                        },
                    )
                } else {
                    thumbnail_data.as_ref().map_or_else(
                        || Some(format!("[Image: {alt}]")),
                        |data| {
                            let mime_type = thumbnail_mime_type.as_deref().unwrap_or("image/png");
                            Some(format!("![Image: {alt}](data:{mime_type};base64,{data})"))
                        },
                    )
                }
            }
            UserPromptContent::File { name, .. } => Some(format!("[File: {name}]")),
            UserPromptContent::WorkspaceFile {
                path,
                start_line,
                end_line,
            } => Some(workspace_reference_mention(path, *start_line, *end_line)),
        })
        .collect::<Vec<_>>();
    parts.join("\n\n")
}

fn markdown_image_alt(name: Option<&str>) -> String {
    name.unwrap_or("attached image")
        .replace(['\n', '\r', '[', ']'], " ")
        .trim()
        .to_string()
}

fn workspace_reference_mention(
    path: &str,
    start_line: Option<u32>,
    end_line: Option<u32>,
) -> String {
    let normalized = path.replace('\\', "/").trim_start_matches('/').to_string();
    match normalize_reference_range(start_line, end_line) {
        Some((start, end)) if start == end => format!("@{normalized}#L{start}"),
        Some((start, end)) => format!("@{normalized}#L{start}-L{end}"),
        None => format!("@{normalized}"),
    }
}

fn normalize_reference_range(start_line: Option<u32>, end_line: Option<u32>) -> Option<(u32, u32)> {
    let start = start_line.filter(|line| *line > 0)?;
    let end = end_line.filter(|line| *line > 0).unwrap_or(start);
    Some((start.min(end), start.max(end)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_with_url(name: &str, url: &str) -> UserPromptContent {
        UserPromptContent::Image {
            data: String::new(),
            mime_type: "image/png".to_string(),
            name: Some(name.to_string()),
            display_url: Some(url.to_string()),
            thumbnail_data: None,
            thumbnail_mime_type: None,
        }
    }

    fn unconfigured_config() -> crate::image_mcp::ImageMcpConfig {
        crate::image_mcp::ImageMcpConfig {
            workspace_root: std::env::temp_dir(),
            settings: workspace_model::ImageSettings::default(),
            view_api_key: None,
            generate_api_key: None,
        }
    }

    #[test]
    fn fallback_hint_when_view_model_not_configured() {
        let mut prompt = vec![
            UserPromptContent::text("这张图里是什么"),
            image_with_url("cat.png", "file:///tmp/cat.png"),
        ];
        degrade_prompt_for_image_fallback(
            &mut prompt,
            &unconfigured_config(),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        assert!(prompt.iter().all(|c| !matches!(c, UserPromptContent::Image { .. })));
        let text = prompt_text(&prompt).expect("text remains");
        assert!(text.contains("这张图里是什么"));
        assert!(text.contains("view_image"));
        assert!(text.contains("file:///tmp/cat.png"));
    }

    #[test]
    fn plain_text_prompt_passes_through_unchanged() {
        let mut prompt = vec![UserPromptContent::text("帮我画一只猫")];
        let before = prompt_text(&prompt).unwrap();
        degrade_prompt_for_image_fallback(
            &mut prompt,
            &unconfigured_config(),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        assert_eq!(prompt_text(&prompt).as_deref(), Some(before.as_str()));
    }

    #[test]
    fn uncached_image_without_display_url_is_dropped_silently() {
        let mut prompt = vec![
            UserPromptContent::text("描述这张图"),
            UserPromptContent::Image {
                data: String::new(),
                mime_type: "image/png".to_string(),
                name: Some("lost.png".to_string()),
                display_url: None,
                thumbnail_data: None,
                thumbnail_mime_type: None,
            },
        ];
        degrade_prompt_for_image_fallback(
            &mut prompt,
            &unconfigured_config(),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        assert!(prompt.iter().all(|c| !matches!(c, UserPromptContent::Image { .. })));
        let text = prompt_text(&prompt).expect("text remains");
        assert!(!text.contains("view_image"));
    }
}
