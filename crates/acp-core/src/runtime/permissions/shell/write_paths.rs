use super::*;

pub(in crate::runtime::permissions) fn shell_command_directly_mutates_files(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    if contains_command_token(&lower, "apply_patch") {
        return false;
    }
    shell_redirection_writes_file(command)
        || contains_command_token(&lower, "tee")
        || contains_command_token(&lower, "truncate")
        || contains_command_token(&lower, "touch")
        || contains_command_token(&lower, "rm")
        || contains_command_token(&lower, "mv")
        || contains_command_token(&lower, "cp")
        || contains_command_token(&lower, "set-content")
        || contains_command_token(&lower, "add-content")
        || contains_command_token(&lower, "out-file")
        || contains_command_token(&lower, "remove-item")
        || contains_command_token(&lower, "move-item")
        || contains_command_token(&lower, "copy-item")
        || (contains_command_token(&lower, "new-item")
            && lower.contains("-itemtype")
            && lower.contains("file"))
        || (contains_command_token(&lower, "sed") && lower.contains(" -i"))
        || (contains_command_token(&lower, "perl") && lower.contains(" -pi"))
        || lower.contains(".write_text(")
        || lower.contains(".write_bytes(")
        || python_open_uses_write_mode(command)
        || lower.contains("writefile")
        || lower.contains("writefilesync")
}

fn contains_command_token(text: &str, token: &str) -> bool {
    find_command_token(text, token).is_some()
}

fn find_command_token(text: &str, token: &str) -> Option<usize> {
    let mut offset = 0;
    while let Some(index) = text[offset..].find(token) {
        let index = offset + index;
        let before = text[..index].chars().next_back();
        let after = text[index + token.len()..].chars().next();
        if !before.is_some_and(is_command_word_char) && !after.is_some_and(is_command_word_char) {
            return Some(index);
        }
        offset = index + token.len();
    }
    None
}

fn is_command_word_char(value: char) -> bool {
    value.is_ascii_alphanumeric() || matches!(value, '_' | '-')
}

fn shell_redirection_writes_file(command: &str) -> bool {
    let command = strip_shell_here_documents(command);
    let command = command.as_str();
    let bytes = command.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'>' {
            index += 1;
            continue;
        }
        if index > 0 && bytes[index - 1].is_ascii_digit() {
            index += 1;
            continue;
        }
        let mut target_start = index + 1;
        if target_start < bytes.len() && bytes[target_start] == b'>' {
            target_start += 1;
        }
        if let Some(target) = shell_redirection_target(command, target_start)
            && !is_null_redirection_target(&target)
            && !target.starts_with('&')
        {
            return true;
        }
        index = target_start;
    }
    false
}

fn shell_redirection_target(command: &str, start: usize) -> Option<String> {
    let mut index = start;
    let chars = command.as_bytes();
    while index < chars.len() && chars[index].is_ascii_whitespace() {
        index += 1;
    }
    if index >= chars.len() {
        return None;
    }

    let quote = chars[index];
    if quote == b'\'' || quote == b'"' {
        let mut end = index + 1;
        while end < chars.len() && chars[end] != quote {
            end += 1;
        }
        return Some(command[index + 1..end].trim().to_string());
    }

    let mut end = index;
    while end < chars.len()
        && !chars[end].is_ascii_whitespace()
        && !matches!(chars[end], b';' | b'|')
    {
        end += 1;
    }
    Some(command[index..end].trim().to_string()).filter(|target| !target.is_empty())
}

pub(super) fn is_null_redirection_target(target: &str) -> bool {
    matches!(
        target.trim().to_ascii_lowercase().as_str(),
        "/dev/null" | "$null" | "nul" | "null"
    )
}

