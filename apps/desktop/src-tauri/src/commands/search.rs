use crate::state::AppState;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use tauri::State;
use workspace_model::{
    SearchFileResult, SearchFileSuggestion, SearchMatch, SearchNotice, SearchResult,
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const MAX_MATCHES: u32 = 200;
const MAX_FILE_SUGGESTIONS: usize = 25;
const RIPGREP_INSTALL_URL: &str = "https://github.com/BurntSushi/ripgrep#installation";
const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", "dist", "build"];
const SKIP_SUFFIXES: &[&str] = &[".min.js", ".min.css", ".map", ".lock"];

#[tauri::command]
pub fn fs_search(state: State<'_, AppState>, query: String) -> Result<SearchResult, String> {
    let query = query.trim().to_string();
    if query.is_empty() {
        return Err("Search query must not be empty".to_string());
    }

    let remote_result = state.with_app(|app| {
        if app.is_remote_workspace() {
            app.search_workspace(&query).map(Some)
        } else {
            Ok(None)
        }
    })?;
    if let Some(result) = remote_result {
        return Ok(result);
    }

    let workspace_root = state.with_app(|app| Ok(app.ui.workspace.root.clone()))?;
    let file_suggestions = collect_file_suggestions(Path::new(&workspace_root), &query);

    let mut cmd = Command::new("rg");
    cmd.current_dir(&workspace_root)
        .arg("--json")
        .arg("--no-messages")
        .arg("--max-count")
        .arg("50")
        .arg("--fixed-strings")
        .arg("--ignore-case")
        .arg("--glob")
        .arg("!dist/")
        .arg("--glob")
        .arg("!node_modules/")
        .arg("--glob")
        .arg("!target/")
        .arg("--glob")
        .arg("!build/")
        .arg("--glob")
        .arg("!.git/")
        .arg("--glob")
        .arg("!*.min.js")
        .arg("--glob")
        .arg("!*.min.css")
        .arg("--glob")
        .arg("!*.map")
        .arg("--glob")
        .arg("!*.lock")
        .arg("--")
        .arg(&query)
        .arg(".")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Prevent console window from flashing on Windows
    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW

    let output = match cmd.output() {
        Ok(output) => output,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(empty_search_result(
                &query,
                file_suggestions,
                Some(ripgrep_missing_notice()),
            ));
        }
        Err(e) => {
            return Err(format!("Failed to execute ripgrep: {}", e));
        }
    };

    // rg exits with 1 when no matches are found. With --no-messages, ignore
    // per-file IO errors so special Windows device paths (e.g. NUL) do not break search.
    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.code() == Some(2) && stdout.trim().is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            return Err(format!("ripgrep error: {}", stderr));
        }
    }

    parse_rg_json(
        &stdout,
        &query,
        &workspace_root.to_string_lossy(),
        file_suggestions,
        None,
    )
}

fn parse_rg_json(
    stdout: &str,
    query: &str,
    workspace_root: &str,
    file_suggestions: Vec<SearchFileSuggestion>,
    notice: Option<SearchNotice>,
) -> Result<SearchResult, String> {
    let mut file_matches: HashMap<String, Vec<SearchMatch>> = HashMap::new();
    let mut total_matches: u32 = 0;
    let mut truncated = false;

    // Normalize root path for prefix stripping
    let root_prefix = workspace_root.replace('\\', "/");
    let root_prefix = root_prefix.trim_end_matches('/');

    for line in stdout.lines() {
        if total_matches >= MAX_MATCHES {
            truncated = true;
            break;
        }

        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if value.get("type").and_then(|t| t.as_str()) != Some("match") {
            continue;
        }

        let data = match value.get("data") {
            Some(d) => d,
            None => continue,
        };

        let path_obj = data
            .get("path")
            .and_then(|p| p.get("text"))
            .and_then(|t| t.as_str());
        let line_number = data.get("line_number").and_then(|n| n.as_u64());
        let line_text = data
            .get("lines")
            .and_then(|l| l.get("text"))
            .and_then(|t| t.as_str());

        if let (Some(path), Some(line_num), Some(text)) = (path_obj, line_number, line_text) {
            // Make path relative to workspace root
            let normalized_path = path.replace('\\', "/");
            let relative_path = if normalized_path.starts_with(root_prefix) {
                normalized_path[root_prefix.len()..]
                    .trim_start_matches('/')
                    .to_string()
            } else {
                normalized_path.trim_start_matches("./").to_string()
            };

            file_matches
                .entry(relative_path)
                .or_default()
                .push(SearchMatch {
                    line_number: line_num as u32,
                    line_text: text.trim_end_matches('\n').to_string(),
                });
            total_matches += 1;
        }
    }

    // Sort files alphabetically
    let mut files: Vec<SearchFileResult> = file_matches
        .into_iter()
        .map(|(path, matches)| SearchFileResult { path, matches })
        .collect();
    files.sort_by(|a, b| a.path.to_lowercase().cmp(&b.path.to_lowercase()));

    Ok(SearchResult {
        query: query.to_string(),
        file_suggestions,
        files,
        total_matches,
        truncated,
        notice,
    })
}

