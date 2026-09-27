//! Skills Tauri commands: browse remote skills, install, uninstall.
//!
//! Skills are modular folders that extend agent capabilities. This module
//! exposes the skill browser and installer to the frontend.

use app_core::skills::{
    self, DEFAULT_SKILLS_PATH, DEFAULT_SKILLS_REPO, InstalledSkill, RemoteSkill,
};

/// List skills from a remote GitHub repository.
///
/// Returns a list of skills available in the specified repository path.
#[tauri::command]
pub async fn skills_list_remote(
    repo: Option<String>,
    path: Option<String>,
) -> Result<Vec<RemoteSkill>, String> {
    let repo = repo.unwrap_or_else(|| DEFAULT_SKILLS_REPO.to_string());
    let path = path.unwrap_or_else(|| DEFAULT_SKILLS_PATH.to_string());

    skills::list_remote_skills(&repo, &path)
        .await
        .map_err(|e| e.to_string())
}

/// List all locally installed skills.
#[tauri::command]
pub async fn skills_list_installed() -> Result<Vec<InstalledSkill>, String> {
    let skills_dir = skills::default_skills_dir().map_err(|e| e.to_string())?;
    skills::list_installed_skills(&skills_dir).map_err(|e| e.to_string())
}

/// Install a skill from a GitHub repository.
///
/// Downloads the skill and installs it to ~/.kodex/skills.
#[tauri::command]
pub async fn skills_install(repo: String, skill_path: String) -> Result<InstalledSkill, String> {
    let skills_dir = skills::default_skills_dir().map_err(|e| e.to_string())?;

    let path = skills::install_skill(&repo, &skill_path, &skills_dir)
        .await
        .map_err(|e| e.to_string())?;

    // Read the installed skill info
    let description = skills::read_skill_description(&path);

    Ok(InstalledSkill {
        name: skill_path.split('/').last().unwrap_or("").to_string(),
        path,
        description,
        is_system: false,
    })
}

/// Uninstall (delete) an installed skill.
#[tauri::command]
pub async fn skills_uninstall(name: String) -> Result<(), String> {
    let skills_dir = skills::default_skills_dir().map_err(|e| e.to_string())?;
    let skill_path = skills_dir.join(&name);
    skills::uninstall_skill(&skill_path).map_err(|e| e.to_string())
}

/// Get the default skills directory path.
#[tauri::command]
pub async fn skills_get_default_dir() -> Result<String, String> {
    let dir = skills::default_skills_dir().map_err(|e| e.to_string())?;
    Ok(dir.display().to_string())
}

/// Get the default repository name.
#[tauri::command]
pub async fn skills_get_default_repo() -> String {
    DEFAULT_SKILLS_REPO.to_string()
}

/// Get the default path within the repository.
#[tauri::command]
pub async fn skills_get_default_path() -> String {
    DEFAULT_SKILLS_PATH.to_string()
}
