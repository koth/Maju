use std::path::Path;
use workspace_model::{
    ChatMessage, InspectorTab, MessageRole, RemoteLinuxWorkspace, RepositorySnapshot,
    SessionStatus, SessionSummary, SidebarSection, TimelineItem, ToolInvocation, ToolLogEntry,
    ToolStatus, UiSnapshot, WorkspaceDescriptor, WorkspaceLocation,
};

pub(crate) fn build_initial_ui(workspace_root: &Path) -> anyhow::Result<UiSnapshot> {
    // The project-less "聊天" workspace displays a friendly Chinese label
    // rather than the raw "chats" directory name.
    let is_chats = workspace_root
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "chats")
        && workspace_root
            .parent()
            .is_some_and(|parent| parent.ends_with(".kodex"));
    let descriptor = WorkspaceDescriptor {
        id: uuid::Uuid::new_v4(),
        name: if is_chats {
            "聊天".to_string()
        } else {
            workspace_root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("工作区")
            .to_string()
        },
        root: workspace_root.to_path_buf(),
        location: WorkspaceLocation::Local,
    };
    build_initial_ui_for_descriptor(descriptor)
}

pub(crate) fn build_initial_remote_ui(remote: RemoteLinuxWorkspace) -> anyhow::Result<UiSnapshot> {
    let descriptor = WorkspaceDescriptor {
        id: uuid::Uuid::new_v4(),
        name: remote.display_name(),
        root: std::path::PathBuf::from(remote.key()),
        location: WorkspaceLocation::RemoteLinux(remote),
    };
    build_initial_ui_for_descriptor(descriptor)
}

fn build_initial_ui_for_descriptor(descriptor: WorkspaceDescriptor) -> anyhow::Result<UiSnapshot> {
    let created_at = current_timestamp();
    let repository = RepositorySnapshot {
        branch: "加载中".into(),
        head: "待刷新".into(),
        changed_files: Vec::new(),
    };

    let system_message = ChatMessage {
        id: uuid::Uuid::new_v4(),
        role: MessageRole::System,
        body: agent_idle_notice("智能体"),
        created_at: created_at.clone(),
        ..Default::default()
    };

    Ok(UiSnapshot {
        revision: 1,
        workspace: descriptor.clone(),
        workspace_connected: true,
        session: SessionSummary {
            id: uuid::Uuid::new_v4(),
            workspace_id: descriptor.id,
            title: "新 ACP 会话".into(),
            model: "gpt-5.4".into(),
            mode: None,
            agent_cli: None,
            status: SessionStatus::Idle,
        },
        session_config: Default::default(),
        prompt_capabilities: Default::default(),
        image_capabilities: Default::default(),
        available_commands: Vec::new(),
        agent_plan: Vec::new(),
        messages: vec![system_message.clone()],
        timeline: vec![TimelineItem::Message(system_message.id)],
        tools: vec![ToolInvocation {
            id: uuid::Uuid::new_v4(),
            call_id: "workspace.scan".into(),
            parent_call_id: None,
            name: "workspace.scan".into(),
            kind: "system".into(),
            summary: "准备检查 ACP 和 Git 活动".into(),
            status: ToolStatus::Pending,
            is_subagent: false,
            detail_text: String::new(),
            logs: vec![ToolLogEntry {
                title: "就绪".into(),
                body: "等待第一个 ACP 工具调用".into(),
            }],
            diff_paths: Vec::new(),
            diff_previews: Vec::new(),
            raw_input: None,
            raw_output: None,
            terminal_output: None,
            error: None,
            permission_options: Vec::new(),
            permission_input: None,
            permission_decision: None,
            can_stop: false,
            stop_kind: None,
            stop_status: None,
        }],
        repository,
        inspector_tab: InspectorTab::Diff,
        inspector_sections: vec![
            SidebarSection {
                title: "摘要".into(),
                items: vec!["会话和工具活动可用".into()],
            },
            SidebarSection {
                title: "产物".into(),
                items: vec!["规范", "设计", "任务"]
                    .into_iter()
                    .map(String::from)
                    .collect(),
            },
        ],
        session_changes: Vec::new(),
        review_changes: Vec::new(),
        turn_changes: Vec::new(),
        thinking_status: None,
        usage: Default::default(),
        pending_steers: Vec::new(),
    })
}

pub(crate) fn update_initial_agent_notice(ui: &mut UiSnapshot, agent_label: &str) {
    let replacement = agent_idle_notice(agent_label);
    for message in &mut ui.messages {
        if message.role == MessageRole::System && is_initial_agent_notice(&message.body) {
            message.body = replacement.clone();
        }
    }
}

fn agent_idle_notice(agent_label: &str) -> String {
    format!("{agent_label} 将保持空闲，直到您从下方编辑器提交提示。")
}

fn is_initial_agent_notice(body: &str) -> bool {
    matches!(
        body.trim(),
        "CodeBuddy 将保持空闲，直到您从下方编辑器提交提示。"
            | "Codex 将保持空闲，直到您从下方编辑器提交提示。"
            | "Claude 将保持空闲，直到您从下方编辑器提交提示。"
            | "智能体 将保持空闲，直到您从下方编辑器提交提示。"
    )
}

fn current_timestamp() -> String {
    use std::time::SystemTime;
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_ui_uses_git_placeholder_without_status_scan() {
        let dir = tempfile::tempdir().unwrap();
        let ui = build_initial_ui(dir.path()).unwrap();

        assert_eq!(ui.repository.branch, "加载中");
        assert_eq!(ui.repository.head, "待刷新");
        assert!(ui.repository.changed_files.is_empty());
    }

    #[test]
    fn initial_agent_notice_uses_selected_agent_label() {
        let dir = tempfile::tempdir().unwrap();
        let mut ui = build_initial_ui(dir.path()).unwrap();

        update_initial_agent_notice(&mut ui, "Claude");

        assert!(
            ui.messages
                .iter()
                .any(|message| message.role == MessageRole::System
                    && message.body == "Claude 将保持空闲，直到您从下方编辑器提交提示。")
        );
        assert!(
            ui.messages
                .iter()
                .all(|message| !message.body.contains("CodeBuddy 将保持空闲"))
        );
    }

    #[test]
    fn initial_agent_notice_replaces_previous_agent_labels() {
        let dir = tempfile::tempdir().unwrap();
        let mut ui = build_initial_ui(dir.path()).unwrap();
        ui.messages
            .iter_mut()
            .filter(|message| message.role == MessageRole::System)
            .for_each(|message| {
                message.body = "Codex 将保持空闲，直到您从下方编辑器提交提示。".into();
            });

        update_initial_agent_notice(&mut ui, "Claude");

        assert!(ui.messages.iter().any(|message| {
            message.role == MessageRole::System
                && message.body == "Claude 将保持空闲，直到您从下方编辑器提交提示。"
        }));
        assert!(
            ui.messages
                .iter()
                .all(|message| !message.body.contains("Codex 将保持空闲"))
        );
    }
}
