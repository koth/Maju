/// Extract a concise session title from the user's first prompt.
/// Takes the first line, strips common prefixes, and truncates to 60 chars.
pub(super) fn extract_title_from_prompt(prompt: &str) -> String {
    let first_line = prompt.lines().next().unwrap_or(prompt).trim();

    // Strip common conversational prefixes
    let stripped = first_line
        .strip_prefix("Please ")
        .or_else(|| first_line.strip_prefix("please "))
        .or_else(|| first_line.strip_prefix("Help me "))
        .or_else(|| first_line.strip_prefix("help me "))
        .or_else(|| first_line.strip_prefix("Can you "))
        .or_else(|| first_line.strip_prefix("can you "))
        .or_else(|| first_line.strip_prefix("I want to "))
        .or_else(|| first_line.strip_prefix("I need to "))
        .unwrap_or(first_line)
        .trim();

    let text = if stripped.is_empty() {
        first_line
    } else {
        stripped
    };

    if text.chars().count() <= 60 {
        text.to_string()
    } else {
        let truncated: String = text.chars().take(57).collect();
        format!("{truncated}...")
    }
}

pub(super) fn is_placeholder_session_title(title: &str) -> bool {
    matches!(
        title.trim(),
        "" | "新会话"
            | "新 ACP 会话"
            | "新聊天"
            | "新对话"
            | "未命名会话"
            | "无标题"
            | "New Session"
            | "New Chat"
            | "New Conversation"
            | "Untitled"
            | "Untitled Session"
            | "Untitled Chat"
            | "Untitled Conversation"
    )
}

/// Try to extract a refined title from the assistant's first response.
/// Returns None if no good title can be extracted (keeps the prompt-based title).
pub(super) fn extract_title_from_response(response: &str) -> Option<String> {
    // Get the first meaningful line (skip empty lines and markdown headers)
    let first_line = response
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("```"))?;

    // Strip common assistant prefixes to get the action description
    let prefixes = [
        "I'll help you ",
        "I'll ",
        "I will ",
        "Let me ",
        "Sure, I'll ",
        "Sure! I'll ",
        "OK, I'll ",
        "Alright, I'll ",
        "Here's how to ",
        "I can help with ",
        "I can help you ",
        // Chinese prefixes
        "我来帮你",
        "让我",
        "好的，我来",
        "好的，让我",
        "我会",
        "我将",
    ];

    let mut text = first_line;
    for prefix in prefixes {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest;
            break;
        }
    }

    let text = text.trim_end_matches('.');
    let text = text.trim();

    // If too short or same as just a function word, not useful
    if text.len() < 5 {
        return None;
    }

    // Capitalize first letter
    let title = if text.starts_with(|c: char| c.is_lowercase()) {
        let mut chars = text.chars();
        match chars.next() {
            Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
            None => return None,
        }
    } else {
        text.to_string()
    };

    // Truncate to 60 chars
    if title.chars().count() <= 60 {
        Some(title)
    } else {
        let truncated: String = title.chars().take(57).collect();
        Some(format!("{truncated}..."))
    }
}
