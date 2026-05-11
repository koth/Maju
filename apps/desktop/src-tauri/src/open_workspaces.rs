use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const FILE_NAME: &str = "open-workspaces.json";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OpenWorkspaceState {
    pub active_path: Option<String>,
    pub workspaces: Vec<OpenWorkspaceRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenWorkspaceRecord {
    pub path: String,
}

pub struct OpenWorkspaces {
    storage_dir: PathBuf,
}

impl OpenWorkspaces {
    pub fn new(storage_dir: PathBuf) -> Self {
        Self { storage_dir }
    }

    fn file_path(&self) -> PathBuf {
        self.storage_dir.join(FILE_NAME)
    }

    pub fn load(&self) -> OpenWorkspaceState {
        let path = self.file_path();
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(_) => return OpenWorkspaceState::default(),
        };
        serde_json::from_str(&content).unwrap_or_default()
    }

    pub fn save(&self, state: &OpenWorkspaceState) {
        let _ = std::fs::create_dir_all(&self.storage_dir);
        let content = serde_json::to_string_pretty(state).unwrap_or_default();
        let _ = std::fs::write(self.file_path(), content);
    }
}
