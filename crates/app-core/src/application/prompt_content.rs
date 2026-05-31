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