pub(in crate::runtime::permissions) fn extract_write_paths_from_command_text(
    command: &str,
) -> Vec<String> {
    let command = strip_powershell_here_strings(command);
    let shell_redirection_command = strip_shell_here_documents(&command);
    let mut paths = Vec::new();
    collect_shell_redirection_paths(&shell_redirection_command, &mut paths);
    collect_powershell_write_cmdlet_paths(&command, &mut paths);
    collect_python_pathlib_write_paths(&command, &mut paths);
    collect_python_open_write_paths(&command, &mut paths);
    collect_common_mutation_command_paths(&command, &mut paths);
    paths.retain(|path| is_usable_write_path(path));
    paths.sort();
    paths.dedup();
    paths
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ShellHereDocMarker {
    delimiter: String,
    strip_tabs: bool,
}

fn strip_shell_here_documents(command: &str) -> String {
    let mut output = Vec::new();
    let mut pending = Vec::<ShellHereDocMarker>::new();

    for line in command.replace("\r\n", "\n").split('\n') {
        if pending.is_empty() {
            output.push(line.to_string());
            pending.extend(extract_shell_here_doc_markers(line));
            continue;
        }

        let active = &pending[0];
        let comparable = if active.strip_tabs {
            line.trim_start_matches('\t')
        } else {
            line
        };
        if comparable == active.delimiter {
            pending.remove(0);
        }
    }

    output.join("\n")
}

fn extract_shell_here_doc_markers(line: &str) -> Vec<ShellHereDocMarker> {
    let mut markers = Vec::new();
    let mut index = 0;
    while let Some(relative) = line[index..].find("<<") {
        let marker_index = index + relative;
        let mut cursor = marker_index + 2;
        if line[cursor..].starts_with('<') {
            index = cursor + 1;
            continue;
        }

        let strip_tabs = line[cursor..].starts_with('-');
        if strip_tabs {
            cursor += 1;
        }
        while let Some(ch) = line[cursor..].chars().next() {
            if !ch.is_whitespace() {
                break;
            }
            cursor += ch.len_utf8();
        }

        if let Some((delimiter, end)) = parse_shell_here_doc_delimiter(line, cursor) {
            markers.push(ShellHereDocMarker {
                delimiter,
                strip_tabs,
            });
            index = end;
        } else {
            index = cursor.saturating_add(1);
        }
    }
    markers
}

fn parse_shell_here_doc_delimiter(line: &str, start: usize) -> Option<(String, usize)> {
    let first = line[start..].chars().next()?;
    if first == '\'' || first == '"' {
        let body_start = start + first.len_utf8();
        let body = &line[body_start..];
        for (offset, ch) in body.char_indices() {
            if ch == first {
                let delimiter = body[..offset].trim().to_string();
                return (!delimiter.is_empty()).then_some((delimiter, body_start + offset + 1));
            }
        }
        return None;
    }

    let end = line[start..]
        .char_indices()
        .find_map(|(offset, ch)| {
            (ch.is_whitespace() || matches!(ch, ';' | '|' | '&' | '<' | '>')).then_some(offset)
        })
        .map(|offset| start + offset)
        .unwrap_or(line.len());
    let delimiter = line[start..end].replace('\\', "").trim().to_string();
    (!delimiter.is_empty()).then_some((delimiter, end))
}

fn strip_powershell_here_strings(command: &str) -> String {
    let mut output = String::with_capacity(command.len());
    let mut index = 0;
    while index < command.len() {
        let rest = &command[index..];
        let Some((quote, marker_len)) = rest
            .strip_prefix("@\"")
            .map(|_| ('"', 2))
            .or_else(|| rest.strip_prefix("@'").map(|_| ('\'', 2)))
        else {
            let Some(ch) = rest.chars().next() else {
                break;
            };
            output.push(ch);
            index += ch.len_utf8();
            continue;
        };

        index += marker_len;
        let end_marker_lf = format!("\n{quote}@");
        let end_marker_crlf = format!("\r\n{quote}@");
        let remainder = &command[index..];
        let end_lf = remainder.find(&end_marker_lf);
        let end_crlf = remainder.find(&end_marker_crlf);
        let end = match (end_lf, end_crlf) {
            (Some(lf), Some(crlf)) => Some(lf.min(crlf)),
            (Some(lf), None) => Some(lf),
            (None, Some(crlf)) => Some(crlf),
            (None, None) => None,
        };
        if let Some(end) = end {
            index += end;
            let tail = &command[index..];
            if tail.starts_with(&end_marker_crlf) {
                index += end_marker_crlf.len();
            } else {
                index += end_marker_lf.len();
            }
            output.push(' ');
        } else {
            break;
        }
    }
    output
}

fn collect_shell_redirection_paths(command: &str, paths: &mut Vec<String>) {
    let mut previous = '\0';
    let mut chars = command.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if ch != '>' || previous.is_ascii_digit() {
            previous = ch;
            continue;
        }
        let mut target_start = index + ch.len_utf8();
        if matches!(chars.peek(), Some((_, '>'))) {
            if let Some((next_index, next_ch)) = chars.next() {
                target_start = next_index + next_ch.len_utf8();
            }
        }
        if let Some(value) = parse_command_value_at(command, target_start)
            && looks_like_standalone_path(&value)
        {
            paths.push(value);
        }
        previous = ch;
    }
}

