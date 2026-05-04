use crate::state::AppState;
use std::collections::HashMap;
use std::process::{Command, Stdio};
use tauri::State;
use workspace_model::{SearchFileResult, SearchMatch, SearchResult};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const MAX_MATCHES: u32 = 200;

#[tauri::command]
pub fn fs_search(state: State<'_, AppState>, query: String) -> Result<SearchResult, String> {
    let query = query.trim().to_string();
    if query.is_empty() {
        return Err("Search query must not be empty".to_string());
    }

    let workspace_root = state.with_app(|app| Ok(app.ui.workspace.root.clone()))?;

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

    let output = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "ripgrep (rg) is not installed. Please install it: https://github.com/BurntSushi/ripgrep#installation".to_string()
        } else {
            format!("Failed to execute ripgrep: {}", e)
        }
    })?;

    // rg exits with 1 when no matches are found. With --no-messages, ignore
    // per-file IO errors so special Windows device paths (e.g. NUL) do not break search.
    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.code() == Some(2) && stdout.trim().is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            return Err(format!("ripgrep error: {}", stderr));
        }
    }

    parse_rg_json(&stdout, &query, &workspace_root.to_string_lossy())
}

fn parse_rg_json(stdout: &str, query: &str, workspace_root: &str) -> Result<SearchResult, String> {
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
        files,
        total_matches,
        truncated,
    })
}
