use crate::state::AppState;
use std::path::Path;
use tauri::{AppHandle, Manager, State};
use workspace_model::{FileEntry, FileEntryKind};

const MAX_MENTION_DIR_ENTRIES: usize = 60;

#[tauri::command]
pub async fn fs_mention_suggest(app: AppHandle, query: String) -> Result<Vec<FileEntry>, String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let query = query.trim().to_string();

        // Drill-down: the query contains a path separator, so list that
        // directory and filter its direct children by the trailing prefix.
        // This makes `@apps/desktop/comp` browse into `apps/desktop`.
        if let Some(slash) = query.rfind('/') {
            let dir = &query[..slash];
            let prefix = query[slash + 1..].to_lowercase();
            let entries = state.list_workspace_dir(dir.to_string()).unwrap_or_default();
            return Ok(filter_mention_dir_entries(entries, &prefix));
        }

        // Flat: project-wide fuzzy match across files and directories.
        // Remote workspaces delegate to their search endpoint (files only);
        // local workspaces walk the tree cheaply without spawning ripgrep.
        let remote_result = state.with_app(|app| {
            if app.is_remote_workspace() {
                app.search_workspace(&query).map(Some)
            } else {
                Ok(None)
            }
        })?;
        if let Some(result) = remote_result {
            return Ok(result
                .file_suggestions
                .into_iter()
                .map(|suggestion| FileEntry {
                    name: suggestion.name,
                    kind: FileEntryKind::File,
                    path: suggestion.path,
                })
                .collect());
        }

        let workspace_root = state.with_app(|app| Ok(app.ui.workspace.root.clone()))?;
        Ok(crate::commands::search::collect_mention_suggestions(
            &workspace_root,
            &query,
        ))
    })
    .await
    .map_err(|e| format!("Mention suggest task failed: {e}"))?
}

fn filter_mention_dir_entries(entries: Vec<FileEntry>, prefix: &str) -> Vec<FileEntry> {
    if prefix.is_empty() {
        return entries.into_iter().take(MAX_MENTION_DIR_ENTRIES).collect();
    }
    entries
        .into_iter()
        .filter(|entry| entry.name.to_lowercase().starts_with(&prefix))
        .take(MAX_MENTION_DIR_ENTRIES)
        .collect()
}

#[tauri::command]
pub async fn fs_list_dir(app: AppHandle, path: String) -> Result<Vec<FileEntry>, String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.list_workspace_dir(path)
    })
    .await
    .map_err(|e| format!("List directory task failed: {e}"))?
}

#[tauri::command]
pub fn fs_rename(
    state: State<'_, AppState>,
    path: String,
    new_name: String,
) -> Result<FileEntry, String> {
    state.with_app(|app| app.rename_workspace_entry(&path, &new_name))
}

#[tauri::command]
pub fn fs_delete_file(state: State<'_, AppState>, path: String) -> Result<(), String> {
    state.with_app(|app| app.delete_workspace_file(&path))
}

#[tauri::command]
pub fn fs_reveal(state: State<'_, AppState>, path: String, select: bool) -> Result<(), String> {
    state.with_app(|app| {
        ensure_local_workspace(app)?;
        let target = app.resolve_workspace_entry_for_shell(&path)?;
        reveal_path(&target, select).map_err(|e| format!("Cannot open file explorer: {e}"))
    })
}

/// Cheap existence check used by the chat renderer to decide whether an
/// inline-code span is a real, openable workspace file before rendering it
/// as a clickable link. Returns `false` for missing/outside/dir paths, but
/// propagates a workspace/reconnect error so the UI can retry instead of
/// caching a transient startup failure as a permanent dead link.
#[tauri::command]
pub async fn fs_path_exists(app: AppHandle, paths: Vec<String>) -> Result<Vec<bool>, String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state
            .with_app(|app| app.workspace_paths_exist(&paths))
            .map_err(|error| format!("Path existence probe unavailable: {error}"))
    })
    .await
    .map_err(|e| format!("Path exists task failed: {e}"))?
}

/// 将用户选择的 VRM 模型复制到受控资产目录（$HOME/.kodex/companion/），
/// 返回可直接用于 asset 协议加载的绝对路径。
/// 该目录已在 tauri.conf.json 的 assetProtocol.scope 白名单内，
/// 避免「convertFileSrc + 任意路径」在 Windows 下的 scope 匹配不确定性。
#[tauri::command]
pub async fn companion_stage_model(source_path: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let source = Path::new(&source_path);
        if !source.exists() {
            return Err(format!("源文件不存在: {source_path}"));
        }
        let extension = source
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_lowercase())
            .unwrap_or_default();
        if extension != "vrm" && extension != "vrma" {
            return Err(format!("仅支持 .vrm / .vrma 文件，当前: .{extension}"));
        }
        let size_mb = source
            .metadata()
            .map(|meta| meta.len() as f64 / 1_048_576.0)
            .unwrap_or(0.0);
        if size_mb > 64.0 {
            return Err(format!("模型文件过大（{size_mb:.1}MB > 64MB），请压缩贴图后重试"));
        }

        let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
        let companion_dir = paths.root().join("companion");
        std::fs::create_dir_all(&companion_dir)
            .map_err(|e| format!("无法创建资产目录 {}: {e}", companion_dir.display()))?;

        let file_name = source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "无法解析文件名".to_string())?;
        let target = companion_dir.join(file_name);
        // 源文件可能已经在受控目录里（用户直接选了 ~/.kodex/companion/ 下的
        // 模型）。此时复制是 no-op，且"先删目标再复制"会把源也删掉导致
        // os error 2；规范化路径比较后跳过。
        if !paths_equal(source, &target) {
            // Windows 上 `std::fs::copy` 覆盖一个仍被占用（如 WebView 里
            // three-vrm 仍持有句柄）的目标文件会报 os error 32。先删除目标
            // 再复制；删除本身也可能瞬时失败，重试几次给加载器释放句柄的时间。
            copy_with_replace_retry(source, &target)
                .map_err(|e| format!("复制到受控目录失败 {}: {e}", target.display()))?;
        }

        Ok(target.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| format!("Stage model task failed: {e}"))?
}