fn collect_powershell_write_cmdlet_paths(command: &str, paths: &mut Vec<String>) {
    for segment in command.split([';', '\n']) {
        let lower = segment.to_ascii_lowercase();
        if contains_command_token(&lower, "set-content")
            || contains_command_token(&lower, "add-content")
        {
            paths.extend(extract_param_values(
                segment,
                &["-literalpath", "-filepath", "-path"],
            ));
            paths.extend(extract_positional_write_path_values(
                segment,
                &["set-content", "add-content"],
            ));
        } else if contains_command_token(&lower, "out-file") {
            paths.extend(extract_param_values(segment, &["-filepath", "-path"]));
            paths.extend(extract_positional_write_path_values(segment, &["out-file"]));
        } else if contains_command_token(&lower, "new-item")
            && has_param_value(&lower, "-itemtype", "file")
        {
            paths.extend(extract_param_values(segment, &["-literalpath", "-path"]));
            paths.extend(extract_positional_write_path_values(segment, &["new-item"]));
        }
    }
}

fn collect_common_mutation_command_paths(command: &str, paths: &mut Vec<String>) {
    for segment in split_shell_segments(command) {
        let words = shell_words(&segment);
        let Some(command_word) = shell_command_word(&words) else {
            continue;
        };
        let command = shell_command_basename(command_word);
        let args = words
            .iter()
            .skip_while(|word| word.as_str() != command_word)
            .skip(1)
            .map(String::as_str)
            .collect::<Vec<_>>();

        match command.as_str() {
            "mkdir" | "touch" | "rm" | "rmdir" | "del" | "erase" | "remove-item" => {
                paths.extend(command_path_args(&args));
            }
            "mv" | "move" | "move-item" | "cp" | "copy" | "copy-item" => {
                paths.extend(command_path_args(&args));
            }
            "git" => {
                if let Some(subcommand) = args
                    .iter()
                    .find(|arg| !arg.starts_with('-'))
                    .map(|arg| arg.to_ascii_lowercase())
                    && matches!(
                        subcommand.as_str(),
                        "add" | "checkout" | "restore" | "reset" | "apply" | "commit"
                    )
                {
                    paths.extend(command_path_args(&args));
                }
            }
            _ => {}
        }
    }
}

fn split_shell_segments(command: &str) -> Vec<String> {
    let mut segments = Vec::new();
    for pipeline in split_shell_pipeline(command) {
        for segment in pipeline.split([';', '\n']) {
            for part in segment.split("&&") {
                for item in part.split("||") {
                    let item = item.trim();
                    if !item.is_empty() {
                        segments.push(item.to_string());
                    }
                }
            }
        }
    }
    segments
}

