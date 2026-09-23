//! Registry of dsh harness background jobs (后台任务) per harness session id.
//!
//! The harness pushes per-session job snapshots over `session/jobs` (pre-0.1.5
//! mux frames) and `session/control` (`jobs` frames plus the opening
//! `baseline`). They are session-level state, not turn events, so nothing is
//! mapped into `ClientEvent` — the mapping layer records the latest snapshot
//! here and `app-core` reads it for the visible session ("后台任务" in the
//! conversation's context dock). Each push carries the FULL list for the
//! session, so recording replaces the previous snapshot.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use workspace_model::SessionJobRecord;

fn registry() -> &'static Mutex<HashMap<String, Vec<SessionJobRecord>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Vec<SessionJobRecord>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Replace `session_id`'s job snapshot with `jobs` (wire `SessionJob` values).
/// Unparseable entries are dropped and unknown fields ride `#[serde(default)]`
/// so new harness additions never break the list.
pub fn record_session_jobs(session_id: &str, jobs: &[serde_json::Value]) {
    if session_id.is_empty() {
        return;
    }
    let snapshot: Vec<SessionJobRecord> = jobs
        .iter()
        .filter_map(|value| serde_json::from_value(value.clone()).ok())
        .collect();
    if let Ok(mut guard) = registry().lock() {
        guard.insert(session_id.to_string(), snapshot);
    }
}

/// Latest job snapshot for `session_id` (empty when none was ever reported).
pub fn session_jobs(session_id: &str) -> Vec<SessionJobRecord> {
    registry()
        .lock()
        .ok()
        .and_then(|guard| guard.get(session_id).cloned())
        .unwrap_or_default()
}

/// Drop `session_id`'s snapshot (the host removed the session).
pub fn clear_session_jobs(session_id: &str) {
    if session_id.is_empty() {
        return;
    }
    if let Ok(mut guard) = registry().lock() {
        guard.remove(session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn records_replaces_and_clears_job_snapshots() {
        record_session_jobs(
            "s-1",
            &[json!({
                "id": "j-1", "kind": "bash", "label": "npm test",
                "status": "running", "startedAt": 1000
            })],
        );
        record_session_jobs(
            "s-2",
            &[json!({
                "id": "j-9", "kind": "bash", "label": "other",
                "status": "failed", "startedAt": 5
            })],
        );

        let jobs = session_jobs("s-1");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "j-1");
        assert_eq!(jobs[0].kind, "bash");
        assert_eq!(jobs[0].status, "running");
        assert_eq!(jobs[0].started_at_ms, 1000);
        assert_eq!(jobs[0].finished_at_ms, None);

        // A push replaces the snapshot (a finish transition arrives as a full
        // list, not a delta).
        record_session_jobs(
            "s-1",
            &[json!({
                "id": "j-1", "kind": "bash", "label": "npm test",
                "status": "completed", "startedAt": 1000, "finishedAt": 2000,
                "detail": "exit 0"
            })],
        );
        let jobs = session_jobs("s-1");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].status, "completed");
        assert_eq!(jobs[0].finished_at_ms, Some(2000));
        assert_eq!(jobs[0].detail.as_deref(), Some("exit 0"));

        // Unknown statuses survive as plain strings; malformed entries are
        // dropped rather than poisoning the list.
        record_session_jobs(
            "s-1",
            &[
                json!({ "id": "j-2", "status": "future-state" }),
                json!("junk"),
            ],
        );
        let jobs = session_jobs("s-1");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "j-2");
        assert_eq!(jobs[0].status, "future-state");

        assert_eq!(session_jobs("s-2").len(), 1);
        assert!(session_jobs("missing").is_empty());

        clear_session_jobs("s-2");
        assert!(session_jobs("s-2").is_empty());
    }
}
