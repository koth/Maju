//! Skill repository browser and installer.
//!
//! Skills are modular, self-contained folders that extend agent capabilities.
//! This module provides:
//! - Browsing skills from a GitHub repository (e.g., openai/skills)
//! - Installing skills to the local skills directory (`~/.kodex/skills`)
//! - Listing installed skills

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Default GitHub repository for skills (owner/repo format).
pub const DEFAULT_SKILLS_REPO: &str = "openai/skills";

/// Default path within the repository where skills are stored.
pub const DEFAULT_SKILLS_PATH: &str = "skills/.curated";

/// A skill entry from the remote repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteSkill {
    /// Skill name (directory name in the repository).
    pub name: String,
    /// Path to the skill directory within the repository.
    pub path: String,
    /// Short description extracted from SKILL.md frontmatter (if available).
    pub description: Option<String>,
}

/// A locally installed skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledSkill {
    /// Skill name (directory name).
    pub name: String,
    /// Path to the skill directory.
    pub path: PathBuf,
    /// Description from SKILL.md frontmatter.
    pub description: Option<String>,
    /// Whether this is a system skill (in .system directory).
    pub is_system: bool,
}

/// GitHub API response for a directory listing.
#[derive(Debug, Deserialize)]
struct GitHubTreeResponse {
    tree: Vec<GitHubTreeEntry>,
}

#[derive(Debug, Deserialize)]
struct GitHubTreeEntry {
    path: String,
    #[serde(rename = "type")]
    entry_type: String,
}

/// GitHub API response for file content.
#[derive(Debug, Deserialize)]
struct GitHubContentResponse {
    content: String,
    encoding: String,
}

/// Parse "owner/repo" format.
pub fn parse_repo(repo: &str) -> Result<(String, String)> {
    let parts: Vec<&str> = repo.trim().split('/').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        anyhow::bail!(
            "Invalid repository format. Expected 'owner/repo', got '{}'",
            repo
        );
    }
    Ok((parts[0].to_string(), parts[1].to_string()))
}

/// Fetch the list of skills from a GitHub repository.
///
/// Uses the GitHub API to list directories under the specified path.
/// Each subdirectory is treated as a skill.
pub async fn list_remote_skills(repo: &str, path: &str) -> Result<Vec<RemoteSkill>> {
    let (owner, repo_name) = parse_repo(repo)?;

    // Use GitHub API to list directory contents
    let url = format!(
        "https://api.github.com/repos/{}/{}/contents/{}",
        owner, repo_name, path
    );

    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .header("User-Agent", "maju-skills-browser")
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .context("Failed to fetch skills from GitHub")?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        anyhow::bail!("GitHub API error {}: {}", status, text);
    }

    let items: Vec<GitHubTreeEntry> = response
        .json()
        .await
        .context("Failed to parse GitHub API response")?;

    let mut skills = Vec::new();

    for item in items {
        // Only include directories (type == "tree")
        if item.entry_type != "tree" {
            continue;
        }

        // Extract skill name from path
        let name = item
            .path
            .trim_start_matches(&format!("{}/", path))
            .split('/')
            .next()
            .unwrap_or(&item.path)
            .to_string();

        if name.is_empty() {
            continue;
        }

        // Try to fetch SKILL.md for description
        let description = fetch_skill_description(repo, &item.path)
            .await
            .ok()
            .flatten();

        skills.push(RemoteSkill {
            name,
            path: item.path,
            description,
        });
    }

    Ok(skills)
}

/// Fetch the description from a skill's SKILL.md file.
async fn fetch_skill_description(repo: &str, skill_path: &str) -> Result<Option<String>> {
    let (owner, repo_name) = parse_repo(repo)?;

    let url = format!(
        "https://api.github.com/repos/{}/{}/contents/{}/SKILL.md",
        owner, repo_name, skill_path
    );

    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .header("User-Agent", "maju-skills-browser")
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .context("Failed to fetch SKILL.md")?;

    if !response.status().is_success() {
        return Ok(None);
    }

    let content: GitHubContentResponse = response
        .json()
        .await
        .context("Failed to parse SKILL.md response")?;

    // Decode base64 content
    let decoded = decode_base64(&content.content)?;

    // Extract description from frontmatter
    let description = extract_description_from_markdown(&decoded);

    Ok(description)
}

/// Decode base64 content (GitHub API returns base64-encoded content).
fn decode_base64(content: &str) -> Result<String> {
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(content.replace('\n', ""))
        .context("Failed to decode base64 content")?;
    String::from_utf8(decoded).context("Invalid UTF-8 in decoded content")
}

/// Extract description from SKILL.md frontmatter.
fn extract_description_from_markdown(content: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();

    // Check for YAML frontmatter
    if lines.is_empty() || lines[0].trim() != "---" {
        return None;
    }

    // Find the closing ---
    let mut end_idx = None;
    for (i, line) in lines.iter().enumerate().skip(1) {
        if line.trim() == "---" {
            end_idx = Some(i);
            break;
        }
    }

    let end_idx = end_idx?;

    // Parse frontmatter for description
    for line in &lines[1..end_idx] {
        if let Some(desc) = line.strip_prefix("description:") {
            return Some(desc.trim().to_string());
        }
    }

    None
}

