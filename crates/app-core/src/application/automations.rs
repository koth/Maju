//! Automation (定时任务) execution.
//!
//! An automation run fires its prompt as a **fresh background session** in the
//! target workspace: the new session's state swaps in only for the duration of
//! the send (see `swap_visible_state_with_runtime`), the visible conversation
//! is never disturbed, and the dispatched turn keeps streaming through
//! `poll_background_runtimes` like any other background runtime. The sidebar
//! shows its progress dot, and the run's `automation_runs` row is finalized
//! when the turn ends — even if the user opens the session mid-run (the run
//! id travels with the session state).

use super::*;
use workspace_model::{AgentCliId, AutomationRunStatus};

impl Application {
    /// Create a fresh session in the current workspace and dispatch `prompt`
    /// on it as a background runtime. Returns the new session's id.
    ///
    /// `automation_run_id` tags the dispatched turn as an automation run so
    /// its row can be finalized at turn end (see
    /// [`Self::finalize_automation_run`]); pass `None` for ad-hoc background
    /// dispatches.
    pub fn run_automation_prompt(
        &mut self,
        agent: Option<AgentCliId>,
        preset: Option<String>,
        prompt: String,
        automation_run_id: Option<String>,
    ) -> Result<String, String> {
        let mut runtime = self.runtime_for_new_session(agent, preset)?;
        let session_id = runtime.local_session_id.to_string();

        // Swap the fresh session's state in, dispatch the prompt on it, and
        // swap the visible session back. The `automation_run_id` set below
        // travels with the session state into `runtime` on the second swap.
        self.swap_visible_state_with_runtime(&mut runtime);
        self.automation_run_id = automation_run_id;
        let outcome = self.send_prompt_background(prompt);
        self.swap_visible_state_with_runtime(&mut runtime);

        match outcome {
            Ok(_outcome) => {
                runtime.runtime_status = SessionRuntimeStatus::BackgroundRunning;
                runtime.idle_since = None;
                runtime.last_viewed = self.runtime_now();
                runtime.attention_state = SessionAttentionState::None;
                self.runtime_registry.insert(runtime);
                Ok(session_id)
            }
            Err(error) => {
                // Nothing usable was dispatched (the half-created session may
                // hold only an error notice): finalize the run and tear the
                // session down so no empty row lingers in the sidebar.
                if let Some(run_id) = runtime.automation_run_id.as_deref() {
                    let _ = self.store.finish_automation_run(
                        run_id,
                        AutomationRunStatus::Failed,
                        Some(&error.to_string()),
                    );
                }
                runtime.session.shutdown();
                let _ = self.store.delete_session(&session_id);
                Err(error.to_string())
            }
        }
    }

    /// Record the terminal state of the automation run driving the current
    /// turn (if any) and stop tracking it. Called from the prompt-poll
    /// terminal paths in `prompting.rs`, for visible and background sessions
    /// alike — the run id rides the swapped session state.
    pub(super) fn finalize_automation_run(
        &mut self,
        status: AutomationRunStatus,
        error: Option<String>,
    ) {
        let Some(run_id) = self.automation_run_id.take() else {
            return;
        };
        let _ = self
            .store
            .finish_automation_run(&run_id, status, error.as_deref());
    }

    /// Whether `run_id`'s turn is still owned by a live in-flight runtime
    /// (visible or registered). Used to reconcile `running` run rows orphaned
    /// by an app restart: no live owner ⇒ the run never finished.
    pub fn automation_run_in_flight(&self, run_id: &str) -> bool {
        if self.automation_run_id.as_deref() == Some(run_id) {
            return self.has_in_flight_prompt();
        }
        self.runtime_registry.entries.values().any(|runtime| {
            runtime.automation_run_id.as_deref() == Some(run_id) && runtime.is_in_flight()
        })
    }
}
