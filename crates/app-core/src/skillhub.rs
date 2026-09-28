//! SkillHub (skillhub.cn) integration.
//!
//! Search and install go straight at the public HTTP API — it returns complete
//! records (created_at / downloads / namespace) and relevance ordering, and it
//! does not mutate itself underneath us the way the `skillhub` CLI does.
//!
//! The category rankings (热门 / 精选 / 最新 / 推荐 / 趋势) still shell out to
//! the CLI: there is no equivalent public endpoint (unknown `/api/v1/*` paths
//! all answer 405), so that is the only source for them.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// API root. `SKILLHUB_API_BASE` overrides it for enterprise gateways.
const DEFAULT_API_BASE: &str = "https://api.skillhub.cn/api/v1";

/// Page size for search. The API caps at 100.
const SEARCH_LIMIT: usize = 30;

fn api_base() -> String {
    std::env::var("SKILLHUB_API_BASE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string())
        .trim_end_matches('/')
        .to_string()
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(60))
        .user_agent("maju-skills")
        .build()
        .context("构建 HTTP 客户端失败")
}

/// A skill entry from SkillHub.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillHubSkill {
    /// Bare slug (e.g. `openspec`). The namespaced form lives in
    /// `namespace.canonicalName` (e.g. `@clawhub_jcorrego/openspec`) and is
    /// what the download endpoint accepts as an alternative.
    pub slug: String,
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// One-line description rendered on the card. Filled in by [`normalize`]
    /// from the best available source; the raw API fields are kept separately
    /// because the payload carries `description_zh`, `summary` AND
    /// `description` at once, which serde aliases cannot express.
    ///
    /// `skip_deserializing` keeps the API's own `summary` key from landing here
    /// directly — that would pre-empt [`Self::normalize`]. It is still sent to
    /// the frontend under the `summary` key.
    #[serde(default, skip_deserializing)]
    pub summary: String,
    /// The API's `summary` field.
    #[serde(rename = "summary", default, skip_serializing)]
    pub api_summary: String,
    /// The API's `description` field (identical to `summary` in practice; the
    /// rankings CLI only sends this one).
    #[serde(rename = "description", default, skip_serializing)]
    pub api_description: String,
    /// Chinese description. Preferred for display since the UI is Chinese.
    #[serde(
        rename = "description_zh",
        alias = "descriptionZh",
        default,
        skip_serializing
    )]
    pub description_zh: String,
    /// Author handle. The API field is `owner_name`.
    #[serde(default, alias = "owner_name")]
    pub author: String,
    /// Download count.
    #[serde(default)]
    pub downloads: Option<u64>,
    /// Relevance score for search hits (small float, e.g. 0.13). This is NOT
    /// the popularity score used by the rankings endpoint.
    #[serde(default)]
    pub score: Option<f64>,
    /// 上架时间（epoch 毫秒）。用于「最新」分类按创建时间排序。
    #[serde(default)]
    pub created_at: Option<u64>,
    /// Publisher block; `handle` is far more readable than `owner_name`.
    #[serde(default)]
    pub namespace: Option<SkillNamespace>,
}

/// SkillHub's nested namespace block. The API uses camelCase here even though
/// the rest of the payload is snake_case, so the mapping is explicit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillNamespace {
    #[serde(default)]
    pub handle: String,
    #[serde(rename = "displayName", default)]
    pub display_name: String,
    /// Namespaced slug, e.g. `@clawhub_jcorrego/openspec`.
    #[serde(rename = "canonicalName", default)]
    pub canonical_name: String,
}

impl SkillHubSkill {
    /// Collapse the three description fields into [`Self::summary`].
    ///
    /// Preference order: Chinese text first (the UI is Chinese), then the
    /// English `summary`, then `description`. Sources are identical in practice
    /// but the CLI only populates `description`, so all three must be tried.
    pub fn normalize(&mut self) {
        if self.summary.is_empty() {
            self.summary = first_non_empty(&[
                &self.description_zh,
                &self.api_summary,
                &self.api_description,
            ]);
        }
    }

    /// Best available display name for the publisher.
    pub fn author_label(&self) -> String {
        if let Some(ns) = &self.namespace {
            if !ns.handle.is_empty() {
                return ns.handle.clone();
            }
            if !ns.display_name.is_empty() {
                return ns.display_name.clone();
            }
        }
        self.author.clone()
    }

    /// The namespaced slug (`@owner/name`) when the API supplied one.
    pub fn canonical_slug(&self) -> String {
        self.namespace
            .as_ref()
            .map(|ns| ns.canonical_name.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.slug.clone())
    }
}

