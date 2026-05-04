use anyhow::{Context, bail};
use git2::{IndexAddOption, Repository, Status, StatusOptions};
use std::path::Component;
use std::path::{Path, PathBuf};
use workspace_model::{
    ChangeSection, ChangedFile, DiffHunk, DiffLine, DiffLineKind, DiffStats, PatchStatus,
    RepositorySnapshot,
};

pub struct GitService;

impl GitService {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<RepositorySnapshot> {
        let repo = Repository::discover(path).context("failed to discover git repository")?;
        let head = repo.head().ok();
        let branch = head
            .as_ref()
            .and_then(|reference| reference.shorthand())
            .unwrap_or("detached")
            .to_string();
        let head_id = head
            .and_then(|reference| reference.target())
            .map(|oid| oid.to_string())
            .unwrap_or_else(|| "unborn".into());

        let mut options = StatusOptions::new();
        options
            .include_untracked(true)
            .recurse_untracked_dirs(false);

        let statuses = repo.statuses(Some(&mut options))?;
        let mut changed_files = Vec::new();

        for entry in statuses.iter() {
            let Some(path) = entry.path() else {
                continue;
            };

            let status = entry.status();
            let section = classify(status);
            let stats = infer_stats(status);

            changed_files.push(ChangedFile {
                path: PathBuf::from(path),
                section,
                stats,
                patch_status: PatchStatus::Proposed,
                hunks: vec![DiffHunk {
                    heading: "Working tree changes".into(),
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
        let repo = Repository::discover(path).context("failed to discover git repository")?;
        let workdir = repo
            .workdir()
            .context("repository has no working directory")?;
        let mut index = repo.index().context("failed to open git index")?;

        for raw_path in paths {
            let relative_path = sanitize_relative_path(raw_path)?;
            let absolute_path = workdir.join(&relative_path);

            if absolute_path.is_dir() {
                index.add_all([relative_path.as_path()], IndexAddOption::DEFAULT, None)?;
            } else {
                index.add_path(&relative_path)?;
            }
        }

        index.write().context("failed to write git index")
    }
}

fn sanitize_relative_path(path: &str) -> anyhow::Result<PathBuf> {
    let normalized = path.replace('\\', "/").trim_end_matches('/').to_string();
    if normalized.is_empty() {
        bail!("path cannot be empty");
    }

    let path = PathBuf::from(normalized);
    if path.is_absolute() {
        bail!("absolute paths are not allowed");
    }

    for component in path.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            bail!("path traversal is not allowed");
        }
    }

    Ok(path)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

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
}
