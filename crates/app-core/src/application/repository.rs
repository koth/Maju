use super::*;

impl Application {
    pub fn refresh_repository(&mut self) {
        match GitService::open(&self.ui.workspace.root) {
            Ok(snapshot) if snapshot != self.ui.repository => {
                self.ui.repository = snapshot;
                self.bump_revision();
            }
            Ok(_) => {}
            Err(_) if !self.ui.repository.changed_files.is_empty() => {
                self.ui.repository.changed_files.clear();
                self.bump_revision();
            }
            Err(_) => {}
        }
    }

    pub fn stage_files(&mut self, paths: &[String]) -> Result<(), String> {
        GitService::stage(&self.ui.workspace.root, paths).map_err(|e| e.to_string())?;
        self.refresh_repository();
        Ok(())
    }
}
