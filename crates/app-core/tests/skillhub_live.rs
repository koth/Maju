//! Live end-to-end check of the SkillHub HTTP path: search → download → extract.
//!
//! Requires network access. Run explicitly:
//!   cargo test -p app-core --test skillhub_live -- --ignored --nocapture

use std::path::PathBuf;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("maju-skillhub-live-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[tokio::test]
#[ignore = "hits the live SkillHub API"]
async fn search_returns_relevant_hits_with_full_metadata() {
    let hits = app_core::skillhub::search_skills("openspec")
        .await
        .expect("search failed");
    assert!(!hits.is_empty(), "expected hits for 'openspec'");
    assert!(
        hits.iter().any(|h| h.slug.contains("openspec")),
        "expected an openspec hit, got: {:?}",
        hits.iter().map(|h| &h.slug).collect::<Vec<_>>()
    );

    let first = &hits[0];
    println!(
        "top hit: {} ({}) dl={:?} created={:?} author={}",
        first.name,
        first.slug,
        first.downloads,
        first.created_at,
        first.author_label()
    );
    // The direct API returns complete records; the CLI used to strip these.
    assert!(first.downloads.is_some(), "downloads should be present");
    assert!(first.created_at.is_some(), "created_at should be present");
    assert!(!first.summary.is_empty(), "summary should be present");
    assert!(!first.author_label().is_empty(), "author should resolve");
}

#[tokio::test]
#[ignore = "hits the live SkillHub API"]
async fn empty_query_returns_default_popular_listing() {
    let popular = app_core::skillhub::search_skills("")
        .await
        .expect("default listing failed");
    assert!(!popular.is_empty(), "default listing should not be empty");
    println!(
        "default listing: {} hits, first = {}",
        popular.len(),
        popular[0].name
    );
    assert!(
        popular[0].downloads.unwrap_or(0) > 0,
        "default listing should carry download counts"
    );
}

#[tokio::test]
#[ignore = "hits the live SkillHub API"]
async fn install_downloads_and_extracts_skill() {
    let dest = temp_dir("install");
    let path = app_core::skillhub::install_skill("openspec", &dest)
        .await
        .expect("install failed");

    assert!(
        path.join("SKILL.md").exists(),
        "SKILL.md should be at the root"
    );
    let skill_md = std::fs::read_to_string(path.join("SKILL.md")).expect("read SKILL.md");
    assert!(
        skill_md.starts_with("---"),
        "SKILL.md should start with YAML frontmatter"
    );
    assert!(
        skill_md.contains("name:"),
        "SKILL.md frontmatter should carry a name"
    );
    println!(
        "installed to {} ({} bytes of SKILL.md)",
        path.display(),
        skill_md.len()
    );

    // No staging folder may survive a successful install.
    let leftovers: Vec<_> = std::fs::read_dir(&dest)
        .expect("read dest")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with('.'))
        .collect();
    assert!(
        leftovers.is_empty(),
        "staging folders left behind: {leftovers:?}"
    );

    let _ = std::fs::remove_dir_all(&dest);
}

#[tokio::test]
#[ignore = "hits the live SkillHub API"]
async fn install_rejects_unknown_slug() {
    let dest = temp_dir("missing");
    let result =
        app_core::skillhub::install_skill("definitely-not-a-real-skill-xyz-123", &dest).await;
    assert!(result.is_err(), "unknown slug should fail, got {result:?}");
    let _ = std::fs::remove_dir_all(&dest);
}