/// List all installed skills in the skills directory.
pub fn list_installed_skills(skills_dir: &Path) -> Result<Vec<InstalledSkill>> {
    let mut skills = Vec::new();

    if !skills_dir.exists() {
        return Ok(skills);
    }

    // Read .system directory first
    let system_dir = skills_dir.join(".system");
    if system_dir.exists() {
        collect_skills_from_dir(&system_dir, &mut skills, true)?;
    }

    // Read regular skills
    collect_skills_from_dir(skills_dir, &mut skills, false)?;

    Ok(skills)
}

/// Collect skills from a directory.
fn collect_skills_from_dir(
    dir: &Path,
    skills: &mut Vec<InstalledSkill>,
    is_system: bool,
) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("Failed to read directory: {}", dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        if !path.is_dir() {
            continue;
        }

        let name = entry.file_name().to_string_lossy().to_string();

        // Skip hidden directories (including .system itself — it's a container,
        // not a skill; its contents are collected separately)
        if name.starts_with('.') {
            continue;
        }

        // Try to read SKILL.md for description
        let description = read_skill_description(&path);

        skills.push(InstalledSkill {
            name,
            path,
            description,
            is_system,
        });
    }

    Ok(())
}

/// Read description from a skill's SKILL.md file.
pub fn read_skill_description(skill_dir: &Path) -> Option<String> {
    let skill_md = skill_dir.join("SKILL.md");
    let content = std::fs::read_to_string(skill_md).ok()?;
    extract_description_from_markdown(&content)
}

/// Install a skill from GitHub repository.
///
/// Downloads the skill directory as a ZIP archive and extracts it to the
/// destination directory.
pub async fn install_skill(repo: &str, skill_path: &str, dest_dir: &Path) -> Result<PathBuf> {
    let (owner, repo_name) = parse_repo(repo)?;
    let skill_name = skill_path
        .split('/')
        .last()
        .unwrap_or(skill_path)
        .to_string();

    // Download ZIP archive from GitHub
    let url = format!(
        "https://api.github.com/repos/{}/{}/zipball/{}",
        owner, repo_name, "main"
    );

    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .header("User-Agent", "maju-skills-installer")
        .send()
        .await
        .context("Failed to download skill archive")?;

    if !response.status().is_success() {
        let status = response.status();
        anyhow::bail!("Failed to download skill: HTTP {}", status);
    }

    let bytes = response
        .bytes()
        .await
        .context("Failed to read skill archive")?;

    // Extract ZIP archive
    let zip_path = dest_dir.join(format!("{}.zip", skill_name));
    std::fs::write(&zip_path, &bytes)
        .with_context(|| format!("Failed to write ZIP archive: {}", zip_path.display()))?;

    // Extract to temporary directory first
    let temp_dir = dest_dir.join(format!(".tmp-{}", skill_name));
    std::fs::create_dir_all(&temp_dir)?;

    extract_zip(&zip_path, &temp_dir)?;

    // Move skill to final destination
    let final_path = dest_dir.join(&skill_name);
    if final_path.exists() {
        std::fs::remove_dir_all(&final_path)?;
    }

    // Find the extracted directory (GitHub ZIP extracts to owner-repo-sha/)
    let extracted_dir = find_extracted_dir(&temp_dir)?;
    std::fs::rename(&extracted_dir, &final_path)
        .with_context(|| format!("Failed to move skill to: {}", final_path.display()))?;

    // Cleanup
    std::fs::remove_file(&zip_path).ok();
    std::fs::remove_dir_all(&temp_dir).ok();

    Ok(final_path)
}

/// Extract a ZIP archive to a directory.
fn extract_zip(zip_path: &Path, dest_dir: &Path) -> Result<()> {
    use std::io::Read;

    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let outpath = dest_dir.join(file.name());

        if file.name().ends_with('/') {
            std::fs::create_dir_all(&outpath)?;
        } else {
            if let Some(parent) = outpath.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut outfile = std::fs::File::create(&outpath)?;
            std::io::copy(&mut file, &mut outfile)?;
        }
    }

    Ok(())
}

/// Find the extracted directory from a GitHub ZIP archive.
fn find_extracted_dir(temp_dir: &Path) -> Result<PathBuf> {
    let entries = std::fs::read_dir(temp_dir)?;
    for entry in entries {
        let entry = entry?;
        if entry.path().is_dir() {
            return Ok(entry.path());
        }
    }
    anyhow::bail!("Could not find extracted directory")
}

/// Uninstall a skill by removing its directory.
pub fn uninstall_skill(skill_dir: &Path) -> Result<()> {
    if !skill_dir.exists() {
        anyhow::bail!("Skill directory does not exist: {}", skill_dir.display());
    }
    std::fs::remove_dir_all(skill_dir)
        .with_context(|| format!("Failed to remove skill directory: {}", skill_dir.display()))?;
    Ok(())
}

/// Get the skills directory path (~/.kodex/skills).
pub fn default_skills_dir() -> Result<PathBuf> {
    let paths = crate::AppPaths::resolve()?;
    Ok(paths.root().join("skills"))
}

// Add zip dependency for archive extraction
use zip::ZipArchive;