/// Percent-encode a query string component.
fn urlencode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            b' ' => out.push_str("%20"),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// First non-empty candidate, trimmed.
fn first_non_empty(candidates: &[&str]) -> String {
    candidates
        .iter()
        .map(|c| c.trim())
        .find(|c| !c.is_empty())
        .unwrap_or_default()
        .to_string()
}

fn truncate(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(max).collect();
    format!("{head}…")
}

/// Parse `{"results":[...]}` from the search API.
fn parse_search_body(body: &str) -> Result<Vec<SkillHubSkill>> {
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(default)]
        results: Vec<SkillHubSkill>,
        #[serde(default)]
        error: Option<String>,
    }

    let parsed: Envelope = serde_json::from_str(body.trim())
        .with_context(|| format!("解析 SkillHub 搜索响应失败: {}", truncate(body, 200)))?;

    if let Some(err) = parsed.error {
        anyhow::bail!("SkillHub 搜索出错: {err}");
    }
    let mut results = parsed.results;
    for skill in &mut results {
        skill.normalize();
    }
    Ok(results)
}

/// Search SkillHub. Results come back relevance-ordered.
///
/// An empty query is allowed and returns the platform's default (most popular)
/// listing, which is what the SkillHub front page shows.
pub async fn search_skills(query: &str) -> Result<Vec<SkillHubSkill>> {
    let q = query.trim();
    let url = format!(
        "{}/search?limit={}{}",
        api_base(),
        SEARCH_LIMIT,
        if q.is_empty() {
            String::new()
        } else {
            format!("&q={}", urlencode(q))
        }
    );

    let response = http_client()?
        .get(&url)
        .send()
        .await
        .with_context(|| format!("请求 SkillHub 搜索失败: {url}"))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .context("读取 SkillHub 搜索响应失败")?;

    if !status.is_success() {
        anyhow::bail!("SkillHub 搜索返回 {status}: {}", truncate(&body, 160));
    }

    parse_search_body(&body)
}

/// Where installed skills live (`~/.kodex/skills`).
pub fn default_skills_dir() -> Result<PathBuf> {
    Ok(crate::AppPaths::resolve()?.root().join("skills"))
}

/// Turn a (possibly namespaced) slug into a safe folder name.
fn skill_dir_name(slug: &str) -> String {
    let bare = slug.rsplit('/').next().unwrap_or(slug).trim();
    let cleaned: String = bare
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('-').to_string();
    if cleaned.is_empty() {
        "skill".to_string()
    } else {
        cleaned
    }
}

/// Download and install a skill into `dest_dir`, returning the created folder.
pub async fn install_skill(slug: &str, dest_dir: &Path) -> Result<PathBuf> {
    let slug = slug.trim();
    if slug.is_empty() {
        anyhow::bail!("技能 slug 不能为空");
    }
    let name = skill_dir_name(slug);
    let target = dest_dir.join(&name);

    std::fs::create_dir_all(dest_dir)
        .with_context(|| format!("创建技能目录失败: {}", dest_dir.display()))?;
    // Overwrite semantics: a reinstall replaces rather than merges.
    if target.exists() {
        std::fs::remove_dir_all(&target)
            .with_context(|| format!("清理旧技能目录失败: {}", target.display()))?;
    }

    let url = format!("{}/download?slug={}", api_base(), urlencode(slug));
    let response = http_client()?
        .get(&url)
        .send()
        .await
        .with_context(|| format!("下载技能失败: {slug}"))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("下载技能「{slug}」失败: {status} {}", truncate(&body, 160));
    }

    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("读取技能包失败: {slug}"))?;

    // Unpack into a staging folder so a corrupt archive never leaves a
    // half-written skill behind.
    let staging = dest_dir.join(format!(".{name}.installing"));
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    std::fs::create_dir_all(&staging)?;

    let result = extract_zip(&bytes, &staging).and_then(|()| {
        // Some archives wrap everything in a single top-level folder.
        if let Some(inner) = single_child_dir(&staging)? {
            std::fs::rename(inner, &target)?;
        } else {
            std::fs::rename(&staging, &target)?;
        }
        Ok(())
    });

    if let Err(err) = result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(err);
    }
    let _ = std::fs::remove_dir_all(&staging);

    Ok(target)
}

/// Extract every entry of a zip archive into `dest`.
fn extract_zip(bytes: &[u8], dest: &Path) -> Result<()> {
    use std::io::Read;

    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).context("技能包不是有效的 zip")?;

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        // `enclosed_name` rejects absolute paths and `..` traversal (zip-slip).
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let out_path = dest.join(rel);

        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;
        std::fs::write(&out_path, &buf)?;
    }
    Ok(())
}

