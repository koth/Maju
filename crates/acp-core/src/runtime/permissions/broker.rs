use anyhow::anyhow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};

use workspace_model::PermissionInputResponse;

#[derive(Clone, Debug, Default)]
pub(crate) struct PermissionBroker {
    state: Arc<Mutex<PermissionBrokerState>>,
    mode: Arc<Mutex<PermissionPolicyMode>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PermissionResolution {
    pub(crate) option_id: Option<String>,
    pub(crate) guidance: Option<String>,
    pub(crate) input_response: Option<PermissionInputResponse>,
}

impl PermissionResolution {
    pub(crate) fn new(
        option_id: Option<String>,
        guidance: Option<String>,
        input_response: Option<PermissionInputResponse>,
    ) -> Self {
        Self {
            option_id,
            guidance: guidance.and_then(|value| {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            }),
            input_response: input_response.filter(|response| !response.answers.is_empty()),
        }
    }
}

#[derive(Debug, Default)]
struct PermissionBrokerState {
    pending: HashMap<String, mpsc::Sender<PermissionResolution>>,
    early_resolutions: HashMap<String, PermissionResolution>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::runtime) enum PermissionPolicyMode {
    ReadOnly,
    #[default]
    Build,
    FullAccess,
}

impl PermissionBroker {
    pub(crate) fn register(
        &self,
        request_id: String,
    ) -> anyhow::Result<mpsc::Receiver<PermissionResolution>> {
        let (tx, rx) = mpsc::channel();

        let early_resolution = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("permission broker lock poisoned"))?;
            if let Some(option_id) = state.early_resolutions.remove(&request_id) {
                Some(option_id)
            } else {
                state.pending.insert(request_id, tx.clone());
                None
            }
        };

        if let Some(option_id) = early_resolution {
            tx.send(option_id)
                .map_err(|_| anyhow!("permission request already closed"))?;
        }

        Ok(rx)
    }

    pub(crate) fn resolve(
        &self,
        request_id: &str,
        option_id: Option<String>,
        guidance: Option<String>,
        input_response: Option<PermissionInputResponse>,
    ) -> anyhow::Result<bool> {
        let resolution = PermissionResolution::new(option_id, guidance, input_response);
        let sender = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("permission broker lock poisoned"))?;
            if let Some(sender) = state.pending.remove(request_id) {
                Some(sender)
            } else {
                state
                    .early_resolutions
                    .insert(request_id.to_string(), resolution.clone());
                None
            }
        };

        let Some(sender) = sender else {
            return Ok(false);
        };

        sender
            .send(resolution)
            .map_err(|_| anyhow!("permission request already closed"))?;
        Ok(true)
    }

    pub(crate) fn cancel(&self, request_id: &str) -> anyhow::Result<bool> {
        let sender = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("permission broker lock poisoned"))?;
            state.early_resolutions.remove(request_id);
            state.pending.remove(request_id)
        };

        let Some(sender) = sender else {
            return Ok(false);
        };

        let _ = sender.send(PermissionResolution::default());
        Ok(true)
    }

    pub(crate) fn set_mode(&self, mode_id: &str) -> anyhow::Result<()> {
        let normalized = mode_id.to_ascii_lowercase();
        let mode = match normalized.as_str() {
            "full-access" | "fullaccess" | "full_access" | "danger-full-access"
            | "bypasspermissions" | "bypass" | "完全访问" => PermissionPolicyMode::FullAccess,
            "build" | "auto" | "default" | "acceptedits" | "accept-edits" | "accept_edits"
            | "dontask" | "don't ask" | "dont ask" => PermissionPolicyMode::Build,
            "plan" | "readonly" | "read-only" | "read_only" => PermissionPolicyMode::ReadOnly,
            _ => PermissionPolicyMode::ReadOnly,
        };
        *self
            .mode
            .lock()
            .map_err(|_| anyhow!("permission broker lock poisoned"))? = mode;
        Ok(())
    }

    pub(in crate::runtime) fn mode(&self) -> PermissionPolicyMode {
        self.mode.lock().map(|mode| *mode).unwrap_or_default()
    }

    pub(crate) fn cancel_all(&self) -> anyhow::Result<()> {
        let pending = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("permission broker lock poisoned"))?;
            state.early_resolutions.clear();
            std::mem::take(&mut state.pending)
        };
        for (_, sender) in pending {
            let _ = sender.send(PermissionResolution::default());
        }
        Ok(())
    }
}
