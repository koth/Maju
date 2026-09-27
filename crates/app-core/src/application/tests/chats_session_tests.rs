use super::*;
use crate::paths::AppPaths;

/// Bootstrap an app whose workspace root IS the project-less chats workspace
/// (`~/.kodex/chats`) — the only workspace where `session_create` reuses the
/// empty bootstrap placeholder instead of allocating a fresh row.
fn chats_app(dir: &tempfile::TempDir) -> Application {
    let app_paths = AppPaths::from_root(dir.path().join("home").join(".kodex"));
    let chats_root = app_paths.chats_workspace_root();
    fs::create_dir_all(&chats_root).unwrap();
    let app =
        Application::bootstrap_with_app_paths(&chats_root, mock_agent_command(), app_paths, None)
            .unwrap();
    // Guard the fixture itself: if the workspace root did not match, the
    // placeholder shortcut under test would never run and the assertions below
    // would pass vacuously.
    assert_eq!(app.ui.workspace.root, chats_root);
    app
}

/// Raw path of the mock ACP agent (the `mock_agent_command()` helper returns a
/// shell-quoted command line).
fn mock_agent_binary_path() -> PathBuf {
    let mut parts = shell_words::split(&mock_agent_command()).unwrap();
    PathBuf::from(parts.remove(0))
}

#[test]
fn chats_delete_last_session_leaves_a_visible_session() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = chats_app(&dir);
    wait_for_control(&mut app, SessionConfigCategory::Model);
    let placeholder_id = app.ui.session.id.to_string();

    app.session_delete(&placeholder_id).unwrap();

    let visible = app.session_list().unwrap();
    assert!(
        !visible.is_empty(),
        "删除最后一个聊天后必须留下一个新的可见会话"
    );
    let active_id = app.ui.session.id.to_string();
    assert_ne!(active_id, placeholder_id);
    assert!(
        visible.iter().any(|session| session.id == active_id),
        "新建的会话必须出现在列表里，否则侧栏会一直显示“暂无会话”"
    );
    assert!(app.store.session_is_visible(&active_id).unwrap());
}

#[test]
fn chats_create_delete_cycles_never_empty_the_sidebar() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = chats_app(&dir);
    wait_for_control(&mut app, SessionConfigCategory::Model);

    // The reported repro: 新建 → 删除 → 新建 → 删除 → 新建. Every deletion has to
    // leave a real replacement, otherwise the sidebar ends up on 暂无会话.
    for round in 0..3 {
        app.session_create(None, None).unwrap();
        let active_id = app.ui.session.id.to_string();
        assert!(
            app.store.session_is_visible(&active_id).unwrap(),
            "round {round}: 新建的会话必须已落库"
        );

        app.session_delete(&active_id).unwrap();
        let visible = app.session_list().unwrap();
        assert!(!visible.is_empty(), "round {round}: 删除后必须仍有可见会话");
        assert!(
            visible
                .iter()
                .any(|session| session.id == app.ui.session.id.to_string()),
            "round {round}: 当前会话必须在列表中"
        );
    }
}

#[test]
fn chats_archive_last_session_leaves_a_visible_session() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = chats_app(&dir);
    wait_for_control(&mut app, SessionConfigCategory::Model);
    let placeholder_id = app.ui.session.id.to_string();

    app.session_archive(&placeholder_id).unwrap();

    let visible = app.session_list().unwrap();
    assert!(
        !visible.is_empty(),
        "归档最后一个聊天后必须留下一个可见会话"
    );
    assert!(
        !visible.iter().any(|session| session.id == placeholder_id),
        "被归档的会话不能再出现在列表里"
    );
    assert!(
        visible
            .iter()
            .any(|session| session.id == app.ui.session.id.to_string())
    );
}

#[test]
fn chats_create_reuses_the_empty_placeholder_without_duplicating() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = chats_app(&dir);
    wait_for_control(&mut app, SessionConfigCategory::Model);
    let placeholder_id = app.ui.session.id.to_string();

    app.session_create(None, None).unwrap();

    assert_eq!(
        app.ui.session.id.to_string(),
        placeholder_id,
        "空占位会话应被复用，而不是再开一个空会话"
    );
    assert_eq!(app.session_list().unwrap().len(), 1);
}

#[test]
fn chats_picked_agent_applies_to_the_reused_placeholder() {
    let dir = tempfile::tempdir().unwrap();
    let app_paths = AppPaths::from_root(dir.path().join("home").join(".kodex"));
    let chats_root = app_paths.chats_workspace_root();
    fs::create_dir_all(&chats_root).unwrap();

    // Point the Codex slot at the mock ACP agent so picking "Codex" in the
    // 新建对话 picker resolves to a spawnable (mock) binary.
    let codex_binary = crate::settings::codex_acp_binary_path(&app_paths);
    fs::create_dir_all(codex_binary.parent().unwrap()).unwrap();
    fs::copy(mock_agent_binary_path(), &codex_binary).unwrap();

    let mut app =
        Application::bootstrap_with_app_paths(&chats_root, mock_agent_command(), app_paths, None)
            .unwrap();
    wait_for_control(&mut app, SessionConfigCategory::Model);
    let placeholder_id = app.ui.session.id.to_string();
    assert_ne!(
        app.ui.session.agent_cli.as_deref(),
        Some("Codex"),
        "占位会话默认不应是 Codex，否则这个回归测试没有意义"
    );

    // The reported bug: the placeholder was booted with the workspace default
    // agent and "reusing" it ignored the picked agent, so the new chat came
    // back as the old agent.
    app.session_create(Some(AgentCliId::CodexAcp), None)
        .unwrap();
    wait_for_control(&mut app, SessionConfigCategory::Model);

    assert_eq!(
        app.ui.session.id.to_string(),
        placeholder_id,
        "选中的 agent 应该套用到被复用的占位会话上，而不是另开一个会话"
    );
    assert_eq!(
        app.ui.session.agent_cli.as_deref(),
        Some("Codex"),
        "新建对话选中的 agent 必须生效"
    );
    assert_eq!(app.session_list().unwrap().len(), 1);
}
