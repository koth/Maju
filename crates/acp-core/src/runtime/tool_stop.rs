use super::permissions::PermissionBroker;
use super::terminal::TerminalManager;
use crate::events::ClientEvent;
use agent_client_protocol::JsonRpcNotification;
use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub(super) const KODEX_TOOL_STOP_METHOD: &str = "kodex.ai/tool_stop";
const KODEX_TOOL_STOP_META_KEY: &str = "kodex.ai/toolStop";

#[derive(Debug, Clone, Serialize, Deserialize, JsonRpcNotification)]
#[notification(method = "kodex.ai/tool_stop")]
#[serde(rename_all = "camelCase")]
pub(super) struct ToolStopNotification {
    session_id: String,
    tool_call_id: String,
}

impl ToolStopNotification {
    pub(super) fn new(session_id: impl Into<String>, tool_call_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            tool_call_id: tool_call_id.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ToolExecutionHandle {
    Permission { request_id: String },
    Terminal { terminal_id: String },
    AgentOwned,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ToolStopKind {
    Permission,
    Terminal,
    AgentOwned,
}

impl ToolStopKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Terminal => "terminal",
            Self::AgentOwned => "agent_owned",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct ToolExecutionRegistry {
    handles: Arc<Mutex<HashMap<String, Vec<ToolExecutionHandle>>>>,
}

impl ToolExecutionRegistry {
    pub(super) fn register_permission(
        &self,
        tool_call_id: impl Into<String>,
        request_id: impl Into<String>,
    ) -> anyhow::Result<Option<ClientEvent>> {
        let tool_call_id = tool_call_id.into();
        let request_id = request_id.into();
        let inserted = self.insert_unique(
            tool_call_id.clone(),
            ToolExecutionHandle::Permission { request_id },
        )?;
        Ok(inserted
            .then(|| availability_event(&tool_call_id, true, Some(ToolStopKind::Permission))))
    }

    pub(super) fn unregister_permission(
        &self,
        tool_call_id: &str,
        request_id: &str,
    ) -> anyhow::Result<Option<ClientEvent>> {
        let remaining = self.retain(tool_call_id, |handle| match handle {
            ToolExecutionHandle::Permission {
                request_id: existing,
            } => existing != request_id,
            ToolExecutionHandle::Terminal { .. } | ToolExecutionHandle::AgentOwned => true,
        })?;
        Ok((remaining == 0).then(|| availability_event(tool_call_id, false, None)))
    }

    pub(super) fn register_terminal(
        &self,
        tool_call_id: impl Into<String>,
        terminal_id: impl Into<String>,
    ) -> anyhow::Result<Option<ClientEvent>> {
        let tool_call_id = tool_call_id.into();
        let terminal_id = terminal_id.into();
        let inserted = self.insert_unique(
            tool_call_id.clone(),
            ToolExecutionHandle::Terminal { terminal_id },
        )?;
        Ok(inserted.then(|| availability_event(&tool_call_id, true, Some(ToolStopKind::Terminal))))
    }

    pub(super) fn register_agent_owned(
        &self,
        tool_call_id: impl Into<String>,
    ) -> anyhow::Result<Option<ClientEvent>> {
        let tool_call_id = tool_call_id.into();
        let inserted = self.insert_unique(tool_call_id.clone(), ToolExecutionHandle::AgentOwned)?;
        Ok(inserted
            .then(|| availability_event(&tool_call_id, true, Some(ToolStopKind::AgentOwned))))
    }

    pub(super) fn unregister_terminal_id(
        &self,
        terminal_id: &str,
    ) -> anyhow::Result<Vec<ClientEvent>> {
        let mut handles = self
            .handles
            .lock()
            .map_err(|_| anyhow!("tool execution registry poisoned"))?;
        let mut events = Vec::new();
        handles.retain(|tool_call_id, entry| {
            entry.retain(|handle| match handle {
                ToolExecutionHandle::Terminal {
                    terminal_id: existing,
                } => existing != terminal_id,
                ToolExecutionHandle::Permission { .. } | ToolExecutionHandle::AgentOwned => true,
            });
            let keep = !entry.is_empty();
            if !keep {
                events.push(availability_event(tool_call_id, false, None));
            }
            keep
        });
        Ok(events)
    }

    pub(super) fn stop_tool<F>(
        &self,
        tool_call_id: &str,
        terminal_manager: &TerminalManager,
        permission_broker: &PermissionBroker,
        mut stop_agent_owned: F,
    ) -> anyhow::Result<Vec<ClientEvent>>
    where
        F: FnMut(&str) -> anyhow::Result<bool>,
    {
        let handles = self
            .handles
            .lock()
            .map_err(|_| anyhow!("tool execution registry poisoned"))?
            .remove(tool_call_id)
            .unwrap_or_default();

        if handles.is_empty() {
            return Err(anyhow!("tool call is not stoppable: {tool_call_id}"));
        }

        let mut stopped_any = false;
        for handle in handles {
            match handle {
                ToolExecutionHandle::Permission { request_id } => {
                    stopped_any |= permission_broker.cancel(&request_id)?;
                }
                ToolExecutionHandle::Terminal { terminal_id } => {
                    stopped_any |= terminal_manager.kill_terminal_id(&terminal_id)?;
                }
                ToolExecutionHandle::AgentOwned => {
                    stopped_any |= stop_agent_owned(tool_call_id)?;
                }
            }
        }

        if !stopped_any {
            return Err(anyhow!("tool call is no longer running: {tool_call_id}"));
        }

        Ok(vec![
            ClientEvent::ToolStopped {
                id: tool_call_id.to_string(),
                outcome: "已停止".into(),
            },
            availability_event(tool_call_id, false, None),
        ])
    }

    pub(super) fn clear(&self) {
        if let Ok(mut handles) = self.handles.lock() {
            handles.clear();
        }
    }

    pub(super) fn events_from_session_payload(
        &self,
        payload: &Value,
    ) -> anyhow::Result<Vec<ClientEvent>> {
        let Some(tool_call_id) = find_string_field(
            payload,
            &[
                "kodex.ai/toolCallId",
                "kodexToolCallId",
                "toolCallId",
                "tool_call_id",
            ],
        ) else {
            return Ok(Vec::new());
        };

        if session_payload_marks_terminal(payload)
            && let Some(terminal_id) = find_string_field(
                payload,
                &[
                    "kodex.ai/terminalId",
                    "kodexTerminalId",
                    "terminalId",
                    "terminal_id",
                ],
            )
            && let Some(event) = self.register_terminal(tool_call_id.clone(), terminal_id)?
        {
            return Ok(vec![event]);
        }

        if session_payload_marks_agent_owned(payload)
            && let Some(event) = self.register_agent_owned(tool_call_id.clone())?
        {
            return Ok(vec![event]);
        }

        if session_payload_marks_terminal_closed(payload)
            || session_payload_marks_tool_finished(payload)
        {
            self.remove_all(&tool_call_id)?;
            return Ok(vec![availability_event(&tool_call_id, false, None)]);
        }

        Ok(Vec::new())
    }

    fn insert_unique(
        &self,
        tool_call_id: String,
        handle: ToolExecutionHandle,
    ) -> anyhow::Result<bool> {
        let mut handles = self
            .handles
            .lock()
            .map_err(|_| anyhow!("tool execution registry poisoned"))?;
        let entry = handles.entry(tool_call_id).or_default();
        if entry.iter().any(|existing| existing == &handle) {
            return Ok(false);
        }
        entry.push(handle);
        Ok(true)
    }

    fn retain(
        &self,
        tool_call_id: &str,
        keep: impl Fn(&ToolExecutionHandle) -> bool,
    ) -> anyhow::Result<usize> {
        let mut handles = self
            .handles
            .lock()
            .map_err(|_| anyhow!("tool execution registry poisoned"))?;
        let Some(entry) = handles.get_mut(tool_call_id) else {
            return Ok(0);
        };
        entry.retain(keep);
        let remaining = entry.len();
        if remaining == 0 {
            handles.remove(tool_call_id);
        }
        Ok(remaining)
    }

    fn remove_all(&self, tool_call_id: &str) -> anyhow::Result<()> {
        self.handles
            .lock()
            .map_err(|_| anyhow!("tool execution registry poisoned"))?
            .remove(tool_call_id);
        Ok(())
    }
}

pub(super) fn terminal_tool_call_id_from_request_payload(payload: &Value) -> Option<String> {
    explicit_string_field(payload, "kodex.ai/toolCallId")
        .or_else(|| explicit_string_field(payload, "kodexToolCallId"))
        .or_else(|| env_value(payload, "KODEX_TOOL_CALL_ID"))
}

fn availability_event(id: &str, can_stop: bool, kind: Option<ToolStopKind>) -> ClientEvent {
    ClientEvent::ToolStopAvailability {
        id: id.to_string(),
        can_stop,
        stop_kind: kind.map(|kind| kind.as_str().to_string()),
    }
}

fn session_payload_marks_terminal(payload: &Value) -> bool {
    stop_meta_kind(payload).as_deref() == Some("terminal")
        || has_field(payload, "kodex.ai/terminalId")
        || has_field(payload, "kodexTerminalId")
}

fn session_payload_marks_agent_owned(payload: &Value) -> bool {
    stop_meta_kind(payload).as_deref() == Some("agent_owned")
}

fn stop_meta_kind(payload: &Value) -> Option<String> {
    find_nested_object(payload, KODEX_TOOL_STOP_META_KEY).and_then(|value| {
        find_string_field(
            value,
            &[
                "stopKind",
                "stop_kind",
                "kind",
                "kodex.ai/stopKind",
                "kodexStopKind",
            ],
        )
        .map(|value| value.to_ascii_lowercase())
    })
}

fn session_payload_marks_terminal_closed(payload: &Value) -> bool {
    string_field_matches(
        payload,
        "kodex.ai/terminalStatus",
        &["exited", "released", "closed"],
    ) || string_field_matches(payload, "terminalStatus", &["exited", "released", "closed"])
}

fn session_payload_marks_tool_finished(payload: &Value) -> bool {
    string_field_matches(
        payload,
        "status",
        &[
            "completed",
            "succeeded",
            "failed",
            "interrupted",
            "cancelled",
            "canceled",
        ],
    )
}

fn string_field_matches(payload: &Value, key: &str, values: &[&str]) -> bool {
    find_string_for_key(payload, key).is_some_and(|value| {
        let normalized = value.to_ascii_lowercase();
        values.iter().any(|expected| normalized == *expected)
    })
}

fn find_string_field(payload: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| find_string_for_key(payload, key))
        .filter(|value| !value.trim().is_empty())
}

fn explicit_string_field(payload: &Value, key: &str) -> Option<String> {
    find_string_for_key(payload, key).filter(|value| !value.trim().is_empty())
}

fn find_string_for_key(payload: &Value, key: &str) -> Option<String> {
    match payload {
        Value::Object(object) => {
            if let Some(value) = object.get(key).and_then(Value::as_str) {
                return Some(value.to_string());
            }
            object
                .values()
                .find_map(|value| find_string_for_key(value, key))
        }
        Value::Array(items) => items
            .iter()
            .find_map(|value| find_string_for_key(value, key)),
        _ => None,
    }
}

fn has_field(payload: &Value, key: &str) -> bool {
    match payload {
        Value::Object(object) => {
            object.contains_key(key) || object.values().any(|value| has_field(value, key))
        }
        Value::Array(items) => items.iter().any(|value| has_field(value, key)),
        _ => false,
    }
}

fn find_nested_object<'a>(payload: &'a Value, key: &str) -> Option<&'a Value> {
    match payload {
        Value::Object(object) => {
            if let Some(value @ Value::Object(_)) = object.get(key) {
                return Some(value);
            }
            object
                .values()
                .find_map(|value| find_nested_object(value, key))
        }
        Value::Array(items) => items
            .iter()
            .find_map(|value| find_nested_object(value, key)),
        _ => None,
    }
}

