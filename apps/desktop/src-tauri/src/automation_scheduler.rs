//! Periodic scheduler for automations (定时任务).
//!
//! A background tokio task ticks every [`TICK`] and, for every due automation:
//! advances (or disarms) its schedule, records a run row, dispatches the
//! prompt as a background session run — "到点自动执行" — and emits the
//! `automation:fired` reminder event — "到点提醒". Overdue automations
//! (e.g. their moment passed while the app was closed) fire on the first tick
//! after launch; a missed window is then skipped rather than replayed in a
//! burst.
//!
//! Schedule math lives in [`app_core::automation`]; automations and runs are
//! persisted in the global session store.

use crate::{events, state::AppState};
use app_core::AppPaths;
use session_store::SessionStore;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use uuid::Uuid;
use workspace_model::{
    AutomationFiredEvent, AutomationRecord, AutomationRunRecord, AutomationRunStatus,
    AutomationRunTrigger, AutomationScheduleKind,
};

const TICK: Duration = Duration::from_secs(20);
/// Run rows younger than this are never reconciled: a manual `run now`
/// dispatch may be between inserting its row and registering its runtime.
const RECONCILE_GRACE_SECS: u64 = 60;

/// Spawn the scheduler loop. Called once from `main.rs` setup.
pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let handle = app.clone();
            // Dispatch spawns agent processes and takes workspace locks —
            // keep it off the async runtime like the other blocking commands.
            if let Err(error) = tokio::task::spawn_blocking(move || tick_once(&handle)).await {
                tracing::warn!("automation scheduler tick failed: {error}");
            }
        }
    });
}

fn tick_once(app: &AppHandle) {
    let Ok(paths) = AppPaths::resolve() else {
        return;
    };
    let Ok(store) = SessionStore::open_global(paths.root()) else {
        return;
    };
    reconcile_orphaned_runs(app, &store);

    let now_ms = app_core::automation::now_epoch_ms();
    let due = match store.list_due_automations(now_ms) {
        Ok(due) => due,
        Err(error) => {
            tracing::warn!("automation scheduler: listing due automations failed: {error}");
            return;
        }
    };
    for automation in due {
        // Advance (or disarm) the schedule BEFORE dispatch so a slow dispatch
        // can never double-fire the same window.
        let next_run_at_ms = app_core::automation::next_run_at_ms(&automation.schedule, now_ms);
        if matches!(automation.schedule.kind, AutomationScheduleKind::Once) {
            let _ = store.set_automation_enabled(&automation.id, false);
        }
        if let Err(error) = store.set_automation_next_run(&automation.id, next_run_at_ms) {
            tracing::warn!(
                "automation scheduler: advancing {} failed: {error}",
                automation.id
            );
            continue;
        }
        let _ = dispatch_run(app, &store, &automation, AutomationRunTrigger::Scheduled);
    }
}

/// Record a run row and fire `automation`'s prompt as a background session in
/// its target workspace. Emits the `automation:fired` reminder event on both
/// success and dispatch failure (a failed dispatch is still worth a reminder).
pub(crate) fn dispatch_run(
    app: &AppHandle,
    store: &SessionStore,
    automation: &AutomationRecord,
    trigger: AutomationRunTrigger,
) -> Result<AutomationRunRecord, String> {
    let mut run = AutomationRunRecord {
        id: Uuid::new_v4().to_string(),
        automation_id: automation.id.clone(),
        trigger,
        status: AutomationRunStatus::Running,
        started_at: app_core::automation::now_epoch_secs().to_string(),
        finished_at: None,
        session_id: None,
        workspace_root: automation.workspace_root.clone(),
        error: None,
    };
    store
        .insert_automation_run(&run)
        .map_err(|error| error.to_string())?;

    let outcome = app.state::<AppState>().run_automation_prompt(
        automation.workspace_root.clone(),
        automation.agent_cli,
        automation.agent_preset.clone(),
        automation.prompt.clone(),
        Some(run.id.clone()),
    );

    let mut fired = AutomationFiredEvent {
        run_id: run.id.clone(),
        automation_id: automation.id.clone(),
        name: automation.name.clone(),
        workspace_root: automation.workspace_root.clone(),
        session_id: None,
        error: None,
    };
    match outcome {
        Ok(session_id) => {
            let _ = store.attach_automation_run_session(&run.id, &session_id);
            run.session_id = Some(session_id.clone());
            fired.session_id = Some(session_id);
            events::emit_automation_fired(app, &fired);
            Ok(run)
        }
        Err(error) => {
            let _ = store.finish_automation_run(
                &run.id,
                AutomationRunStatus::Failed,
                Some(&error),
            );
            run.status = AutomationRunStatus::Failed;
            run.finished_at =
                Some(session_store::instant_to_iso_utc(
                    &app_core::automation::now_epoch_secs().to_string(),
                ));
            run.error = Some(error.clone());
            fired.error = Some(error);
            events::emit_automation_fired(app, &fired);
            Ok(run)
        }
    }
}

/// Mark `running` rows with no live owning runtime as `interrupted`: the app
/// exited (or the session broke) before their turn ended, so "running" would
/// otherwise stick forever after a restart.
fn reconcile_orphaned_runs(app: &AppHandle, store: &SessionStore) {
    let Ok(running) = store.list_automation_runs_with_status(AutomationRunStatus::Running) else {
        return;
    };
    if running.is_empty() {
        return;
    }
    let cutoff = session_store::epoch_secs_to_iso_utc(
        app_core::automation::now_epoch_secs().saturating_sub(RECONCILE_GRACE_SECS),
    );
    let state = app.state::<AppState>();
    for run in running {
        // Rows younger than the grace window may be mid-dispatch (the row is
        // inserted before the runtime registers) — leave them alone.
        if run.started_at.as_str() >= cutoff.as_str() {
            continue;
        }
        // Unknown ownership is treated as live (conservative): a lock error
        // must not kill a healthy run.
        let live = state.automation_run_in_flight(&run.id).unwrap_or(true);
        if !live {
            let _ = store.finish_automation_run(
                &run.id,
                AutomationRunStatus::Interrupted,
                Some("应用退出或会话中断，未能确认执行结果"),
            );
        }
    }
}
