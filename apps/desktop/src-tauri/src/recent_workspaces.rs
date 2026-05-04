use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MAX_RECENT: usize = 10;
const FILE_NAME: &str = "recent-workspaces.json";

#[derive(Debug, Serialize, Deserialize)]
pub struct RecentEntry {
    pub path: String,
    pub exists: bool,
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
        let paths: Vec<String> = serde_json::from_str(&content).unwrap_or_default();
        paths
            .into_iter()
            .map(|p| {
                let exists = Path::new(&p).is_dir();
                RecentEntry { path: p, exists }
            })
            .collect()
    }

    fn load_paths(&self) -> Vec<String> {
        let path = self.file_path();
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        serde_json::from_str(&content).unwrap_or_default()
    }

    fn save(&self, paths: &[String]) {
        let _ = std::fs::create_dir_all(&self.storage_dir);
        let content = serde_json::to_string_pretty(paths).unwrap_or_default();
        let _ = std::fs::write(self.file_path(), content);
    }

    pub fn add(&self, path: &str) {
        let mut paths = self.load_paths();
        paths.retain(|p| p != path);
        paths.insert(0, path.to_string());
        paths.truncate(MAX_RECENT);
        self.save(&paths);
    }

    pub fn remove(&self, path: &str) {
        let mut paths = self.load_paths();
        paths.retain(|p| p != path);
        self.save(&paths);
    }
}