/// Compare two paths for equality after normalizing separators, case, and any
/// canonicalization differences (e.g. `C:\a\b.vrm` vs `C:/a/b.vrm`). Falls
/// back to a plain component-wise comparison when canonicalization fails.
fn paths_equal(a: &Path, b: &Path) -> bool {
    let canon_a = a.canonicalize().ok();
    let canon_b = b.canonicalize().ok();
    if let (Some(ca), Some(cb)) = (canon_a, canon_b) {
        if ca == cb {
            return true;
        }
        // Windows canonicalize keeps original casing; compare case-insensitively.
        return ca.to_string_lossy().to_lowercase() == cb.to_string_lossy().to_lowercase();
    }
    // Fallback: normalize separators + lowercase and compare strings.
    let norm = |p: &Path| p.to_string_lossy().replace('\\', "/").to_lowercase();
    norm(a) == norm(b)
}

/// Copy `source` onto `target`, tolerating a brief Windows file lock on the
/// destination: remove the target first and retry a few times so a loader that
/// still holds a handle (e.g. three-vrm in the WebView) gets a chance to drop
/// it before we surface os error 32.
fn copy_with_replace_retry(source: &Path, target: &Path) -> std::io::Result<()> {
    const MAX_ATTEMPTS: u32 = 6;
    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..MAX_ATTEMPTS {
        if target.exists() {
            match std::fs::remove_file(target) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(120 * (attempt as u64 + 1)));
                    continue;
                }
            }
        }
        return std::fs::copy(source, target).map(|_| ());
    }
    Err(last_err.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::Other, "replace retry exhausted")
    }))
}

fn ensure_local_workspace(app: &app_core::Application) -> Result<(), String> {
    if app.is_remote_workspace() {
        Err("Remote workspaces do not support local filesystem commands yet".into())
    } else {
        Ok(())
    }
}

/// Resolve bare file names mentioned in chat (e.g. `Composer.tsx:548`) to
/// their workspace-relative path by scanning for the first file with that
/// exact name. Returns null when no match exists so the span stays plain
/// code. Skips heavy directories so the walk stays cheap.
#[tauri::command]
pub async fn fs_find_by_name(app: AppHandle, names: Vec<String>) -> Result<Vec<Option<String>>, String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let root = state
            .with_app(|app| {
                if app.is_remote_workspace() {
                    return Ok(None);
                }
                let root = app.ui.workspace.root.clone();
                Ok(if root.as_os_str().is_empty() { None } else { Some(root) })
            })
            .ok()
            .flatten();
        let Some(root) = root else {
            return Ok(names.iter().map(|_| None).collect());
        };
        const SKIP_DIRS: &[&str] = &[
            "node_modules", "target", "dist", ".git", "build", "out",
            ".next", ".turbo", "coverage", "__pycache__",
        ];
        let mut remaining: std::collections::HashSet<&str> =
            names.iter().map(String::as_str).collect();
        let mut found: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            if remaining.is_empty() {
                break;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name_str) = name.to_str() else {
                    continue;
                };
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_dir() {
                    if !SKIP_DIRS.contains(&name_str) && !name_str.starts_with('.') {
                        stack.push(entry.path());
                    }
                } else if file_type.is_file() && remaining.contains(name_str) {
                    if let Ok(relative) = entry.path().strip_prefix(&root) {
                        found.insert(
                            name_str.to_string(),
                            relative.to_string_lossy().replace('\\', "/"),
                        );
                        remaining.remove(name_str);
                    }
                }
            }
        }
        Ok(names
            .iter()
            .map(|name| found.get(name).cloned())
            .collect())
    })
    .await
    .map_err(|e| format!("Find-by-name task failed: {e}"))?
}

#[cfg(target_os = "windows")]
fn reveal_path(path: &Path, select: bool) -> std::io::Result<()> {
    let mut command = std::process::Command::new("explorer.exe");
    if select && path.is_file() {
        command.arg(format!("/select,{}", path.display()));
    } else {
        command.arg(path);
    }
    command.spawn().map(|_| ())
}

#[cfg(target_os = "macos")]
fn reveal_path(path: &Path, select: bool) -> std::io::Result<()> {
    let mut command = std::process::Command::new("open");
    if select && path.is_file() {
        command.arg("-R").arg(path);
    } else {
        command.arg(path);
    }
    command.spawn().map(|_| ())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn reveal_path(path: &Path, select: bool) -> std::io::Result<()> {
    let target = if select && path.is_file() {
        path.parent().unwrap_or(path)
    } else {
        path
    };
    std::process::Command::new("xdg-open")
        .arg(target)
        .spawn()
        .map(|_| ())
}