fn env_value(payload: &Value, name: &str) -> Option<String> {
    match payload {
        Value::Object(object) => {
            if object.get("name").and_then(Value::as_str) == Some(name)
                && let Some(value) = object.get("value").and_then(Value::as_str)
            {
                return Some(value.to_string());
            }
            object.values().find_map(|value| env_value(value, name))
        }
        Value::Array(items) => items.iter().find_map(|value| env_value(value, name)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn stop_tool_cancels_registered_permission_request() {
        let broker = PermissionBroker::default();
        let permission_rx = broker
            .register("request-1".into())
            .expect("permission request should register");
        let registry = ToolExecutionRegistry::default();
        registry
            .register_permission("tool-1", "request-1")
            .expect("permission handle should register");
        let terminal_manager = TerminalManager::default();

        let events = registry
            .stop_tool("tool-1", &terminal_manager, &broker, |_| Ok(false))
            .expect("tool stop should succeed");

        let resolution = permission_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("permission request should be cancelled");
        assert_eq!(resolution.option_id, None);
        assert!(events.iter().any(|event| {
            matches!(
                event,
                ClientEvent::ToolStopped { id, .. } if id == "tool-1"
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(
                event,
                ClientEvent::ToolStopAvailability { id, can_stop: false, .. } if id == "tool-1"
            )
        }));
    }

    #[test]
    fn stop_tool_does_not_cancel_unrelated_permission_request() {
        let broker = PermissionBroker::default();
        let stopped_rx = broker
            .register("request-1".into())
            .expect("first permission request should register");
        let unrelated_rx = broker
            .register("request-2".into())
            .expect("second permission request should register");
        let registry = ToolExecutionRegistry::default();
        registry
            .register_permission("tool-1", "request-1")
            .expect("permission handle should register");
        let terminal_manager = TerminalManager::default();

        registry
            .stop_tool("tool-1", &terminal_manager, &broker, |_| Ok(false))
            .expect("tool stop should succeed");

        assert!(stopped_rx.recv_timeout(Duration::from_secs(1)).is_ok());
        assert!(
            unrelated_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err()
        );
    }

    #[test]
    fn terminal_tool_call_id_can_be_read_from_env_payload() {
        let payload = serde_json::json!({
            "env": [
                { "name": "KODEX_TOOL_CALL_ID", "value": "tool-terminal-1" }
            ]
        });

        assert_eq!(
            terminal_tool_call_id_from_request_payload(&payload),
            Some("tool-terminal-1".into())
        );
    }

    #[test]
    fn terminal_release_removes_registered_terminal_handle() {
        let registry = ToolExecutionRegistry::default();
        registry
            .register_terminal("tool-terminal-1", "terminal-1")
            .expect("terminal handle should register");

        let events = registry
            .unregister_terminal_id("terminal-1")
            .expect("terminal handle should unregister");

        assert!(events.iter().any(|event| {
            matches!(
                event,
                ClientEvent::ToolStopAvailability { id, can_stop: false, .. }
                    if id == "tool-terminal-1"
            )
        }));
    }

    #[test]
    fn agent_owned_stop_metadata_registers_agent_handle() {
        let payload = serde_json::json!({
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "tool-agent-1",
                "_meta": {
                    "kodex.ai/toolStop": {
                        "toolCallId": "tool-agent-1",
                        "stopKind": "agent_owned"
                    }
                }
            }
        });
        let registry = ToolExecutionRegistry::default();

        let events = registry
            .events_from_session_payload(&payload)
            .expect("agent-owned metadata should parse");

        assert!(events.iter().any(|event| {
            matches!(
                event,
                ClientEvent::ToolStopAvailability {
                    id,
                    can_stop: true,
                    stop_kind: Some(kind),
                } if id == "tool-agent-1" && kind == "agent_owned"
            )
        }));
    }

    #[test]
    fn codebuddy_payload_without_kodex_stop_metadata_does_not_register_stop() {
        let payload = serde_json::json!({
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": "codebuddy-tool-1",
                "_meta": {
                    "codebuddy.ai/interruptionRequest": {
                        "tool_call_id": "codebuddy-tool-1",
                        "tool_name": "Bash"
                    }
                }
            }
        });
        let registry = ToolExecutionRegistry::default();

        let events = registry
            .events_from_session_payload(&payload)
            .expect("CodeBuddy payload should parse safely");

        assert!(events.is_empty());
    }

    #[test]
    fn stop_tool_invokes_agent_owned_stop_once() {
        let broker = PermissionBroker::default();
        let terminal_manager = TerminalManager::default();
        let registry = ToolExecutionRegistry::default();
        registry
            .register_agent_owned("tool-agent-1")
            .expect("agent-owned handle should register");
        let mut stopped = Vec::new();

        let events = registry
            .stop_tool("tool-agent-1", &terminal_manager, &broker, |tool_call_id| {
                stopped.push(tool_call_id.to_string());
                Ok(true)
            })
            .expect("tool stop should succeed");

        assert_eq!(stopped, vec!["tool-agent-1"]);
        assert!(events.iter().any(|event| {
            matches!(
                event,
                ClientEvent::ToolStopped { id, .. } if id == "tool-agent-1"
            )
        }));
        assert!(
            registry
                .stop_tool("tool-agent-1", &terminal_manager, &broker, |_| Ok(true))
                .is_err()
        );
    }

    #[test]
    fn interrupted_tool_status_clears_registered_handle() {
        let registry = ToolExecutionRegistry::default();
        registry
            .register_agent_owned("tool-agent-1")
            .expect("agent-owned handle should register");
        let payload = serde_json::json!({
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "tool-agent-1",
                "status": "interrupted"
            }
        });

        let events = registry
            .events_from_session_payload(&payload)
            .expect("interrupted status should parse");

        assert!(events.iter().any(|event| {
            matches!(
                event,
                ClientEvent::ToolStopAvailability { id, can_stop: false, .. }
                    if id == "tool-agent-1"
            )
        }));
        assert!(
            registry
                .stop_tool(
                    "tool-agent-1",
                    &TerminalManager::default(),
                    &PermissionBroker::default(),
                    |_| Ok(true)
                )
                .is_err()
        );
    }
}
