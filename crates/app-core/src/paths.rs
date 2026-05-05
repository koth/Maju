use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct AppPaths {
    root: PathBuf,
}

impl AppPaths {
    pub fn resolve() -> Result<Self> {
        let home = dirs_next::home_dir()
            .ok_or_else(|| anyhow!("无法解析当前用户的主目录"))?;
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

    pub fn workspaces_dir(&self) -> PathBuf {
        self.root.join("workspaces")
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
            self.workspaces_dir(),
        ] {
            std::fs::create_dir_all(&dir).with_context(|| {
                format!("创建 Kodex 数据目录 {} 失败", dir.display())
            })?;
        }
        Ok(())
    }
}
