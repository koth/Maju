use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use workspace_model::RemoteLinuxWorkspace;

const MAX_RECENT: usize = 10;
const FILE_NAME: &str = "recent-workspaces.json";

#[derive(Debug, Serialize, Deserialize)]
pub struct RecentEntry {
    pub path: String,
    pub exists: bool,
    #[serde(default)]
    pub remote: Option<RemoteLinuxWorkspace>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RecentRecord {
    path: String,
    #[serde(default)]
    remote: Option<RemoteLinuxWorkspace>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RecentFile {
    Records(Vec<RecentRecord>),
    Paths(Vec<String>),
}

pub struct RecentWorkspaces {
    storage_dir: PathBuf,
}

impl RecentWorkspaces {
    pub fn new(storage_dir: PathBuf) -> Self {
        Self { storage_dir }
    }

    fn file_path(&self) -> PathBuf {
        self.storage_dir.join(FILE_NAME)
    }

    pub fn load(&self) -> Vec<RecentEntry> {
        let path = self.file_path();
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let records = parse_recent_records(&content);
        app_core::startup_perf::mark(
            "recent_workspaces/load_paths",
            format!("count={}", records.len()),
        );
        records
            .into_iter()
            .map(|record| {
                let exists = if record.remote.is_some() {
                    true
                } else {
                    app_core::startup_perf::measure(
                        "recent_workspaces/is_dir",
                        &record.path,
                        || Path::new(&record.path).is_dir(),
                    )
                };
                RecentEntry {
                    path: record.path,
                    exists,
                    remote: record.remote,
                }
            })
            .collect()
    }

    fn load_records(&self) -> Vec<RecentRecord> {
        let path = self.file_path();
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        parse_recent_records(&content)
    }

    fn save(&self, records: &[RecentRecord]) {
        let _ = std::fs::create_dir_all(&self.storage_dir);
        let content = serde_json::to_string_pretty(records).unwrap_or_default();
        let _ = std::fs::write(self.file_path(), content);
    }

    pub fn add(&self, path: &str) {
        let mut records = self.load_records();
        records.retain(|record| record.path != path);
        records.insert(
            0,
            RecentRecord {
                path: path.to_string(),
                remote: None,
            },
        );
        records.truncate(MAX_RECENT);
        self.save(&records);
    }

    pub fn add_remote(&self, remote: RemoteLinuxWorkspace) {
        let path = remote.key();
        let mut records = self.load_records();
        records.retain(|record| record.path != path);
        records.insert(
            0,
            RecentRecord {
                path,
                remote: Some(remote),
            },
        );
        records.truncate(MAX_RECENT);
        self.save(&records);
    }

    pub fn remove(&self, path: &str) {
        let mut records = self.load_records();
        records.retain(|record| record.path != path);
        self.save(&records);
    }
}

fn parse_recent_records(content: &str) -> Vec<RecentRecord> {
    match serde_json::from_str::<RecentFile>(content) {
        Ok(RecentFile::Records(records)) => records,
        Ok(RecentFile::Paths(paths)) => paths
            .into_iter()
            .map(|path| RecentRecord { path, remote: None })
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use workspace_model::AgentCliId;

    fn remote_fixture() -> RemoteLinuxWorkspace {
        RemoteLinuxWorkspace {
            profile_id: None,
            ssh_target: "devbox".into(),
            ssh_port: None,
            remote_path: "/srv/project".into(),
            ssh_password: None,
            agent_cli: Some(AgentCliId::CodexAcp),
            agent_command: Some("codex-acp".into()),
            local_port: None,
            remote_port: None,
        }
    }

    #[test]
    fn parse_recent_records_accepts_legacy_path_array() {
        let records = parse_recent_records(r#"["D:\\work\\kodex"]"#);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].path, "D:\\work\\kodex");
        assert!(records[0].remote.is_none());
    }

    #[test]
    fn remote_recent_entries_are_reported_without_local_path_probe() {
        let storage_dir =
            std::env::temp_dir().join(format!("kodex-recent-test-{}", uuid::Uuid::new_v4()));
        let recent = RecentWorkspaces::new(storage_dir.clone());
        let remote = remote_fixture();

        recent.add_remote(remote.clone());
        let entries = recent.load();

        let _ = std::fs::remove_dir_all(storage_dir);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, remote.key());
        assert!(entries[0].exists);
        assert_eq!(entries[0].remote.as_ref(), Some(&remote));
    }
}
