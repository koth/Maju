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
            .unwrap_or("workspace")
            .to_string(),
        root: workspace_root.to_path_buf(),
    };

    let repository = GitService::open(workspace_root).unwrap_or_else(|_| RepositorySnapshot {
        branch: "no-repo".into(),
        head: "n/a".into(),
        changed_files: Vec::new(),
    });

    let welcome_message = ChatMessage {
        id: uuid::Uuid::new_v4(),
        role: MessageRole::Assistant,
        body: format!(
            "Ready in {}. Describe the change, bug, or task you want to handle.",
            descriptor.name
        ),
    };
    let system_message = ChatMessage {
        id: uuid::Uuid::new_v4(),
        role: MessageRole::System,
        body: "CodeBuddy stays idle until you submit a prompt from the composer below.".into(),
    };

    Ok(UiSnapshot {
        workspace: descriptor.clone(),
        session: SessionSummary {
            id: uuid::Uuid::new_v4(),
            workspace_id: descriptor.id,
            title: "New ACP session".into(),
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
            summary: "Ready to inspect ACP and Git activity".into(),
            status: ToolStatus::Pending,
            is_subagent: false,
            detail_text: String::new(),
            logs: vec![ToolLogEntry {
                title: "Ready".into(),
                body: "Waiting for the first ACP tool call".into(),
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
                title: "Summary".into(),
                items: vec!["Conversation and tool activity available".into()],
            },
            SidebarSection {
                title: "Artifacts".into(),
                items: vec!["Specs", "Design", "Tasks"]
                    .into_iter()
                    .map(String::from)
                    .collect(),
            },
        ],
        session_changes: Vec::new(),
    })
}