fn empty_search_result(
    query: &str,
    file_suggestions: Vec<SearchFileSuggestion>,
    notice: Option<SearchNotice>,
) -> SearchResult {
    SearchResult {
        query: query.to_string(),
        file_suggestions,
        files: Vec::new(),
        total_matches: 0,
        truncated: false,
        notice,
    }
}

fn ripgrep_missing_notice() -> SearchNotice {
    SearchNotice {
        message: "未检测到 ripgrep (rg)，内容搜索不可用。安装说明：".to_string(),
        url: Some(RIPGREP_INSTALL_URL.to_string()),
        url_label: Some(RIPGREP_INSTALL_URL.to_string()),
    }
}

fn collect_file_suggestions(root: &Path, query: &str) -> Vec<SearchFileSuggestion> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }

    let mut ranked: Vec<(u8, usize, String, SearchFileSuggestion)> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().to_string();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if !should_skip_dir(&file_name) {
                    stack.push(entry.path());
                }
                continue;
            }
            if !file_type.is_file() || should_skip_file(&file_name) {
                continue;
            }
            let Some(relative) = relative_workspace_path(root, &entry.path()) else {
                continue;
            };
            let Some(score) = suggestion_score(&relative, &file_name, &query) else {
                continue;
            };
            ranked.push((
                score,
                relative.len(),
                relative.to_lowercase(),
                SearchFileSuggestion {
                    path: relative,
                    name: file_name,
                },
            ));
        }
    }

    ranked.sort_by(|a, b| (&a.0, &a.1, &a.2).cmp(&(&b.0, &b.1, &b.2)));
    ranked
        .into_iter()
        .take(MAX_FILE_SUGGESTIONS)
        .map(|(_, _, _, suggestion)| suggestion)
        .collect()
}

fn relative_workspace_path(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    Some(relative.to_string_lossy().replace('\\', "/"))
}

fn should_skip_dir(name: &str) -> bool {
    SKIP_DIRS.iter().any(|skip| name.eq_ignore_ascii_case(skip))
}

fn should_skip_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    SKIP_SUFFIXES.iter().any(|suffix| lower.ends_with(suffix))
}

fn suggestion_score(path: &str, name: &str, query: &str) -> Option<u8> {
    let path = path.to_lowercase();
    let name = name.to_lowercase();
    if name == query {
        Some(0)
    } else if name.starts_with(query) {
        Some(1)
    } else if path.starts_with(query) {
        Some(2)
    } else if name.contains(query) {
        Some(3)
    } else if path.contains(query) {
        Some(4)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn file_suggestions_prefer_matching_file_names() {
        let root = temp_root("file_suggestions");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("src").join("SearchResults.tsx"), "").unwrap();
        fs::write(root.join("docs").join("search-notes.md"), "").unwrap();
        fs::write(root.join("target").join("SearchResults.tsx"), "").unwrap();

        let suggestions = collect_file_suggestions(&root, "search");

        assert_eq!(suggestions[0].path, "docs/search-notes.md");
        assert!(
            suggestions
                .iter()
                .any(|item| item.path == "src/SearchResults.tsx")
        );
        assert!(
            !suggestions
                .iter()
                .any(|item| item.path == "target/SearchResults.tsx")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parse_rg_json_preserves_file_suggestions_and_notice() {
        let stdout = r#"{"type":"match","data":{"path":{"text":"./src/main.rs"},"line_number":7,"lines":{"text":"fn search() {}\n"}}}"#;
        let suggestions = vec![SearchFileSuggestion {
            path: "src/main.rs".into(),
            name: "main.rs".into(),
        }];
        let notice = Some(ripgrep_missing_notice());

        let result = parse_rg_json(stdout, "search", "/tmp/root", suggestions, notice).unwrap();

        assert_eq!(result.file_suggestions.len(), 1);
        assert_eq!(result.files[0].path, "src/main.rs");
        assert!(result.notice.is_some());
    }

    fn temp_root(label: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("kodex_{label}_{nonce}"));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
