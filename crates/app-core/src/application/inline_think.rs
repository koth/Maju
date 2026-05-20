#[derive(Debug, Default)]
pub(super) struct InlineThinkFilter {
    in_think: bool,
    pending: String,
}

impl InlineThinkFilter {
    pub(super) fn reset(&mut self) {
        self.in_think = false;
        self.pending.clear();
    }

    pub(super) fn filter_chunk(&mut self, chunk: &str) -> Option<String> {
        if chunk.is_empty() && self.pending.is_empty() {
            return None;
        }

        let mut text = String::new();
        if !self.pending.is_empty() {
            text.push_str(&self.pending);
            self.pending.clear();
        }
        text.push_str(chunk);

        let mut visible = String::new();
        let mut cursor = 0;

        while cursor < text.len() {
            if self.in_think {
                if let Some(close_at) = find_ascii_case_insensitive(&text[cursor..], "</think>") {
                    cursor += close_at + "</think>".len();
                    self.in_think = false;
                } else {
                    let suffix_len = trailing_tag_prefix_len(&text[cursor..], "</think>");
                    if suffix_len > 0 {
                        self.pending = text[text.len() - suffix_len..].to_string();
                    }
                    break;
                }
            } else if let Some(open_at) = find_ascii_case_insensitive(&text[cursor..], "<think>") {
                let open_start = cursor + open_at;
                visible.push_str(&text[cursor..open_start]);
                cursor = open_start + "<think>".len();
                self.in_think = true;
            } else {
                let suffix_len = trailing_tag_prefix_len(&text[cursor..], "<think>");
                let emit_end = text.len() - suffix_len;
                visible.push_str(&text[cursor..emit_end]);
                if suffix_len > 0 {
                    self.pending = text[emit_end..].to_string();
                }
                break;
            }
        }

        (!visible.is_empty()).then_some(visible)
    }

    pub(super) fn flush(&mut self) -> Option<String> {
        let visible = if self.in_think {
            None
        } else if self.pending.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.pending))
        };
        self.reset();
        visible
    }
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

fn trailing_tag_prefix_len(text: &str, tag: &str) -> usize {
    let lower = text.to_ascii_lowercase();
    let tag = tag.to_ascii_lowercase();
    (1..tag.len())
        .rev()
        .find(|len| lower.ends_with(&tag[..*len]))
        .unwrap_or(0)
}
