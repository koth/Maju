use anyhow::{Context, Result, anyhow};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const KODEX_DATA_ROOT_ENV: &str = "KODEX_DATA_ROOT";

#[derive(Clone, Debug)]
pub struct AppPaths {
    root: PathBuf,
}

impl AppPaths {
    pub fn resolve() -> Result<Self> {
        Self::resolve_with_data_root(std::env::var_os(KODEX_DATA_ROOT_ENV))
    }

    fn resolve_with_data_root(data_root: Option<OsString>) -> Result<Self> {
        if let Some(root) = data_root.filter(|root| !root.is_empty()) {
            return Ok(Self::from_root(PathBuf::from(root)));
        }
        let home = dirs_next::home_dir().ok_or_else(|| anyhow!("无法解析当前用户的主目录"))?;
        Ok(Self::from_root(home.join(".kodex")))
    }

    pub fn from_root(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config_dir(&self) -> PathBuf {
        self.root.join("config")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    pub fn attachments_dir(&self) -> PathBuf {
        self.root.join("attachments")
    }

    pub fn workspaces_dir(&self) -> PathBuf {
        self.root.join("workspaces")
    }

    /// Workspace root for project-less chat sessions (`~/.kodex/chats`).
    /// Sessions created here are not bound to a real project directory and
    /// surface under the "聊天" group in the sidebar.
    pub fn chats_workspace_root(&self) -> PathBuf {
        self.root.join("chats")
    }

    pub fn ensure_root(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("创建 Kodex 数据根目录 {} 失败", self.root.display()))
    }

    pub fn ensure_standard_dirs(&self) -> Result<()> {
        self.ensure_root()?;
        for dir in [
            self.config_dir(),
            self.logs_dir(),
            self.sessions_dir(),
            self.attachments_dir(),
            self.workspaces_dir(),
        ] {
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("创建 Kodex 数据目录 {} 失败", dir.display()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_with_data_root_override_uses_exact_root() {
        let root = PathBuf::from("C:/tmp/kodex-data-root-test");

        let paths = AppPaths::resolve_with_data_root(Some(root.as_os_str().to_os_string()))
            .expect("override path should resolve");

        assert_eq!(paths.root(), root.as_path());
        assert_eq!(paths.logs_dir(), root.join("logs"));
    }
}
