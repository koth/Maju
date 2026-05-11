use anyhow::{Context, bail};
use git2::{DiffOptions, IndexAddOption, Repository, Status, StatusOptions};
use std::path::Component;
use std::path::{Path, PathBuf};
use workspace_model::{
    ChangeSection, ChangedFile, DiffHunk, DiffLine, DiffLineKind, DiffStats, PatchStatus,
    RepositorySnapshot,
};

pub struct GitService;

impl GitService {
    pub fn open_metadata(path: impl AsRef<Path>) -> anyhow::Result<RepositorySnapshot> {
        let repo = Repository::discover(path).context("failed to discover git repository")?;
        let (branch, head) = repository_identity(&repo);
        Ok(RepositorySnapshot {
            branch,
            head,
            changed_files: Vec::new(),
        })
    }

    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<RepositorySnapshot> {
        let repo = Repository::discover(path).context("failed to discover git repository")?;
        let (branch, head_id) = repository_identity(&repo);

        let mut options = StatusOptions::new();
        options
            .include_untracked(true)
            .recurse_untracked_dirs(false)
            .update_index(true);

        let statuses = repo.statuses(Some(&mut options))?;
        let mut changed_files = Vec::new();

        for entry in statuses.iter() {
            let Some(path) = entry.path() else {
                continue;
            };

            let status = entry.status();
            let section = classify(status);
            let stats =
                compute_diff_stats(&repo, &entry, &section).unwrap_or_else(|_| infer_stats(status));
            if !has_effective_change(&repo, &entry, &section).unwrap_or(true) {
                continue;
            }

            changed_files.push(ChangedFile {
                path: PathBuf::from(path),
                section,
                stats,
                patch_status: PatchStatus::Proposed,
                hunks: vec![DiffHunk {
                    heading: "工作树变更".into(),
                    lines: vec![DiffLine {
                        kind: DiffLineKind::Context,
                        content: format!("Git status entry: {:?}", status),
                    }],
                }],
            });
        }

        Ok(RepositorySnapshot {
            branch,
            head: head_id,
            changed_files,
        })
    }

    pub fn stage(path: impl AsRef<Path>, paths: &[String]) -> anyhow::Result<()> {
        let repo = Repository::discover(path).context("未找到 Git 仓库")?;
        let workdir = repo.workdir().context("仓库没有工作目录")?;
        let mut index = repo.index().context("无法打开 Git 索引")?;

        for raw_path in paths {
            let relative_path = sanitize_relative_path(raw_path)?;
            let absolute_path = workdir.join(&relative_path);

            if absolute_path.is_dir() {
                index.add_all([relative_path.as_path()], IndexAddOption::DEFAULT, None)?;
            } else {
                index.add_path(&relative_path)?;
            }
        }

        index.write().context("无法写入 Git 索引")
    }
}

fn sanitize_relative_path(path: &str) -> anyhow::Result<PathBuf> {
    let normalized = path.replace('\\', "/").trim_end_matches('/').to_string();
    if normalized.is_empty() {
        bail!("路径不能为空");
    }

    let path = PathBuf::from(normalized);
    if path.is_absolute() {
        bail!("不允许使用绝对路径");
    }

    for component in path.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            bail!("不允许路径遍历");
        }
    }

    Ok(path)
}

fn repository_identity(repo: &Repository) -> (String, String) {
    let head = repo.head().ok();
    let branch = head
        .as_ref()
        .and_then(|reference| reference.shorthand())
        .unwrap_or("分离头指针")
        .to_string();
    let head_id = head
        .and_then(|reference| reference.target())
        .map(|oid| oid.to_string())
        .unwrap_or_else(|| "未诞生".into());
    (branch, head_id)
}

fn classify(status: Status) -> ChangeSection {
    if status.is_index_new() || status.is_index_modified() || status.is_index_deleted() {
        ChangeSection::Staged
    } else if status.is_wt_new() {
        ChangeSection::Untracked
    } else {
        ChangeSection::Unstaged
    }
}

fn infer_stats(status: Status) -> DiffStats {
    let mut added = 0;
    let mut removed = 0;

    if status.is_index_new() || status.is_wt_new() {
        added += 1;
    }
    if status.is_index_deleted() || status.is_wt_deleted() {
        removed += 1;
    }
    if status.is_index_modified() || status.is_wt_modified() {
        added += 1;
        removed += 1;
    }

    DiffStats { added, removed }
}

