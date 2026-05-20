use agent_client_protocol::schema::{
    BlobResourceContents, ContentBlock, EmbeddedResource, EmbeddedResourceResource, ImageContent,
    TextContent, TextResourceContents,
};
use workspace_model::{PromptInputCapabilities, UserPromptContent};

pub(super) fn prompt_content_to_acp(content: UserPromptContent) -> Option<ContentBlock> {
    match content {
        UserPromptContent::Text { text } => {
            let text = text.trim().to_string();
            if text.is_empty() {
                None
            } else {
                Some(ContentBlock::Text(TextContent::new(text)))
            }
        }
        UserPromptContent::Image {
            data, mime_type, ..
        } => Some(ContentBlock::Image(ImageContent::new(data, mime_type))),
        UserPromptContent::File {
            data,
            text,
            mime_type,
            name,
            uri,
        } => {
            let uri = uri.unwrap_or_else(|| attachment_uri(&name));
            if let Some(text) = text {
                Some(ContentBlock::Resource(EmbeddedResource::new(
                    EmbeddedResourceResource::TextResourceContents(
                        TextResourceContents::new(text, uri).mime_type(mime_type),
                    ),
                )))
            } else if let Some(data) = data {
                Some(ContentBlock::Resource(EmbeddedResource::new(
                    EmbeddedResourceResource::BlobResourceContents(
                        BlobResourceContents::new(data, uri).mime_type(mime_type),
                    ),
                )))
            } else {
                None
            }
        }
    }
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
    prompt
        .iter()
        .any(|content| matches!(content, UserPromptContent::File { .. }))
}

pub(super) fn prompt_capabilities_from_acp(
    capabilities: &agent_client_protocol::schema::PromptCapabilities,
) -> PromptInputCapabilities {
    PromptInputCapabilities {
        image: capabilities.image,
        embedded_context: capabilities.embedded_context,
    }
}
