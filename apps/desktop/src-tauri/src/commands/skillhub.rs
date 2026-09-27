//! SkillHub Tauri commands: search, install, and CLI management.

use app_core::AppPaths;
use app_core::skillhub::{self, SkillHubSkill};
use std::io::Read;
use std::process::{Command, Stdio};
use tauri::{AppHandle, Emitter};

/// Check if the skillhub CLI is installed.
#[tauri::command]
pub fn skillhub_status() -> Result<bool, String> {
    Ok(skillhub::is_cli_available())
}

/// Install the skillhub CLI with streaming progress.
#[tauri::command]
pub async fn skillhub_install_cli(app: AppHandle<tauri::Wry>) -> Result<String, String> {
    let result = tauri::async_runtime::spawn_blocking(move || install_cli_streaming(&app))
        .await
        .map_err(|e| format!("Task join error: {e}"))?;

    result.map_err(|e| e.to_string())
}

/// Run the CLI installer with stdout/stderr streamed as Tauri events.
fn install_cli_streaming(app: &AppHandle) -> anyhow::Result<String> {
    if skillhub::is_cli_available() {
        return Ok("skillhub CLI 已安装".to_string());
    }

    let _ = app.emit("skillhub:cli-install-progress", "开始安装 skillhub CLI...");

    let mut child = Command::new("sh")
        .arg("-c")
        .arg("curl -fsSL https://skillhub.cn/install/install.sh | bash -s -- --cli-only")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn installer: {e}"))?;

    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();

    let mut buf = [0u8; 1024];

    loop {
        let mut read_any = false;

        match stdout.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => {
                let text = String::from_utf8_lossy(&buf[..n]).to_string();
                let _ = app.emit("skillhub:cli-install-progress", text.trim());
                read_any = true;
            }
            Err(_) => {}
        }

        match stderr.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => {
                let text = String::from_utf8_lossy(&buf[..n]).to_string();
                let _ = app.emit("skillhub:cli-install-progress", text.trim());
                read_any = true;
            }
            Err(_) => {}
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    let _ = app.emit("skillhub:cli-install-progress", "安装失败");
                    anyhow::bail!("skillhub CLI 安装失败，请检查网络连接后重试");
                }
                break;
            }
            Ok(None) => {
                if !read_any {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
            Err(_) => break,
        }
    }

    let _ = app.emit("skillhub:cli-install-progress", "安装完成");

    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .ok_or_else(|| anyhow::anyhow!("无法解析用户主目录"))?;
    let local_bin = std::path::PathBuf::from(&home).join(".local/bin/skillhub");

    if local_bin.exists() {
        Ok(format!(
            "skillhub CLI 已安装到 {}，请重开应用或确保 PATH 包含 ~/.local/bin",
            local_bin.display()
        ))
    } else {
        Ok("skillhub CLI 已安装，请重开应用或确保 PATH 包含 ~/.local/bin".to_string())
    }
}

/// Search for skills on SkillHub.
#[tauri::command]
pub async fn skillhub_search(query: String) -> Result<Vec<SkillHubSkill>, String> {
    skillhub::search_skills(&query)
        .await
        .map_err(|e| e.to_string())
}

/// Get skill rankings (hot/featured/newest/recommended/trending/paid).
///
/// This is the one path that still needs the `skillhub` CLI — there is no
/// public endpoint for the category boards.
#[tauri::command]
pub async fn skillhub_rankings(ranking_type: String) -> Result<Vec<SkillHubSkill>, String> {
    skillhub::get_rankings(&ranking_type)
        .await
        .map_err(|e| e.to_string())
}

/// Install a skill from SkillHub (direct download, no CLI required).
#[tauri::command]
pub async fn skillhub_install(slug: String) -> Result<String, String> {
    let dest_dir = skillhub::default_skills_dir().map_err(|e| e.to_string())?;
    let path = skillhub::install_skill(&slug, &dest_dir)
        .await
        .map_err(|e| e.to_string())?;
    Ok(path.display().to_string())
}
