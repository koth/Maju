use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedState {
    pub ui: workspace_model::UiSnapshot,
    pub agent_command: String,
}

pub(crate) fn save_state(path: &Path, state: &PersistedState) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(state)?;
    fs::write(path, json).context("failed to persist state")
}

pub(crate) fn load_state(path: &Path) -> anyhow::Result<PersistedState> {
    let raw = fs::read_to_string(path).context("failed to read persisted state")?;
    serde_json::from_str(&raw).context("failed to deserialize persisted state")
}
