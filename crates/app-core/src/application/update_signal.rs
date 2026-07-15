use workspace_model::PermissionInputRequest;

/// Lightweight signal broadcast by `Application` whenever the UI snapshot
/// changes or a permission is requested. Carries a signal only — never a
/// computed patch — because each subscriber keeps its own `UiPatchCursor`
/// and fetches its own Full/Patch delta on receipt.
#[derive(Debug, Clone)]
pub enum AppUpdate {
    /// The `UiSnapshot` advanced to `revision`; subscribers should fetch
    /// their delta via `poll_active_and_get_update`.
    UiUpdated { revision: u64 },
    /// A tool/agent is requesting permission. The phone surfaces an approval
    /// prompt from this without scraping a patch.
    PermissionRequested {
        tool_call_id: String,
        request: PermissionInputRequest,
    },
}
