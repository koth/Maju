use workspace_model::UserPromptContent;

pub(super) fn prompt_text(prompt: &[UserPromptContent]) -> Option<String> {
    let text = prompt
        .iter()
        .filter_map(|content| match content {
            UserPromptContent::Text { text } => Some(text.trim()),
            UserPromptContent::Image { .. } | UserPromptContent::File { .. } => None,
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
    prompt
        .iter()
        .any(|content| matches!(content, UserPromptContent::File { .. }))
}

pub(super) fn prompt_display_body(prompt: &[UserPromptContent]) -> String {
    let mut parts = Vec::new();
    if let Some(text) = prompt_text(prompt) {
        parts.push(text);
    }
    parts.extend(prompt.iter().filter_map(|content| match content {
        UserPromptContent::Image {
            name,
            thumbnail_data,
            thumbnail_mime_type,
            ..
        } => {
            let alt = markdown_image_alt(name.as_deref());
            thumbnail_data.as_ref().map_or_else(
                || Some(format!("[Image: {alt}]")),
                |data| {
                    let mime_type = thumbnail_mime_type.as_deref().unwrap_or("image/png");
                    Some(format!("![Image: {alt}](data:{mime_type};base64,{data})"))
                },
            )
        }
        UserPromptContent::File { name, .. } => Some(format!("[File: {name}]")),
        UserPromptContent::Text { .. } => None,
    }));
    parts.join("\n\n")
}

fn markdown_image_alt(name: Option<&str>) -> String {
    name.unwrap_or("attached image")
        .replace(['\n', '\r', '[', ']'], " ")
        .trim()
        .to_string()
}
