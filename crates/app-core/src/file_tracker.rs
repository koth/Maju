use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use workspace_model::{DiffHunk, FileChangeType};

/// Directories to skip when scanning for changed files.
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".kodex"];

/// Maximum file size (bytes) for inline diff generation. Larger files are tracked without hunks.
const MAX_DIFF_FILE_SIZE: u64 = 512 * 1024;

/// Maximum number of candidate paths to verify per tool completion.
const MAX_CANDIDATES_PER_TOOL: usize = 64;

/// Lightweight metadata snapshot for a single file.
#[derive(Debug, Clone)]
struct FileMeta {
    mtime: SystemTime,
    len: u64,
    fingerprint: u64,
}

/// A scoped recording window that captures baseline file metadata when a tool
/// starts and compares against the filesystem when the tool finishes.
#[derive(Debug, Clone)]
pub(crate) struct ToolRecordingWindow {
    baseline: HashMap<String, FileMeta>,
    baseline_text: HashMap<String, String>,
    /// Candidate paths observed during execution (from watcher hints or tool input).
    candidates: Vec<String>,
}

/// Session-level tracker that manages per-tool recording windows and a
/// reusable workspace metadata index.
pub(crate) struct FileChangeTracker {
    workspace_root: PathBuf,
    /// Metadata index built from the workspace, reused across tool calls.
    index: HashMap<String, FileMeta>,
    /// Currently active recording windows, keyed by call_id.
    active_windows: HashMap<String, ToolRecordingWindow>,
}

