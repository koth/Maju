use workspace_model::{ChangeSetSource, ChangeSetStatus, DiffQuality, FileChangeType};

pub(super) fn change_set_source_to_str(source: &ChangeSetSource) -> &'static str {
    match source {
        ChangeSetSource::AgentTurn => "AgentTurn",
        ChangeSetSource::AgentConversation => "AgentConversation",
        ChangeSetSource::ManualEdit => "ManualEdit",
        ChangeSetSource::GitWorktree => "GitWorktree",
        ChangeSetSource::ToolPreview => "ToolPreview",
    }
}

pub(super) fn change_set_source_from_str(source: &str) -> ChangeSetSource {
    match source {
        "AgentConversation" => ChangeSetSource::AgentConversation,
        "ManualEdit" => ChangeSetSource::ManualEdit,
        "GitWorktree" => ChangeSetSource::GitWorktree,
        "ToolPreview" => ChangeSetSource::ToolPreview,
        _ => ChangeSetSource::AgentTurn,
    }
}

pub(super) fn change_set_status_to_str(status: &ChangeSetStatus) -> &'static str {
    match status {
        ChangeSetStatus::Pending => "Pending",
        ChangeSetStatus::Complete => "Complete",
        ChangeSetStatus::Live => "Live",
        ChangeSetStatus::LegacyIncomplete => "LegacyIncomplete",
    }
}

pub(super) fn change_set_status_from_str(status: &str) -> ChangeSetStatus {
    match status {
        "Pending" => ChangeSetStatus::Pending,
        "Live" => ChangeSetStatus::Live,
        "LegacyIncomplete" => ChangeSetStatus::LegacyIncomplete,
        _ => ChangeSetStatus::Complete,
    }
}

pub(super) fn diff_quality_to_str(quality: &DiffQuality) -> &'static str {
    match quality {
        DiffQuality::Exact => "Exact",
        DiffQuality::LargeFileSkipped => "LargeFileSkipped",
        DiffQuality::BinarySkipped => "BinarySkipped",
        DiffQuality::MissingBaseline => "MissingBaseline",
        DiffQuality::FragmentRejected => "FragmentRejected",
        DiffQuality::LegacyIncomplete => "LegacyIncomplete",
    }
}

pub(super) fn diff_quality_from_str(quality: &str) -> DiffQuality {
    match quality {
        "LargeFileSkipped" => DiffQuality::LargeFileSkipped,
        "BinarySkipped" => DiffQuality::BinarySkipped,
        "MissingBaseline" => DiffQuality::MissingBaseline,
        "FragmentRejected" => DiffQuality::FragmentRejected,
        "LegacyIncomplete" => DiffQuality::LegacyIncomplete,
        _ => DiffQuality::Exact,
    }
}

pub(super) fn file_change_type_from_str(change_type: &str) -> FileChangeType {
    match change_type {
        "Created" => FileChangeType::Created,
        "Deleted" => FileChangeType::Deleted,
        _ => FileChangeType::Modified,
    }
}