fn command_path_args(args: &[&str]) -> Vec<String> {
    let mut paths = Vec::new();
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        let arg = arg.trim();
        if arg.is_empty() {
            continue;
        }
        if arg == "--" {
            continue;
        }
        if arg.starts_with('-') {
            if powershell_param_takes_value(arg) {
                skip_next = true;
            }
            continue;
        }
        if looks_like_standalone_path(arg) {
            paths.push(arg.to_string());
        }
    }
    paths
}

fn extract_positional_write_path_values(segment: &str, commands: &[&str]) -> Vec<String> {
    let lower = segment.to_ascii_lowercase();
    let mut values = Vec::new();
    for command in commands {
        let Some(index) = find_command_token(&lower, command) else {
            continue;
        };
        let args = &segment[index + command.len()..];
        let mut skip_next_value = false;
        for token in tokenize_command_args(args) {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            if skip_next_value {
                skip_next_value = false;
                continue;
            }
            if token.starts_with('-') {
                if powershell_param_takes_value(token) {
                    skip_next_value = true;
                }
                continue;
            }
            if looks_like_standalone_path(token) {
                values.push(token.to_string());
            }
            break;
        }
    }
    values
}

fn powershell_param_takes_value(param: &str) -> bool {
    matches!(
        param.to_ascii_lowercase().as_str(),
        "-path"
            | "-literalpath"
            | "-filepath"
            | "-value"
            | "-encoding"
            | "-itemtype"
            | "-name"
            | "-destination"
            | "-destinationpath"
    )
}

