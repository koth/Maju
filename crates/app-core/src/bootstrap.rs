use git_service::GitService;
use std::path::Path;
use workspace_model::{
    ChatMessage, InspectorTab, MessageRole, RepositorySnapshot, SessionStatus, SessionSummary,
    SidebarSection, TimelineItem, ToolInvocation, ToolLogEntry, ToolStatus, UiSnapshot,
    WorkspaceDescriptor,
};

pub(crate) fn build_initial_ui(workspace_root: &Path) -> anyhow::Result<UiSnapshot> {
    let descriptor = WorkspaceDescriptor {
        id: uuid::Uuid::new_v4(),
        name: workspace_root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("工作区")
            .to_string(),
        root: workspace_root.to_path_buf(),
    };

    let repository = GitService::open(workspace_root).unwrap_or_else(|_| RepositorySnapshot {
        branch: "无仓库".into(),
        head: "无".into(),
        changed_files: Vec::new(),
    });

    let welcome_message = ChatMessage {
        id: uuid::Uuid::new_v4(),
        role: MessageRole::Assistant,
        body: format!(
            "已在 {} 中就绪。描述您想要处理的变更、问题或任务。",
            descriptor.name
        ),
    };
    let system_message = ChatMessage {
        id: uuid::Uuid::new_v4(),
        role: MessageRole::System,
        body: "CodeBuddy 将保持空闲，直到您从下方编辑器提交提示。".into(),
    };

    Ok(UiSnapshot {
        workspace: descriptor.clone(),
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
        available_commands: Vec::new(),
        agent_plan: Vec::new(),
        messages: vec![welcome_message.clone(), system_message.clone()],
        timeline: vec![
            TimelineItem::Message(welcome_message.id),
            TimelineItem::Message(system_message.id),
        ],
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
            permission_decision: None,
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
        thinking_status: None,
    })
}
