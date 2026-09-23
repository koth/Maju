//! Automation (定时任务) Tauri commands: CRUD, manual runs and run history.
//!
//! Automations live in the global session store (like archived sessions) so
//! they are workspace-independent; each automation names its own target
//! workspace + agent (defaulting to the DeepSeek Harness). Actual dispatch is
//! shared with the scheduler via `automation_scheduler::dispatch_run`.

use crate::automation_scheduler;
use app_core::AppPaths;
use session_store::SessionStore;
use uuid::Uuid;
use workspace_model::{
    AgentCliId, AutomationInput, AutomationRecord, AutomationRunRecord, AutomationRunTrigger,
};

fn open_store() -> Result<SessionStore, String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    SessionStore::open_global(paths.root()).map_err(|e| e.to_string())
}

/// Storage-format timestamp (epoch-seconds numeric string, like sessions).
fn timestamp() -> String {
    app_core::automation::now_epoch_secs().to_string()
}

fn validate_input(input: &AutomationInput) -> Result<(), String> {
    if input.name.trim().is_empty() {
        return Err("请填写自动化名称".into());
    }
    if input.prompt.trim().is_empty() {
        return Err("请填写要执行的提示词".into());
    }
    if input.workspace_root.trim().is_empty() {
        return Err("请选择目标项目".into());
    }
    // A schedule that cannot produce a future firing instant is invalid (a
    // one-shot in the past, an out-of-range wall clock, missing fields …).
    if app_core::automation::next_run_at_ms(&input.schedule, app_core::automation::now_epoch_ms())
        .is_none()
    {
        return Err("执行计划无效或执行时间已过，请检查后重试".into());
    }
    Ok(())
}

fn record_from_input(
    id: String,
    created_at: String,
    enabled: bool,
    input: AutomationInput,
) -> AutomationRecord {
    let now_ms = app_core::automation::now_epoch_ms();
    AutomationRecord {
        id,
        name: input.name.trim().to_string(),
        // Prompt keeps the user's original formatting; only emptiness is
        // validated (whitespace-only was rejected in `validate_input`).
        prompt: input.prompt,
        workspace_root: input.workspace_root.trim().to_string(),
        // Default execution agent: DeepSeek Harness (dsh).
        agent_cli: input.agent_cli.or(Some(AgentCliId::DeepSeekHarness)),
        agent_preset: input.agent_preset.filter(|preset| !preset.trim().is_empty()),
        next_run_at_ms: app_core::automation::next_run_at_ms(&input.schedule, now_ms),
        schedule: input.schedule,
        enabled,
        created_at,
        updated_at: timestamp(),
        run_count: 0,
        last_run: None,
    }
}

#[tauri::command]
pub fn automation_list() -> Result<Vec<AutomationRecord>, String> {
    let store = open_store()?;
    store.list_automations().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn automation_create(input: AutomationInput) -> Result<AutomationRecord, String> {
    validate_input(&input)?;
    let store = open_store()?;
    let record = record_from_input(Uuid::new_v4().to_string(), timestamp(), true, input);
    store
        .insert_automation(&record)
        .map_err(|error| error.to_string())?;
    Ok(record)
}

#[tauri::command]
pub fn automation_update(id: String, input: AutomationInput) -> Result<AutomationRecord, String> {
    validate_input(&input)?;
    let store = open_store()?;
    let existing = store
        .get_automation(&id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "自动化不存在".to_string())?;
    // Preserve `enabled` across edits: editing a paused automation must not
    // silently re-arm it.
    let record = record_from_input(id, existing.created_at.clone(), existing.enabled, input);
    store
        .update_automation(&record)
        .map_err(|error| error.to_string())?;
    store
        .get_automation(&record.id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "自动化不存在".to_string())
}

#[tauri::command]
pub fn automation_delete(id: String) -> Result<(), String> {
    let store = open_store()?;
    store
        .delete_automation(&id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn automation_set_enabled(id: String, enabled: bool) -> Result<AutomationRecord, String> {
    let store = open_store()?;
    store
        .set_automation_enabled(&id, enabled)
        .map_err(|error| error.to_string())?;
    // Re-arming an automation without a pending firing (e.g. a spent one-shot
    // being re-enabled makes no sense, but a recurring one edited while
    // disabled does) schedules its next occurrence from now.
    if enabled {
        let automation = store
            .get_automation(&id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "自动化不存在".to_string())?;
        if automation.next_run_at_ms.is_none() {
            let next = app_core::automation::next_run_at_ms(
                &automation.schedule,
                app_core::automation::now_epoch_ms(),
            );
            store
                .set_automation_next_run(&id, next)
                .map_err(|error| error.to_string())?;
        }
    }
    store
        .get_automation(&id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "自动化不存在".to_string())
}

/// "立即运行": dispatch the automation's prompt right now as a manual run.
/// Async + blocking pool: dispatch spawns the agent and connects the target
/// workspace.
#[tauri::command]
pub async fn automation_run_now(
    app: tauri::AppHandle,
    id: String,
) -> Result<AutomationRunRecord, String> {
    tokio::task::spawn_blocking(move || {
        let store = open_store()?;
        let automation = store
            .get_automation(&id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "自动化不存在".to_string())?;
        automation_scheduler::dispatch_run(
            &app,
            &store,
            &automation,
            AutomationRunTrigger::Manual,
        )
    })
    .await
    .map_err(|error| format!("Automation run task failed: {error}"))?
}

#[tauri::command]
pub fn automation_list_runs(
    id: String,
    limit: Option<u32>,
) -> Result<Vec<AutomationRunRecord>, String> {
    let store = open_store()?;
    store
        .list_automation_runs(&id, limit.unwrap_or(20))
        .map_err(|error| error.to_string())
}