/// If `dir` holds exactly one entry and it is a directory, return it.
fn single_child_dir(dir: &Path) -> Result<Option<PathBuf>> {
    let mut dirs = Vec::new();
    let mut total = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        total += 1;
        if entry.path().is_dir() {
            dirs.push(entry.path());
        }
    }
    if total == 1 && dirs.len() == 1 {
        Ok(dirs.pop())
    } else {
        Ok(None)
    }
}

// ── CLI-backed category rankings ──────────────────────────────────────────
//
// No public endpoint exposes 热门/精选/最新/推荐/趋势 (unknown `/api/v1/*`
// paths all answer 405), so these still shell out to the `skillhub` CLI.

/// Check if the `skillhub` CLI is installed and available.
///
/// Checks both the PATH and the default install location (`~/.local/bin`),
/// because a GUI-launched process does not inherit the user's shell PATH.
/// Build a `skillhub` command that opens no console window on Windows.
///
/// The CLI is a console-subsystem program — an npm/volta shim at that — run from
/// a GUI app, so without this every SkillHub page opens a black window for as
/// long as the command runs. Its output is captured by the caller, never
/// watched, so there is nothing for a window to show.
fn skillhub_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    command
}

pub fn is_cli_available() -> bool {
    cli_path().is_some()
}

/// Resolve the CLI binary, falling back to `~/.local/bin/skillhub`.
pub fn cli_path() -> Option<PathBuf> {
    if skillhub_command("skillhub")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return Some(PathBuf::from("skillhub"));
    }
    dirs_next::home_dir()
        .map(|home| home.join(".local/bin/skillhub"))
        .filter(|p| p.exists())
}

/// Fetch a category ranking (hot/featured/newest/recommended/trending/paid).
pub async fn get_rankings(ranking_type: &str) -> Result<Vec<SkillHubSkill>> {
    let cli = cli_path().ok_or_else(|| anyhow::anyhow!("skillhub CLI 未安装，请先安装"))?;

    let output = skillhub_command(&cli)
        // Deterministic output: without this the CLI may self-upgrade mid-run
        // and interleave upgrade chatter with the JSON payload.
        .arg("--skip-self-upgrade")
        .arg("skill")
        .arg("rankings")
        .arg("--type")
        .arg(ranking_type)
        .output()
        .context("调用 skillhub rankings 失败")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("skillhub rankings 失败: {}", stderr.trim());
    }

    parse_rankings_body(&String::from_utf8_lossy(&output.stdout), ranking_type)
}

