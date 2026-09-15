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

    /// DeepSeek Harness home (`~/.kodex/dsh`), passed to `dsh web` as
    /// `DSH_HOME` so all harness state (settings.yaml, profiles, session
    /// logs) lives under Kodex's data root rather than `~/.dsh`.
    pub fn dsh_dir(&self) -> PathBuf {
        self.root.join("dsh")
    }

    /// `~/.kodex/dsh/settings.yaml` — the dsh settings document Kodex
    /// generates/merges from its BYOK provider catalog before spawning
    /// `dsh web`. See `design-dsh-settings.md`.
    pub fn dsh_settings_path(&self) -> PathBuf {
        self.dsh_dir().join("settings.yaml")
    }

    /// `~/.kodex/dsh/kodex.patch.yml` — the Kodex-owned cordis patch overlay
    /// passed to `dsh web` as `--patch`. `settings.yaml` can only carry the two
    /// sections Kodex owns, so bundle-row plugin configuration (e.g. the
    /// session-title token budget) travels here. Regenerated on every
    /// bring-up; the user's own `profiles/<name>/cordis.patch.yml` is left
    /// untouched because this overlay is applied *after* it.
    pub fn dsh_patch_path(&self) -> PathBuf {
        self.dsh_dir().join("kodex.patch.yml")
    }

    /// Root the harness-exposed `kodex-image` MCP server writes generated
    /// images under.
    ///
    /// The harness host is shared by every `dsh` session (and every
    /// workspace), so that server cannot use a session workspace root the way
    /// the ACP path does. `ImageApi` appends `.kodex/generated-images` to
    /// whatever root it is given, so passing this root's parent lands the
    /// output inside Kodex's own data directory
    /// (`~/.kodex/generated-images`).
    pub fn harness_image_output_root(&self) -> PathBuf {
        self.root
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.root.clone())
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
