use agent_client_protocol::schema::{
    BlobResourceContents, ContentBlock, EmbeddedResource, EmbeddedResourceResource, ImageContent,
    TextContent, TextResourceContents,
};
use anyhow::Context;
use std::path::{Path, PathBuf};
use workspace_model::{PromptInputCapabilities, UserPromptContent};

use super::workspace_paths::validate_workspace_path;

pub(super) fn prompt_content_to_acp(
    content: UserPromptContent,
    workspace_root: &str,
) -> anyhow::Result<Vec<ContentBlock>> {
    match content {
        UserPromptContent::Text { text } => {
            let text = text.trim().to_string();
            if text.is_empty() {
                Ok(Vec::new())
            } else {
                Ok(vec![ContentBlock::Text(TextContent::new(text))])
            }
        }
        UserPromptContent::Image {
            data, mime_type, ..
        } => Ok(vec![ContentBlock::Image(ImageContent::new(
            data, mime_type,
        ))]),
        UserPromptContent::File {
            data,
            text,
            mime_type,
            name,
            uri,
        } => {
            let uri = uri.unwrap_or_else(|| attachment_uri(&name));
            if let Some(text) = text {
                Ok(vec![ContentBlock::Resource(EmbeddedResource::new(
                    EmbeddedResourceResource::TextResourceContents(
                        TextResourceContents::new(text, uri).mime_type(mime_type),
                    ),
                ))])
            } else if let Some(data) = data {
                Ok(vec![ContentBlock::Resource(EmbeddedResource::new(
                    EmbeddedResourceResource::BlobResourceContents(
                        BlobResourceContents::new(data, uri).mime_type(mime_type),
                    ),
                ))])
            } else {
                Ok(Vec::new())
            }
        }
        UserPromptContent::WorkspaceFile {
            path,
            start_line,
            end_line,
        } => workspace_reference_to_acp(workspace_root, &path, start_line, end_line),
    }
}

pub(super) fn prompt_title_text(prompt: &[UserPromptContent]) -> Option<String> {
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

pub(super) fn attachment_uri(name: &str) -> String {
    let safe_name = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let safe_name = safe_name.trim_matches('_');
    format!(
        "attachment://{}",
        if safe_name.is_empty() {
            "file"
        } else {
            safe_name
        }
    )
}

pub(super) fn prompt_contains_image(prompt: &[UserPromptContent]) -> bool {
    prompt
        .iter()
        .any(|content| matches!(content, UserPromptContent::Image { .. }))
}

pub(super) fn prompt_contains_file(prompt: &[UserPromptContent]) -> bool {
    prompt.iter().any(|content| {
        matches!(
            content,
            UserPromptContent::File { .. } | UserPromptContent::WorkspaceFile { .. }
        )
    })
}

pub(super) fn prompt_capabilities_from_acp(
    capabilities: &agent_client_protocol::schema::PromptCapabilities,
) -> PromptInputCapabilities {
    PromptInputCapabilities {
        image: capabilities.image,
        embedded_context: capabilities.embedded_context,
    }
}

fn workspace_reference_to_acp(
    workspace_root: &str,
    path: &str,
    start_line: Option<u32>,
    end_line: Option<u32>,
) -> anyhow::Result<Vec<ContentBlock>> {
    let requested_path = Path::new(path);
    let resolved = validate_workspace_path(workspace_root, requested_path)
        .with_context(|| format!("failed to resolve referenced file {path}"))?;
    if !resolved.is_file() {
        anyhow::bail!("referenced path is not a file: {path}");
    }

    let content = std::fs::read_to_string(&resolved)
        .with_context(|| format!("failed to read referenced file {}", resolved.display()))?;
    let range = normalize_reference_range(start_line, end_line);
    let selected = select_reference_lines(&content, range);
    let mention = workspace_reference_mention(path, range);
    let uri = workspace_file_uri(&resolved, range);
    let mime_type = Some(mime_type_for_path(path).to_string());

    Ok(vec![
        ContentBlock::Text(TextContent::new(mention)),
        ContentBlock::Resource(EmbeddedResource::new(
            EmbeddedResourceResource::TextResourceContents(
                TextResourceContents::new(selected, uri).mime_type(mime_type),
            ),
        )),
    ])
}

fn workspace_reference_mention(path: &str, range: Option<(u32, u32)>) -> String {
    let normalized = path.replace('\\', "/").trim_start_matches('/').to_string();
    match range {
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

fn select_reference_lines(content: &str, range: Option<(u32, u32)>) -> String {
    let Some((start, end)) = range else {
        return content.to_string();
    };

    let start_index = start.saturating_sub(1) as usize;
    let line_count = end.saturating_sub(start).saturating_add(1) as usize;
    content
        .lines()
        .skip(start_index)
        .take(line_count)
        .collect::<Vec<_>>()
        .join("\n")
}

fn workspace_file_uri(path: &Path, range: Option<(u32, u32)>) -> String {
    let fragment = match range {
        Some((start, end)) if start == end => format!("#L{start}"),
        Some((start, end)) => format!("#L{start}-L{end}"),
        None => String::new(),
    };
    format!("{}{}", path_to_file_uri(path), fragment)
}

fn path_to_file_uri(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    let prefixed = if normalized.starts_with('/') {
        format!("file://{normalized}")
    } else {
        format!("file:///{normalized}")
    };
    percent_encode_uri(&prefixed)
}

fn percent_encode_uri(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' | b'/' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn mime_type_for_path(path: &str) -> &'static str {
    match PathBuf::from(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("md") | Some("mdx") => "text/markdown",
        Some("html") => "text/html",
        Some("css") => "text/css",
        Some("json") => "application/json",
        Some("ts") | Some("tsx") | Some("js") | Some("jsx") | Some("mjs") | Some("cjs") => {
            "text/javascript"
        }
        _ => "text/plain",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn workspace_file_reference_expands_to_mention_and_resource() {
        let root = temp_workspace("reference");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "line 1\nline 2\nline 3\n").unwrap();

        let blocks = prompt_content_to_acp(
            UserPromptContent::workspace_file("src/lib.rs", Some(2), Some(3)),
            root.to_str().unwrap(),
        )
        .unwrap();

        assert_eq!(blocks.len(), 2);
        match &blocks[0] {
            ContentBlock::Text(text) => assert_eq!(text.text, "@src/lib.rs#L2-L3"),
            other => panic!("expected text mention, got {other:?}"),
        }
        match &blocks[1] {
            ContentBlock::Resource(resource) => match &resource.resource {
                EmbeddedResourceResource::TextResourceContents(contents) => {
                    assert_eq!(contents.text, "line 2\nline 3");
                    assert!(contents.uri.ends_with("src/lib.rs#L2-L3"));
                    assert_eq!(contents.mime_type.as_deref(), Some("text/plain"));
                }
                other => panic!("expected text resource, got {other:?}"),
            },
            other => panic!("expected resource, got {other:?}"),
        }

        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn workspace_file_reference_rejects_parent_escape() {
        let root = temp_workspace("escape");
        let err = prompt_content_to_acp(
            UserPromptContent::workspace_file("../outside.txt", None, None),
            root.to_str().unwrap(),
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("failed to resolve referenced file")
        );
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    fn temp_workspace(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir()
            .join(format!("kodex-acp-prompt-content-{label}-{unique}"))
            .join("workspace");
        fs::create_dir_all(&root).unwrap();
        root
    }
}