/// Parse the CLI's rankings payload for one category.
///
/// The CLI answers `{"section":...,"skills":[...],"total":N}` for a single
/// type and `{"rankings":{...}}` for `all`.
pub fn parse_rankings_body(body: &str, ranking_type: &str) -> Result<Vec<SkillHubSkill>> {
    #[derive(Deserialize)]
    struct RankingsResponse {
        #[serde(default)]
        skills: Option<Vec<SkillHubSkill>>,
        #[serde(default)]
        rankings: Option<std::collections::HashMap<String, Vec<SkillHubSkill>>>,
    }

    let parsed: RankingsResponse = serde_json::from_str(body.trim())
        .with_context(|| format!("解析排行榜 JSON 失败: {}", truncate(body, 200)))?;

    let mut items = parsed.skills.unwrap_or_default();
    if items.is_empty() {
        if let Some(rankings) = parsed.rankings {
            items = rankings.get(ranking_type).cloned().unwrap_or_default();
        }
    }

    for skill in &mut items {
        skill.normalize();
    }

    // The CLI's `newest` section is only loosely ordered — sort explicitly by
    // creation time so "最新" actually means newest. Missing values sort last.
    if ranking_type == "newest" {
        items.sort_by(|a, b| b.created_at.unwrap_or(0).cmp(&a.created_at.unwrap_or(0)));
    }

    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_parses_results_envelope() {
        let body = r#"{"results":[
            {"slug":"openspec","name":"OpenSpec","description":"spec driven",
             "downloads":1575451,"created_at":1772744566134,
             "owner_name":"jcorrego",
             "namespace":{"canonicalName":"@clawhub_jcorrego/openspec","handle":"jcorrego","displayName":"jcorrego","publicSlug":"openspec"}}
        ]}"#;

        let skills = parse_search_body(body).expect("envelope should parse");
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].slug, "openspec");
        // `description` must land in `summary` via normalize().
        assert_eq!(skills[0].summary, "spec driven");
        assert_eq!(skills[0].downloads, Some(1_575_451));
        assert_eq!(skills[0].created_at, Some(1_772_744_566_134));
        // The readable handle wins over the raw owner id.
        assert_eq!(skills[0].author_label(), "jcorrego");
        assert_eq!(skills[0].canonical_slug(), "@clawhub_jcorrego/openspec");
    }

    /// The live API sends `description_zh`, `summary` and `description`
    /// simultaneously. Serde aliases cannot express that (duplicate field), and
    /// the Chinese text is what a Chinese UI should show.
    #[test]
    fn description_prefers_chinese_then_summary() {
        let body = r#"{"results":[{
            "slug":"a","name":"A",
            "description_zh":"中文描述","summary":"english summary","description":"english description"
        }]}"#;
        let skills = parse_search_body(body).expect("should parse");
        assert_eq!(skills[0].summary, "中文描述");
    }

    #[test]
    fn description_falls_back_when_chinese_missing() {
        let body = r#"{"results":[{
            "slug":"a","name":"A","summary":"english summary","description":"english description"
        }]}"#;
        let skills = parse_search_body(body).expect("should parse");
        assert_eq!(skills[0].summary, "english summary");
    }

    #[test]
    fn search_tolerates_missing_optional_fields() {
        let body = r#"{"results":[{"slug":"a","name":"A"}]}"#;
        let skills = parse_search_body(body).expect("minimal row should parse");
        assert_eq!(skills[0].summary, "");
        assert_eq!(skills[0].downloads, None);
        assert_eq!(skills[0].author_label(), "");
        // Falls back to the bare slug when there is no namespace.
        assert_eq!(skills[0].canonical_slug(), "a");
    }

    #[test]
    fn search_empty_results_is_ok() {
        assert!(
            parse_search_body(r#"{"results":[]}"#)
                .expect("empty is not an error")
                .is_empty()
        );
    }

    #[test]
    fn search_surfaces_api_error() {
        let err = parse_search_body(r#"{"error":"rate limited"}"#).unwrap_err();
        assert!(err.to_string().contains("rate limited"));
    }

    #[test]
    fn rankings_single_type_envelope() {
        let body = r#"{"section":"newest","skills":[
            {"slug":"b","name":"B","description":"d2","created_at":200},
            {"slug":"a","name":"A","description":"d1","created_at":100}
        ],"total":2}"#;

        let skills = parse_rankings_body(body, "newest").expect("should parse");
        assert_eq!(skills.len(), 2);
        // "newest" is sorted by creation time, descending.
        assert_eq!(skills[0].slug, "b");
        assert_eq!(skills[0].summary, "d2");
    }

    #[test]
    fn rankings_all_type_nested_map() {
        let body = r#"{"rankings":{
            "hot":[{"slug":"h","name":"H","created_at":1}],
            "newest":[{"slug":"n","name":"N","created_at":9}]
        }}"#;

        let newest = parse_rankings_body(body, "newest").expect("should parse");
        assert_eq!(newest.len(), 1);
        assert_eq!(newest[0].slug, "n");

        let hot = parse_rankings_body(body, "hot").expect("should parse");
        assert_eq!(hot[0].slug, "h");
    }

    #[test]
    fn newest_sorts_missing_timestamps_last() {
        let body = r#"{"skills":[
            {"slug":"no-date","name":"X"},
            {"slug":"dated","name":"Y","created_at":5}
        ]}"#;
        let skills = parse_rankings_body(body, "newest").expect("should parse");
        assert_eq!(skills[0].slug, "dated");
        assert_eq!(skills[1].slug, "no-date");
    }

    #[test]
    fn non_newest_keeps_cli_order() {
        let body = r#"{"skills":[
            {"slug":"older","name":"O","created_at":1},
            {"slug":"newer","name":"N","created_at":99}
        ]}"#;
        let skills = parse_rankings_body(body, "hot").expect("should parse");
        assert_eq!(skills[0].slug, "older");
    }

    #[test]
    fn dir_name_strips_namespace_and_unsafe_chars() {
        assert_eq!(skill_dir_name("@clawhub_jcorrego/openspec"), "openspec");
        assert_eq!(skill_dir_name("plain-name"), "plain-name");
        assert_eq!(skill_dir_name("///"), "skill");
    }

    #[test]
    fn urlencode_escapes_reserved_characters() {
        assert_eq!(urlencode("openspec"), "openspec");
        assert_eq!(urlencode("a b"), "a%20b");
        assert_eq!(urlencode("@a/b?c=1"), "%40a%2Fb%3Fc%3D1");
    }
}
