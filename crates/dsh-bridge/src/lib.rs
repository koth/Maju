//! DeepSeek Harness (dsh) host RPC bridge for Kodex.
//!
//! Speaks the private dsh host RPC (`POST /api/<method>` for control, SSE
//! `events.mux` / `events.host` for events) and translates the full-fidelity
//! event stream into Kodex [`ClientEvent`]s, without modifying the dsh source.
//!
//! One shared [`HarnessHost`] serves multiple concurrent [`SessionHandle`]s
//! (Mode B): one `dsh web` process, one mux + one host SSE connection, frames
//! demuxed by `sessionId` through a [`SessionRouter`]. Per-session isolation
//! (event channel, command channel, `PermissionBroker`) is preserved at the
//! handle boundary; sharing happens below it.
//!
//! The crate implements [`acp_core::runtime::HarnessBackend`] and is registered
//! via [`acp_core::runtime::set_harness_backend`] at process init; `acp-core`
//! dispatches to it when `SessionConfig.harness_endpoint` is set. The
//! dependency direction is one-way (`dsh-bridge` → `acp-core`), so there is no
//! cycle: `acp-core` calls the bridge through a trait object it does not name.

mod approval;
mod frame;
mod host;
mod mapping;
mod mojibake;
mod process;
mod rpc_types;
mod session;
mod settings_gen;
mod transport;

pub use host::SessionSink;
pub use host::{HarnessHost, HarnessHostRegistry, HarnessHostRegistryHandle};
pub use process::{
    DshChild, SpawnDshWebConfig, kill_child, reap_orphaned_dsh_web, resolve_dsh_launch,
    resolve_npm_launch, spawn_dsh_web,
};
pub use rpc_types::{
    AnswerProtocol, ClientResponse, CommandsExecuteResult, CommandsExecuteValue, PromptContentPart,
    PromptMode, SessionCreatePayload, SessionPromptPayload,
};
pub use session::run_harness_session;
pub use settings_gen::{
    DshDefaultModel, DshModelEntry, DshProviderRoute, DshSettingsConfig, HarnessMcpServer,
    HarnessPatchConfig, key_env_for_provider, patch_path_for_root, render_harness_patch,
    settings_path_for_root, write_harness_patch, write_settings,
};
pub use transport::HttpClient;

use acp_core::runtime::HarnessBackend;
use acp_core::{ClientEvent, PermissionBroker, RuntimeCommand, SessionConfig, ShutdownSignal};
use std::sync::Arc;

/// Bridge backend: implements [`HarnessBackend`] for `acp-core`'s dispatch.
///
/// Holds an [`HarnessHostRegistry`] so multiple sessions targeting the same
/// endpoint share one `dsh web` process and one SSE pair. The registry
/// outlives any single session (held by `app-core`), but a default-constructed
/// backend owns its own registry for convenience (e.g. tests).
pub struct DshBridge {
    registry: Arc<HarnessHostRegistry>,
}

impl DshBridge {
    pub fn new(registry: Arc<HarnessHostRegistry>) -> Self {
        Self { registry }
    }

    /// Convenience constructor with a fresh private registry (no sharing across
    /// separately-constructed bridges — prefer one shared registry in `app-core`).
    pub fn standalone() -> Self {
        Self::new(Arc::new(HarnessHostRegistry::new()))
    }
}

impl HarnessBackend for DshBridge {
    fn run_session(
        &self,
        config: SessionConfig,
        tx_events: std::sync::mpsc::Sender<ClientEvent>,
        rx_commands: std::sync::mpsc::Receiver<RuntimeCommand>,
        permission_broker: PermissionBroker,
        shutdown_signal: ShutdownSignal,
    ) -> anyhow::Result<()> {
        session::run_harness_session(
            self.registry.clone(),
            config,
            tx_events,
            rx_commands,
            permission_broker,
            shutdown_signal,
        )
    }
}