fn file_fingerprint(path: &Path, len: u64) -> u64 {
    if len > MAX_DIFF_FILE_SIZE {
        return len;
    }

    let Ok(bytes) = fs::read(path) else {
        return len;
    };

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

impl FileChangeTracker {
    pub(crate) fn new(workspace_root: &Path) -> Self {
        let mut tracker = Self {
            workspace_root: workspace_root.to_path_buf(),
            index: HashMap::new(),
            active_windows: HashMap::new(),
        };
        tracker.rebuild_index();
        tracker
    }

    /// Rebuild the workspace metadata index from scratch.
    fn rebuild_index(&mut self) {
        self.index.clear();
        let root = self.workspace_root.clone();
        self.scan_dir(&root, 0);
    }

    fn scan_dir(&mut self, dir: &Path, depth: usize) {
        if depth > 10 {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if SKIP_DIRS.contains(&name_str.as_ref()) {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                self.scan_dir(&path, depth + 1);
            } else if let Ok(meta) = fs::metadata(&path) {
                let rel = path
                    .strip_prefix(&self.workspace_root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                self.index.insert(
                    rel.clone(),
                    FileMeta {
                        mtime: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                        len: meta.len(),
                        fingerprint: file_fingerprint(&path, meta.len()),
                    },
                );
            }
        }
    }

    /// Start a recording window for a tool call. Captures current file metadata.
    pub(crate) fn start_recording(&mut self, call_id: &str, hint_paths: Vec<String>) {
        // A poll batch may contain ToolStarted followed by ToolDiff for the same
        // call. Application pre-starts recording before diff preprocessing so the
        // ToolDiff can use the tool-start baseline; the normal event-application
        // pass will see ToolStarted again. Keep the earliest baseline.
        if self.active_windows.contains_key(call_id) {
            return;
        }

        let hint_paths = hint_paths
            .into_iter()
            .map(|path| self.normalize_candidate_path(&path))
            .filter(|path| !path.is_empty())
            .collect::<Vec<_>>();

        let mut baseline = HashMap::new();
        for path in &hint_paths {
            if let Some(meta) = self.index.get(path) {
                baseline.insert(path.clone(), meta.clone());
            }
        }
        // If no hint paths, snapshot the full index for broad comparison.
        if baseline.is_empty() {
            baseline = self.index.clone();
        }

        let baseline_text = self.capture_baseline_text(&baseline);
        self.active_windows.insert(
            call_id.to_string(),
            ToolRecordingWindow {
                baseline,
                baseline_text,
                candidates: hint_paths,
            },
        );
    }

    /// Add a candidate path hint during tool execution.
    pub(crate) fn add_candidate(&mut self, call_id: &str, path: String) {
        let path = self.normalize_candidate_path(&path);
        if path.is_empty() {
            return;
        }
        if let Some(window) = self.active_windows.get_mut(call_id) {
            if !window.candidates.contains(&path) {
                window.candidates.push(path);
            }
        }
    }

    /// Look up the baseline text for a given path across all active recording windows.
    /// Returns the content that was on disk when the tool started, suitable for
    /// computing a "what did this tool change?" diff.
    pub(crate) fn get_any_baseline_text(&self, path: &str) -> Option<&str> {
        let normalized = self.normalize_candidate_path(path);
        for window in self.active_windows.values() {
            if let Some(text) = window.baseline_text.get(&normalized) {
                return Some(text.as_str());
            }
        }
        None
    }

    fn normalize_candidate_path(&self, path: &str) -> String {
        let normalized = path.replace('\\', "/");
        let root = self.workspace_root.to_string_lossy().replace('\\', "/");
        let root_prefix = if root.ends_with('/') {
            root
        } else {
            format!("{root}/")
        };
        normalized
            .strip_prefix(&root_prefix)
            .unwrap_or(&normalized)
            .trim_start_matches("./")
            .to_string()
    }

    fn capture_baseline_text(
        &self,
        baseline: &HashMap<String, FileMeta>,
    ) -> HashMap<String, String> {
        let mut texts = HashMap::new();
        for (path, meta) in baseline {
            if meta.len > MAX_DIFF_FILE_SIZE {
                continue;
            }
            let full_path = self.workspace_root.join(path);
            if let Ok(text) = fs::read_to_string(full_path) {
                texts.insert(path.clone(), text);
            }
        }
        texts
    }

    /// Finish recording for a tool call. Compares before/after state and returns
    /// verified changed files with optional diff hunks.
    pub(crate) fn finish_recording(&mut self, call_id: &str) -> Vec<VerifiedFileChange> {
        let Some(window) = self.active_windows.remove(call_id) else {
            return Vec::new();
        };

        // Refresh the index to get current state
        self.rebuild_index();

        // Determine candidate paths: prefer explicit candidates, fall back to index diff
        let candidates = if !window.candidates.is_empty() {
            window.candidates
        } else {
            // Compare index against baseline to find changed paths
            let mut changed = Vec::new();
            for (path, current_meta) in &self.index {
                if let Some(base_meta) = window.baseline.get(path) {
                    if current_meta.mtime != base_meta.mtime || current_meta.len != base_meta.len {
                        changed.push(path.clone());
                    }
                }
            }
            // Also check for files that existed in baseline but are now gone
            for path in window.baseline.keys() {
                if !self.index.contains_key(path) {
                    changed.push(path.clone());
                }
            }
            changed
        };

        let mut results = Vec::new();
        let limit = candidates.len().min(MAX_CANDIDATES_PER_TOOL);

        for path in &candidates[..limit] {
            let full_path = self.workspace_root.join(path);
            let base_meta = window.baseline.get(path);

            // Check workspace boundary
            if !full_path.starts_with(&self.workspace_root) {
                continue;
            }

            if full_path.exists() {
                let file_meta = fs::metadata(&full_path);
                let is_changed = match (base_meta, file_meta.as_ref()) {
                    (Some(base), Ok(current)) => {
                        let mtime = current.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                        let len = current.len();
                        let fingerprint = file_fingerprint(&full_path, len);
                        mtime != base.mtime || len != base.len || fingerprint != base.fingerprint
                    }
                    (None, Ok(_)) => true, // new file
                    (_, Err(_)) => false,
                };

                if !is_changed {
                    continue;
                }

                let len = file_meta.map(|m| m.len()).unwrap_or(0);
                let old_text = window.baseline_text.get(path).cloned();

                if len > MAX_DIFF_FILE_SIZE {
                    results.push(VerifiedFileChange {
                        path: path.clone(),
                        change_type: if base_meta.is_some() {
                            FileChangeType::Modified
                        } else {
                            FileChangeType::Created
                        },
                        old_text,
                        new_text: String::new(),
                        hunks: Vec::new(),
                        skipped_diff: true,
                    });
                    continue;
                }

                let Ok(new_text) = fs::read_to_string(&full_path) else {
                    results.push(VerifiedFileChange {
                        path: path.clone(),
                        change_type: if base_meta.is_some() {
                            FileChangeType::Modified
                        } else {
                            FileChangeType::Created
                        },
                        old_text,
                        new_text: String::new(),
                        hunks: Vec::new(),
                        skipped_diff: true,
                    });
                    continue;
                };

                if base_meta.is_some() {
                    let Some(old_text) = old_text else {
                        results.push(VerifiedFileChange {
                            path: path.clone(),
                            change_type: FileChangeType::Modified,
                            old_text: None,
                            new_text: String::new(),
                            hunks: Vec::new(),
                            skipped_diff: true,
                        });
                        continue;
                    };
                    if old_text == new_text {
                        continue;
                    }
                    results.push(VerifiedFileChange {
                        path: path.clone(),
                        change_type: FileChangeType::Modified,
                        old_text: Some(old_text),
                        new_text,
                        hunks: Vec::new(),
                        skipped_diff: false,
                    });
                } else {
                    results.push(VerifiedFileChange {
                        path: path.clone(),
                        change_type: FileChangeType::Created,
                        old_text: None,
                        new_text,
                        hunks: Vec::new(),
                        skipped_diff: false,
                    });
                }
            } else if base_meta.is_some() {
                results.push(VerifiedFileChange {
                    path: path.clone(),
                    change_type: FileChangeType::Deleted,
                    old_text: window.baseline_text.get(path).cloned(),
                    new_text: String::new(),
                    hunks: Vec::new(),
                    skipped_diff: true,
                });
            }
        }

        results
    }
}

#[derive(Debug, Clone)]
pub(crate) struct VerifiedFileChange {
    pub(crate) path: String,
    pub(crate) change_type: FileChangeType,
    pub(crate) old_text: Option<String>,
    pub(crate) new_text: String,
    pub(crate) hunks: Vec<DiffHunk>,
    pub(crate) skipped_diff: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn no_candidates_returns_empty() {
        let dir = tempdir();
        let mut tracker = FileChangeTracker::new(dir.path());
        tracker.start_recording("call-1", Vec::new());
        let changes = tracker.finish_recording("call-1");
        assert!(changes.is_empty());
    }

    #[test]
    fn unchanged_metadata_skips_diffing() {
        let dir = tempdir();
        let file_path = dir.path().join("foo.txt");
        fs::write(&file_path, "hello").unwrap();

        let mut tracker = FileChangeTracker::new(dir.path());
        tracker.start_recording("call-1", vec!["foo.txt".to_string()]);
        let changes = tracker.finish_recording("call-1");
        assert!(changes.is_empty());
    }

    #[test]
    fn get_any_baseline_text_returns_tool_start_content() {
        let dir = tempdir();
        let file_path = dir.path().join("foo.txt");
        fs::write(&file_path, "before").unwrap();

        let mut tracker = FileChangeTracker::new(dir.path());
        tracker.start_recording("call-1", vec!["foo.txt".to_string()]);

        fs::write(&file_path, "after").unwrap();

        assert_eq!(tracker.get_any_baseline_text("foo.txt"), Some("before"));
    }

    #[test]
    fn modified_file_is_detected() {
        let dir = tempdir();
        let file_path = dir.path().join("foo.txt");
        fs::write(&file_path, "hello").unwrap();

        let mut tracker = FileChangeTracker::new(dir.path());
        tracker.start_recording("call-1", vec!["foo.txt".to_string()]);

        fs::write(&file_path, "hello world").unwrap();

        let changes = tracker.finish_recording("call-1");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "foo.txt");
        assert_eq!(changes[0].change_type, FileChangeType::Modified);
        assert_eq!(changes[0].old_text.as_deref(), Some("hello"));
        assert_eq!(changes[0].new_text, "hello world");
        assert!(!changes[0].skipped_diff);
    }

    #[test]
    fn modified_file_is_detected_even_when_metadata_timestamp_does_not_change() {
        let dir = tempdir();
        let file_path = dir.path().join("foo.txt");
        fs::write(&file_path, "hello").unwrap();
        let original_meta = fs::metadata(&file_path).unwrap();
        let original_mtime = original_meta.modified().unwrap();

        let mut tracker = FileChangeTracker::new(dir.path());
        tracker.start_recording("call-1", vec!["foo.txt".to_string()]);

        fs::write(&file_path, "world").unwrap();
        filetime::set_file_mtime(
            &file_path,
            filetime::FileTime::from_system_time(original_mtime),
        )
        .unwrap();

        let changes = tracker.finish_recording("call-1");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "foo.txt");
        assert_eq!(changes[0].change_type, FileChangeType::Modified);
        assert_eq!(changes[0].old_text.as_deref(), Some("hello"));
        assert_eq!(changes[0].new_text, "world");
        assert!(!changes[0].skipped_diff);
    }

    #[test]
    fn new_file_is_detected_as_created() {
        let dir = tempdir();
        let mut tracker = FileChangeTracker::new(dir.path());
        tracker.start_recording("call-1", vec!["new_file.txt".to_string()]);

        fs::write(dir.path().join("new_file.txt"), "content").unwrap();

        let changes = tracker.finish_recording("call-1");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change_type, FileChangeType::Created);
        assert!(changes[0].old_text.is_none());
    }

    #[test]
    fn deleted_file_is_detected() {
        let dir = tempdir();
        let file_path = dir.path().join("del.txt");
        fs::write(&file_path, "bye").unwrap();

        let mut tracker = FileChangeTracker::new(dir.path());
        tracker.start_recording("call-1", vec!["del.txt".to_string()]);

        fs::remove_file(&file_path).unwrap();

        let changes = tracker.finish_recording("call-1");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change_type, FileChangeType::Deleted);
        assert!(changes[0].skipped_diff);
    }

    #[test]
    fn large_file_skips_inline_diff() {
        let dir = tempdir();
        let file_path = dir.path().join("big.txt");
        let big_content = "x".repeat(600 * 1024);
        fs::write(&file_path, "small").unwrap();

        let mut tracker = FileChangeTracker::new(dir.path());
        tracker.start_recording("call-1", vec!["big.txt".to_string()]);

        fs::write(&file_path, &big_content).unwrap();

        let changes = tracker.finish_recording("call-1");
        assert_eq!(changes.len(), 1);
        assert!(changes[0].skipped_diff);
        assert!(changes[0].new_text.is_empty());
    }

    #[test]
    fn binary_file_not_utf8_skips_inline_diff() {
        let dir = tempdir();
        let file_path = dir.path().join("bin.dat");
        fs::write(&file_path, "valid utf8").unwrap();

        let mut tracker = FileChangeTracker::new(dir.path());
        tracker.start_recording("call-1", vec!["bin.dat".to_string()]);

        fs::write(&file_path, [0x80, 0x81, 0x82]).unwrap();

        let changes = tracker.finish_recording("call-1");
        assert_eq!(changes.len(), 1);
        assert!(changes[0].skipped_diff);
    }

    #[test]
    fn skipped_dirs_are_ignored() {
        let dir = tempdir();
        let git_dir = dir.path().join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        fs::write(git_dir.join("config"), "old").unwrap();

        let mut tracker = FileChangeTracker::new(dir.path());
        tracker.start_recording("call-1", Vec::new());

        fs::write(git_dir.join("config"), "new").unwrap();

        let changes = tracker.finish_recording("call-1");
        assert!(changes.is_empty());
    }

    #[test]
    fn finish_unknown_call_id_returns_empty() {
        let dir = tempdir();
        let mut tracker = FileChangeTracker::new(dir.path());
        let changes = tracker.finish_recording("nonexistent");
        assert!(changes.is_empty());
    }

    #[test]
    fn over_budget_candidates_are_truncated() {
        let dir = tempdir();
        let mut paths = Vec::new();
        for i in 0..70 {
            let name = format!("file_{:03}.txt", i);
            let path = dir.path().join(&name);
            fs::write(&path, format!("content {}", i)).unwrap();
            paths.push(name);
        }

        let mut tracker = FileChangeTracker::new(dir.path());
        tracker.start_recording("call-1", paths);

        for i in 0..70 {
            let name = format!("file_{:03}.txt", i);
            let path = dir.path().join(&name);
            fs::write(&path, format!("modified {}", i)).unwrap();
        }

        let changes = tracker.finish_recording("call-1");
        assert!(changes.len() <= MAX_CANDIDATES_PER_TOOL);
    }
}