fn tokenize_command_args(args: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut chars = args.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '`' {
            if let Some(next) = chars.next() {
                current.push(next);
            }
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        if ch == '"' || ch == '\'' {
            quote = Some(ch);
            continue;
        }
        if ch.is_whitespace() || matches!(ch, '|' | ';' | ')') {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            if matches!(ch, '|' | ';') {
                break;
            }
            continue;
        }
        current.push(ch);
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn has_param_value(segment_lower: &str, param: &str, expected: &str) -> bool {
    extract_param_values(segment_lower, &[param])
        .iter()
        .any(|value| value.eq_ignore_ascii_case(expected))
}

fn extract_param_values(segment: &str, params: &[&str]) -> Vec<String> {
    let lower = segment.to_ascii_lowercase();
    let mut values = Vec::new();
    for param in params {
        let mut offset = 0;
        while let Some(relative) = lower[offset..].find(param) {
            let index = offset + relative;
            let before = lower[..index].chars().next_back();
            let after = lower[index + param.len()..].chars().next();
            let before_ok = before.map_or(true, |ch| ch.is_whitespace() || ch == '|');
            let after_ok = after.map_or(true, |ch| ch.is_whitespace() || ch == ':');
            if before_ok
                && after_ok
                && let Some(value) = parse_command_value_at(segment, index + param.len())
            {
                values.push(value);
            }
            offset = index + param.len();
        }
    }
    values
}

fn parse_command_value_at(text: &str, start: usize) -> Option<String> {
    let rest = &text[start..];
    let mut offset = 0;
    for (idx, ch) in rest.char_indices() {
        if ch.is_whitespace() || ch == ':' {
            offset = idx + ch.len_utf8();
            continue;
        }
        offset = idx;
        break;
    }

    let value = &rest[offset..];
    let first = value.chars().next()?;
    if first == '"' || first == '\'' {
        let quote = first;
        let body = &value[first.len_utf8()..];
        for (idx, ch) in body.char_indices() {
            if ch == quote {
                return Some(body[..idx].to_string());
            }
        }
        return Some(body.to_string());
    }

    let end = value
        .char_indices()
        .find_map(|(idx, ch)| {
            if ch.is_whitespace() || matches!(ch, ';' | '|' | ')') {
                Some(idx)
            } else {
                None
            }
        })
        .unwrap_or(value.len());
    Some(value[..end].to_string())
}

fn collect_python_pathlib_write_paths(command: &str, paths: &mut Vec<String>) {
    if !command.contains("write_text(") && !command.contains("write_bytes(") {
        return;
    }

    let mut offset = 0;
    while let Some(index) = find_next_python_path_call(command, offset) {
        if let Some((path, end)) = parse_python_path_call_at(command, index) {
            let after = command[end..].trim_start();
            if after.starts_with(".write_text(") || after.starts_with(".write_bytes(") {
                paths.push(path);
            }
            offset = end;
        } else {
            offset = index + 1;
        }
    }

    for (name, path) in python_pathlib_assignments(command) {
        if contains_python_method_call(command, &name, "write_text")
            || contains_python_method_call(command, &name, "write_bytes")
        {
            paths.push(path);
        }
    }
}

fn collect_python_open_write_paths(command: &str, paths: &mut Vec<String>) {
    let mut offset = 0;
    while let Some(index) = find_next_python_open_call(command, offset) {
        if let Some((path, end)) = parse_python_open_write_call_at(command, index) {
            paths.push(path);
            offset = end;
        } else {
            offset = index + 1;
        }
    }
}

fn python_open_uses_write_mode(command: &str) -> bool {
    let mut offset = 0;
    while let Some(index) = find_next_python_open_call(command, offset) {
        if let Some((_mode, _end)) = parse_python_open_write_mode_at(command, index) {
            return true;
        }
        offset = index + 1;
    }
    false
}

fn find_next_python_open_call(command: &str, start: usize) -> Option<usize> {
    let open = command[start..].find("open(").map(|index| start + index);
    let io_open = command[start..].find("io.open(").map(|index| start + index);
    [open, io_open]
        .into_iter()
        .flatten()
        .filter(|index| {
            let rest = &command[*index..];
            if rest.starts_with("io.open(") {
                return true;
            }
            command[..*index]
                .chars()
                .next_back()
                .map_or(true, |ch| !is_python_identifier_char(ch) && ch != '.')
        })
        .min()
}

fn parse_python_open_write_call_at(text: &str, start: usize) -> Option<(String, usize)> {
    let rest = &text[start..];
    let arg_start = if rest.starts_with("io.open(") {
        start + "io.open(".len()
    } else if rest.starts_with("open(") {
        start + "open(".len()
    } else {
        return None;
    };
    let (path, path_end) = parse_python_string_literal_at(text, arg_start)?;
    let comma = skip_ascii_whitespace(text, path_end);
    if !text[comma..].starts_with(',') {
        return None;
    }
    let (mode, mode_end) = parse_python_string_literal_at(text, comma + 1)?;
    if python_file_mode_can_write(&mode) {
        Some((path, mode_end))
    } else {
        None
    }
}

fn parse_python_open_write_mode_at(text: &str, start: usize) -> Option<(String, usize)> {
    let rest = &text[start..];
    let arg_start = if rest.starts_with("io.open(") {
        start + "io.open(".len()
    } else if rest.starts_with("open(") {
        start + "open(".len()
    } else {
        return None;
    };
    let comma = find_top_level_python_comma(text, arg_start)?;
    let (mode, mode_end) = parse_python_string_literal_at(text, comma + 1)?;
    if python_file_mode_can_write(&mode) {
        Some((mode, mode_end))
    } else {
        None
    }
}

fn find_top_level_python_comma(text: &str, start: usize) -> Option<usize> {
    let mut quote = None::<char>;
    let mut escaped = false;
    let mut depth = 0usize;
    for (relative, ch) in text[start..].char_indices() {
        if let Some(active_quote) = quote {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == active_quote {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' | '[' | '{' => depth += 1,
            ')' if depth == 0 => return None,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => return Some(start + relative),
            '\n' | '\r' => return None,
            _ => {}
        }
    }
    None
}

fn python_file_mode_can_write(mode: &str) -> bool {
    mode.chars().any(|ch| matches!(ch, 'w' | 'a' | 'x' | '+'))
}

fn python_pathlib_assignments(command: &str) -> Vec<(String, String)> {
    let mut assignments = Vec::new();
    for line in command.lines() {
        let line = line.trim_start();
        if line.starts_with('#') {
            continue;
        }
        let Some(eq_index) = line.find('=') else {
            continue;
        };
        let name = line[..eq_index].trim();
        if !is_python_identifier(name) {
            continue;
        }
        let right = line[eq_index + 1..].trim_start();
        if let Some((path, _)) = parse_python_path_call_at(right, 0) {
            assignments.push((name.to_string(), path));
        }
    }
    assignments
}

fn contains_python_method_call(command: &str, name: &str, method: &str) -> bool {
    let pattern = format!("{name}.{method}(");
    let mut offset = 0;
    while let Some(relative) = command[offset..].find(&pattern) {
        let index = offset + relative;
        let before = command[..index].chars().next_back();
        if before.map_or(true, |ch| !is_python_identifier_char(ch)) {
            return true;
        }
        offset = index + pattern.len();
    }
    false
}

fn find_next_python_path_call(command: &str, start: usize) -> Option<usize> {
    let path = command[start..].find("Path(").map(|index| start + index);
    let pathlib = command[start..]
        .find("pathlib.Path(")
        .map(|index| start + index);
    match (path, pathlib) {
        (Some(path), Some(pathlib)) => Some(path.min(pathlib)),
        (Some(path), None) => Some(path),
        (None, Some(pathlib)) => Some(pathlib),
        (None, None) => None,
    }
}

fn parse_python_path_call_at(text: &str, start: usize) -> Option<(String, usize)> {
    let rest = &text[start..];
    let arg_start = if rest.starts_with("pathlib.Path(") {
        start + "pathlib.Path(".len()
    } else if rest.starts_with("Path(") {
        start + "Path(".len()
    } else {
        return None;
    };
    let (path, value_end) = parse_python_string_literal_at(text, arg_start)?;
    let close_paren = skip_ascii_whitespace(text, value_end);
    if text[close_paren..].starts_with(')') {
        Some((path, close_paren + 1))
    } else {
        None
    }
}

fn parse_python_string_literal_at(text: &str, start: usize) -> Option<(String, usize)> {
    let mut index = skip_ascii_whitespace(text, start);
    while let Some(ch) = text[index..].chars().next() {
        if matches!(ch, 'r' | 'R' | 'u' | 'U' | 'b' | 'B' | 'f' | 'F') {
            index += ch.len_utf8();
            continue;
        }
        break;
    }
    let quote = text[index..].chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let body_start = index + quote.len_utf8();
    let mut escaped = false;
    for (relative, ch) in text[body_start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == quote {
            return Some((
                text[body_start..body_start + relative].to_string(),
                body_start + relative + quote.len_utf8(),
            ));
        }
    }
    None
}

fn skip_ascii_whitespace(text: &str, start: usize) -> usize {
    let mut index = start;
    while let Some(ch) = text[index..].chars().next() {
        if !ch.is_ascii_whitespace() {
            break;
        }
        index += ch.len_utf8();
    }
    index
}

fn is_python_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic()) && chars.all(is_python_identifier_char)
}

fn is_python_identifier_char(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

pub(in crate::runtime::permissions) fn is_usable_write_path(path: &str) -> bool {
    let path = path.trim();
    if path.is_empty() || path.contains('\n') || path.contains('\r') {
        return false;
    }
    if path.contains('<') || path.contains('>') {
        return false;
    }
    if path.starts_with('$') || path.starts_with('(') || path.starts_with('{') {
        return false;
    }
    let lower = path.to_ascii_lowercase();
    !matches!(lower.as_str(), "$null" | "null" | "nul" | "/dev/null")
}

fn looks_like_standalone_path(path: &str) -> bool {
    let path = path.trim();
    if !is_usable_write_path(path) || path.len() > 512 {
        return false;
    }
    path.contains('/')
        || path.contains('\\')
        || path.starts_with('.')
        || path
            .rsplit_once('.')
            .map(|(_, extension)| {
                !extension.is_empty()
                    && extension
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            })
            .unwrap_or(false)
}