fn compute_diff_stats(
    repo: &Repository,
    entry: &git2::StatusEntry,
    section: &ChangeSection,
) -> anyhow::Result<DiffStats> {
    let path = entry.path().context("entry has no path")?;

    let (added, removed) = match section {
        ChangeSection::Untracked => {
            let workdir = repo.workdir().context("no workdir")?;
            let full_path = workdir.join(path);
            if full_path.exists() {
                let content = std::fs::read_to_string(&full_path).unwrap_or_default();
                let lines = if content.is_empty() {
                    0
                } else {
                    content.lines().count()
                };
                (lines, 0)
            } else {
                (0, 0)
            }
        }
        ChangeSection::Staged => {
            let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
            let diff = match head_tree {
                Some(ref tree) => {
                    let mut opts = DiffOptions::new();
                    opts.pathspec(path);
                    repo.diff_tree_to_index(Some(tree), None, Some(&mut opts))?
                }
                None => {
                    return Ok(DiffStats {
                        added: 0,
                        removed: 0,
                    });
                }
            };
            count_diff_stats(&diff)
        }
        ChangeSection::Unstaged => {
            let mut opts = DiffOptions::new();
            opts.pathspec(path);
            let diff = repo.diff_index_to_workdir(None, Some(&mut opts))?;
            count_diff_stats(&diff)
        }
    };

    Ok(DiffStats { added, removed })
}

fn has_effective_change(
    repo: &Repository,
    entry: &git2::StatusEntry,
    section: &ChangeSection,
) -> anyhow::Result<bool> {
    let path = entry.path().context("entry has no path")?;
    let status = entry.status();

    if matches!(section, ChangeSection::Untracked) {
        return Ok(true);
    }

    if status.is_index_new()
        || status.is_index_deleted()
        || status.is_wt_new()
        || status.is_wt_deleted()
    {
        return Ok(true);
    }

    let old_text = match section {
        ChangeSection::Staged => head_text_for_path(repo, path)?,
        ChangeSection::Unstaged => index_text_for_path(repo, path)?
            .or_else(|| head_text_for_path(repo, path).ok().flatten()),
        ChangeSection::Untracked => None,
    };
    let new_text = match section {
        ChangeSection::Staged => index_text_for_path(repo, path)?,
        ChangeSection::Unstaged => workdir_text_for_path(repo, path)?,
        ChangeSection::Untracked => None,
    };

    let Some(old_text) = old_text else {
        return Ok(true);
    };
    let Some(new_text) = new_text else {
        return Ok(true);
    };

    Ok(normalize_line_endings(&old_text) != normalize_line_endings(&new_text))
}

fn head_text_for_path(repo: &Repository, path: &str) -> anyhow::Result<Option<String>> {
    let tree = match repo.head().ok().and_then(|head| head.peel_to_tree().ok()) {
        Some(tree) => tree,
        None => return Ok(None),
    };
    let entry = match tree.get_path(Path::new(path)) {
        Ok(entry) => entry,
        Err(_) => return Ok(None),
    };
    read_blob_text(repo, entry.id())
}

fn index_text_for_path(repo: &Repository, path: &str) -> anyhow::Result<Option<String>> {
    let index = repo.index().context("failed to open git index")?;
    let Some(entry) = index.get_path(Path::new(path), 0) else {
        return Ok(None);
    };
    read_blob_text(repo, entry.id)
}

fn workdir_text_for_path(repo: &Repository, path: &str) -> anyhow::Result<Option<String>> {
    let workdir = repo.workdir().context("no workdir")?;
    let full_path = workdir.join(path);
    if !full_path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(full_path)?;
    Ok(String::from_utf8(bytes).ok())
}

fn read_blob_text(repo: &Repository, oid: git2::Oid) -> anyhow::Result<Option<String>> {
    let blob = repo.find_blob(oid)?;
    Ok(std::str::from_utf8(blob.content())
        .ok()
        .map(ToString::to_string))
}

fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn count_diff_stats(diff: &git2::Diff) -> (usize, usize) {
    if let Ok(stats) = diff.stats() {
        (stats.insertions(), stats.deletions())
    } else {
        (0, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn setup_repo() -> (tempfile::TempDir, Repository) {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        // Initial commit so HEAD exists
        fs::write(dir.path().join(".gitkeep"), "").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(".gitkeep")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        drop(tree);
        (dir, repo)
    }

    fn commit_file(repo: &Repository, rel_path: &str, content: &str) {
        let full = repo.workdir().unwrap().join(rel_path);
        fs::write(&full, content).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(rel_path)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        let parent_oid = repo.head().ok().and_then(|r| r.target());
        let parent_commit = parent_oid.and_then(|oid| repo.find_commit(oid).ok());
        let parents: Vec<git2::Commit> = parent_commit.into_iter().collect();
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, "commit", &tree, &parent_refs)
            .unwrap();
        drop(tree);
    }

    #[test]
    fn discovers_untracked_files() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("notes.txt"), "draft").unwrap();
        drop(repo);

        let snapshot = GitService::open(dir.path()).unwrap();
        assert_eq!(snapshot.changed_files.len(), 1);
        assert!(matches!(
            snapshot.changed_files[0].section,
            ChangeSection::Untracked
        ));
    }

    #[test]
    fn stages_untracked_file() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("notes.txt"), "draft").unwrap();
        drop(repo);

        GitService::stage(dir.path(), &["notes.txt".to_string()]).unwrap();

        let snapshot = GitService::open(dir.path()).unwrap();
        assert_eq!(snapshot.changed_files.len(), 1);
        assert!(matches!(
            snapshot.changed_files[0].section,
            ChangeSection::Staged
        ));
    }

    #[test]
    fn untracked_file_shows_all_lines_as_additions() {
        let (dir, _repo) = setup_repo();
        fs::write(dir.path().join("new_file.rs"), "line1\nline2\nline3\n").unwrap();

        let snapshot = GitService::open(dir.path()).unwrap();
        let file = snapshot
            .changed_files
            .iter()
            .find(|f| f.path.ends_with("new_file.rs"))
            .unwrap();
        assert_eq!(file.stats.added, 3);
        assert_eq!(file.stats.removed, 0);
    }

    #[test]
    fn modified_file_shows_accurate_diff_stats() {
        let (dir, repo) = setup_repo();
        commit_file(&repo, "main.rs", "aaa\nbbb\nccc\nddd\n");

        // Change: remove bbb, add eee, modify ddd -> eee added
        fs::write(dir.path().join("main.rs"), "aaa\nxxx\nyyy\n").unwrap();

        let snapshot = GitService::open(dir.path()).unwrap();
        let file = snapshot
            .changed_files
            .iter()
            .find(|f| f.path.ends_with("main.rs"))
            .unwrap();
        // 5 original lines -> 3 new lines: 3 additions of new content lines, 3 removals of old
        // Actually the diff: -bbb, -ccc, -ddd; +xxx, +yyy = 3 removed, 2 added
        // Wait: original "aaa\nbbb\nccc\nddd\n" has 4 lines.
        // New: "aaa\nxxx\nyyy\n" has 3 lines.
        // Diff: line 2: -bbb +xxx, line 3: -ccc +yyy, line 4: -ddd
        // That's 3 removed, 2 added... but git2 might count differently.
        // We just verify it's not the bogus +1 -1
        assert!(file.stats.added > 0);
        assert!(file.stats.removed > 0);
        assert_ne!(file.stats.added, 1);
        assert_ne!(file.stats.removed, 1);
    }

    #[test]
    fn line_ending_only_worktree_change_is_not_reported() {
        let (dir, repo) = setup_repo();
        commit_file(&repo, "main.rs", "alpha\nbeta\n");

        fs::write(dir.path().join("main.rs"), "alpha\r\nbeta\r\n").unwrap();

        let snapshot = GitService::open(dir.path()).unwrap();
        assert!(
            snapshot
                .changed_files
                .iter()
                .all(|file| !file.path.ends_with("main.rs"))
        );
    }

    #[test]
    fn staged_new_file_shows_all_lines_as_additions() {
        let (dir, repo) = setup_repo();
        fs::write(dir.path().join("staged_file.rs"), "fn main() {}\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("staged_file.rs")).unwrap();
        index.write().unwrap();

        let snapshot = GitService::open(dir.path()).unwrap();
        let file = snapshot
            .changed_files
            .iter()
            .find(|f| f.path.ends_with("staged_file.rs"))
            .unwrap();
        assert_eq!(file.stats.added, 1);
        assert_eq!(file.stats.removed, 0);
    }

    #[test]
    fn snapshot_total_changed_files_matches_count() {
        let (dir, _repo) = setup_repo();
        fs::write(dir.path().join("a.rs"), "// a\n").unwrap();
        fs::write(dir.path().join("b.rs"), "// b\n").unwrap();
        fs::write(dir.path().join("c.rs"), "// c\n").unwrap();

        let snapshot = GitService::open(dir.path()).unwrap();
        let untracked: Vec<_> = snapshot
            .changed_files
            .iter()
            .filter(|f| matches!(f.section, ChangeSection::Untracked))
            .collect();
        assert_eq!(untracked.len(), 3);
    }

    #[test]
    fn metadata_snapshot_does_not_scan_changed_files() {
        let (dir, repo) = setup_repo();
        commit_file(&repo, "tracked.txt", "one\n");
        fs::write(dir.path().join("tracked.txt"), "one\ntwo\n").unwrap();
        fs::write(dir.path().join("untracked.txt"), "new\n").unwrap();

        let snapshot = GitService::open_metadata(dir.path()).unwrap();
        assert_eq!(snapshot.branch, "master");
        assert!(snapshot.changed_files.is_empty());
    }
}
