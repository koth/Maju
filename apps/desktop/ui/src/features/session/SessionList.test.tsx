import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { confirm } from "@tauri-apps/plugin-dialog";
import { SessionList } from "./SessionList";
import { onRemoteOpenProgress, onSessionStatus } from "../../lib/events";
import {
  sessionCreate,
  sessionArchive,
  sessionList,
  sessionSwitch,
  workspaceArchive,
  workspaceSetActive,
  settingsGetAgentSnapshot,
  settingsGetRemoteProfiles,
  settingsValidateRemoteProfile,
  workspaceOpenRemoteProfile,
} from "../../lib/tauri";
import type {
  AgentProviderProfile,
  AgentSettingsSnapshot,
  RemoteMachineProfilesSnapshot,
  RemoteOpenProgressEvent,
  SessionSummary,
  SessionListItem,
  WorkspaceSessionList,
} from "../../types";

vi.mock("../../lib/tauri", async () => {
  const actual = await vi.importActual<typeof import("../../lib/tauri")>("../../lib/tauri");
  return {
    ...actual,
    sessionList: vi.fn(),
    sessionSwitch: vi.fn(),
    sessionCreate: vi.fn(),
    sessionArchive: vi.fn(),
    sessionCancel: vi.fn(),
    workspaceArchive: vi.fn(),
    settingsGetAgentSnapshot: vi.fn(),
    settingsGetRemoteProfiles: vi.fn(),
    settingsValidateRemoteProfile: vi.fn(),
    workspaceSetActive: vi.fn(),
    workspaceOpenRemoteProfile: vi.fn(),
  };
});

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(),
  confirm: vi.fn(),
}));

vi.mock("../../lib/events", () => ({
  onRemoteOpenProgress: vi.fn(async (_callback: (progress: RemoteOpenProgressEvent) => void) => vi.fn()),
  onSessionStatus: vi.fn(async (_callback: (status: SessionSummary) => void) => () => {}),
}));

function providerProfile(
  id: string,
  label: string,
  selected: boolean,
  configured: boolean,
  requiresCredential: boolean,
): AgentProviderProfile {
  return {
    family: "claude",
    id,
    label,
    proxy_kind: "claude_native",
    selected,
    configured,
    base_url: id === "xiaomi_mimo" ? "https://token-plan-cn.xiaomimimo.com/anthropic" : null,
    default_model: id === "xiaomi_mimo" ? "MiMo-V2.5-Pro" : null,
    models: id === "xiaomi_mimo" ? ["MiMo-V2.5-Pro", "MiMo-V2.5"] : [],
    model_list_url: null,
    credential_label: requiresCredential ? `${label} API key` : null,
    requires_credential: requiresCredential,
    help_text: `${label} help`,
  };
}

function agentSnapshot(
  selectedClaudeProfile = "xiaomi_mimo",
  xiaomiConfigured = true,
  codebuddyInstalled = true,
): AgentSettingsSnapshot {
  return {
    settings: {
      selected_agent: "claude-agent-acp",
      acp_port: 0,
      theme: "graphite",
      lsp_servers: {},
      codex_connection_mode: "managed",
      selected_codex_provider_profile_id: "default",
      selected_claude_provider_profile_id: selectedClaudeProfile,
      claude: {
        available_models: [],
        fast_model: null,
      },
      web_tools: {
        enabled: false,
        provider: "brave",
      },
    },
    agents: [
      {
        id: "codebuddy",
        label: "CodeBuddy",
        binary: "codebuddy",
        installed: codebuddyInstalled,
        detected_path: codebuddyInstalled ? "/opt/homebrew/bin/codebuddy" : null,
        selected: false,
      },
      {
        id: "claude-agent-acp",
        label: "Claude",
        binary: "claude-agent-acp",
        installed: true,
        detected_path: "/Users/kothchen/.kodex/bin/claude-agent-acp",
        selected: true,
      },
    ],
    env_override: null,
    codex_acp: {
      provider: "default",
      selected_profile_id: "default",
      profiles: [],
      connection_mode: "default",
      deepseek_key_configured: false,
      config_path: "/Users/kothchen/.kodex/config.toml",
    },
    claude: {
      selected_profile_id: selectedClaudeProfile,
      profiles: [
        providerProfile("xiaomi_mimo", "Xiaomi Token Plan", selectedClaudeProfile === "xiaomi_mimo", xiaomiConfigured, true),
      ],
      fast_model: null,
      fast_model_options: [],
    },
    web_tools: {
      enabled: false,
      provider: "brave",
      configured: false,
    },
  };
}

const workspaceSessions: WorkspaceSessionList[] = [
  {
    workspace: {
      id: "workspace-1",
      root: "/Users/kothchen/code/Kodex",
      name: "Kodex",
    },
    sessions: [],
    active_session_id: "",
    is_active: true,
    connected: true,
  },
];

function sessionItem(overrides: Partial<SessionListItem>): SessionListItem {
  return {
    id: "session-1",
    title: "Feature work",
    status: "Idle",
    created_at: "2026-05-30T00:00:00Z",
    updated_at: "2026-05-30T00:00:00Z",
    message_count: 1,
    acp_session_id: "acp-1",
    agent_cli: "Codex",
    runtime_status: "none",
    attention_state: "none",
    ...overrides,
  };
}

function workspaceWithSessions(sessions: SessionListItem[]): WorkspaceSessionList[] {
  return [
    {
      ...workspaceSessions[0],
      sessions,
      active_session_id: sessions[0]?.id ?? "",
    },
  ];
}

function remoteProfilesSnapshot(): RemoteMachineProfilesSnapshot {
  return {
    profiles: [
      {
        id: "remote-1",
        display_name: "Devbox",
        ssh_target: "root@9.134.121.208",
        ssh_port: 36000,
        created_at_ms: 1,
        updated_at_ms: 2,
        last_validation: null,
      },
    ],
  };
}

function remoteProfilesSnapshotWithTwoMachines(): RemoteMachineProfilesSnapshot {
  return {
    profiles: [
      ...remoteProfilesSnapshot().profiles,
      {
        id: "remote-2",
        display_name: "GpuBox",
        ssh_target: "root@10.0.0.8",
        ssh_port: 22022,
        created_at_ms: 3,
        updated_at_ms: 4,
        last_validation: null,
      },
    ],
  };
}

describe("SessionList agent picker", () => {
  beforeEach(() => {
    vi.mocked(sessionList).mockResolvedValue(workspaceSessions);
    vi.mocked(onSessionStatus).mockImplementation(async (_callback: (status: SessionSummary) => void) => () => {});
    vi.mocked(settingsGetAgentSnapshot).mockResolvedValue(agentSnapshot());
    vi.mocked(settingsGetRemoteProfiles).mockResolvedValue(remoteProfilesSnapshot());
    vi.mocked(settingsValidateRemoteProfile).mockResolvedValue(remoteProfilesSnapshot());
    vi.mocked(sessionCreate).mockResolvedValue(undefined);
    vi.mocked(sessionSwitch).mockResolvedValue(undefined);
    vi.mocked(sessionArchive).mockResolvedValue(undefined);
    vi.mocked(workspaceArchive).mockResolvedValue(null);
    vi.mocked(confirm).mockResolvedValue(true);
    vi.mocked(workspaceSetActive).mockResolvedValue({} as never);
    vi.mocked(workspaceOpenRemoteProfile).mockResolvedValue({} as never);
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("allows creating Claude sessions when a configured provider is selected", async () => {
    render(
      <SessionList
        activeSessionId=""
        activeSessionTitle=""
        activeWorkspaceRoot="/Users/kothchen/code/Kodex"
        currentSessionStatus="Idle"
        onOpenSettings={vi.fn()}
        onSessionChanged={vi.fn()}
        onWorkspaceChanged={vi.fn()}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "在 Kodex 中新建会话" }));

    expect(await screen.findByRole("dialog")).toBeInTheDocument();
    expect(screen.queryByText(/需要先在设置中保存/)).not.toBeInTheDocument();

    const createButton = screen.getByRole("button", { name: "创建会话" });
    expect(createButton).toBeEnabled();
    fireEvent.click(createButton);

    await waitFor(() => expect(sessionCreate).toHaveBeenCalledWith("/Users/kothchen/code/Kodex", "claude-agent-acp"));
  });

  it("blocks Claude sessions when the selected provider is not configured", async () => {
    vi.mocked(settingsGetAgentSnapshot).mockResolvedValue(agentSnapshot("xiaomi_mimo", false, false));

    render(
      <SessionList
        activeSessionId=""
        activeSessionTitle=""
        activeWorkspaceRoot="/Users/kothchen/code/Kodex"
        currentSessionStatus="Idle"
        onOpenSettings={vi.fn()}
        onSessionChanged={vi.fn()}
        onWorkspaceChanged={vi.fn()}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "在 Kodex 中新建会话" }));

    expect(await screen.findByText("Claude Xiaomi Token Plan 需要先在设置中保存 Xiaomi Token Plan API key。")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "创建会话" })).toBeDisabled();
  });

  it("defaults to CodeBuddy when the default Claude profile is not configured", async () => {
    vi.mocked(settingsGetAgentSnapshot).mockResolvedValue(agentSnapshot("xiaomi_mimo", false));

    render(
      <SessionList
        activeSessionId=""
        activeSessionTitle=""
        activeWorkspaceRoot="/Users/kothchen/code/Kodex"
        currentSessionStatus="Idle"
        onOpenSettings={vi.fn()}
        onSessionChanged={vi.fn()}
        onWorkspaceChanged={vi.fn()}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "在 Kodex 中新建会话" }));

    expect(await screen.findByRole("dialog")).toBeInTheDocument();
    expect(screen.queryByText(/需要先在设置中保存/)).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "创建会话" }));

    await waitFor(() => expect(sessionCreate).toHaveBeenCalledWith("/Users/kothchen/code/Kodex", "codebuddy"));
  });

  it("defaults to Codex when Claude is not configured but Codex provider is ready", async () => {
    const snapshot = agentSnapshot("xiaomi_mimo", false);
    snapshot.agents.push({
      id: "codex-acp",
      label: "Codex",
      binary: "codex-acp",
      installed: true,
      detected_path: "/Users/kothchen/.kodex/bin/codex-acp",
      selected: false,
    });
    snapshot.settings.selected_codex_provider_profile_id = "timiai";
    snapshot.codex_acp.selected_profile_id = "timiai";
    snapshot.codex_acp.profiles = [
      {
        family: "codex",
        id: "timiai",
        label: "TimiAI",
        proxy_kind: "responses",
        selected: true,
        configured: true,
        base_url: "http://api.timiai.woa.com/ai_api_manage/llmproxy",
        default_model: "gpt-5.5",
        models: ["gpt-5.5"],
        model_list_url: null,
        credential_label: "TimiAI API key",
        requires_credential: true,
        help_text: "TimiAI help",
      },
    ];
    vi.mocked(settingsGetAgentSnapshot).mockResolvedValue(snapshot);

    render(
      <SessionList
        activeSessionId=""
        activeSessionTitle=""
        activeWorkspaceRoot="/Users/kothchen/code/Kodex"
        currentSessionStatus="Idle"
        onOpenSettings={vi.fn()}
        onSessionChanged={vi.fn()}
        onWorkspaceChanged={vi.fn()}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "在 Kodex 中新建会话" }));
    fireEvent.click(await screen.findByRole("button", { name: "创建会话" }));

    await waitFor(() => expect(sessionCreate).toHaveBeenCalledWith("/Users/kothchen/code/Kodex", "codex-acp"));
  });

  it("reopens dormant remote workspaces through the remote bootstrap flow", async () => {
    vi.mocked(sessionList).mockResolvedValue([
      {
        workspace: {
          id: "remote-workspace-1",
          root: "ssh://root@9.134.121.208:36000/data/workspace/CodeTrans",
          name: "CodeTrans",
          location: {
            kind: "remote_linux",
            profile_id: "remote-1",
            ssh_target: "root@9.134.121.208",
            ssh_port: 36000,
            remote_path: "/data/workspace/CodeTrans",
            agent_cli: "codex-acp",
            agent_command: "/root/.kodex/remote-agents/codex-acp/current/bin/codex-acp",
          },
        },
        sessions: [],
        active_session_id: "",
        is_active: true,
        connected: false,
      },
    ]);

    render(
      <SessionList
        activeSessionId=""
        activeSessionTitle=""
        activeWorkspaceRoot="ssh://root@9.134.121.208:36000/data/workspace/CodeTrans"
        currentSessionStatus="Idle"
        onOpenSettings={vi.fn()}
        onSessionChanged={vi.fn()}
        onWorkspaceChanged={vi.fn()}
      />,
    );

    const workspaceButton = await screen.findByTitle(/^双击连接远程工作区/);
    expect(screen.getByText("远程")).toBeInTheDocument();

    fireEvent.click(workspaceButton);
    expect(workspaceSetActive).not.toHaveBeenCalled();

    fireEvent.doubleClick(workspaceButton);
    const dialog = await screen.findByRole("dialog", { name: "打开远程目录" });
    expect(within(dialog).getByText("重新连接远程目录")).toBeInTheDocument();
    expect(within(dialog).getByLabelText("remote_open_path")).toHaveValue("/data/workspace/CodeTrans");

    fireEvent.change(within(dialog).getByLabelText("remote_open_password"), { target: { value: "ssh-secret" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "打开目录" }));

    await waitFor(() => expect(workspaceOpenRemoteProfile).toHaveBeenCalledWith(expect.objectContaining({
      profile_id: "remote-1",
      remote_path: "/data/workspace/CodeTrans",
      ssh_password: "ssh-secret",
    })));
    expect(workspaceSetActive).not.toHaveBeenCalled();
  });

  it("disables session rows for disconnected remote workspaces", async () => {
    vi.mocked(sessionList).mockResolvedValue([
      {
        workspace: {
          id: "remote-workspace-1",
          root: "ssh://root@9.134.121.208:36000/data/workspace/CodeTrans",
          name: "CodeTrans",
          location: {
            kind: "remote_linux",
            profile_id: "remote-1",
            ssh_target: "root@9.134.121.208",
            ssh_port: 36000,
            remote_path: "/data/workspace/CodeTrans",
            agent_cli: "codex-acp",
            agent_command: "/root/.kodex/remote-agents/codex-acp/current/bin/codex-acp",
          },
        },
        sessions: [sessionItem({ id: "remote-session-1", title: "继续任务" })],
        active_session_id: "remote-session-1",
        is_active: true,
        connected: false,
      },
    ]);

    render(
      <SessionList
        activeSessionId="remote-session-1"
        activeSessionTitle="继续任务"
        activeWorkspaceRoot="ssh://root@9.134.121.208:36000/data/workspace/CodeTrans"
        currentSessionStatus="Idle"
        onOpenSettings={vi.fn()}
        onSessionChanged={vi.fn()}
        onWorkspaceChanged={vi.fn()}
      />,
    );

    const sessionTitle = await screen.findByText("继续任务");
    const sessionButton = sessionTitle.closest("button") as HTMLButtonElement;
    expect(sessionButton).toBeDisabled();

    fireEvent.click(sessionButton);
    expect(sessionSwitch).not.toHaveBeenCalled();

    expect(screen.getByRole("button", { name: "归档会话 继续任务" })).toBeDisabled();
  });

  it("creates sessions from a connected remote workspace row using the remote workspace root", async () => {
    const onSessionChanged = vi.fn();
    vi.mocked(sessionList).mockResolvedValue([
      {
        workspace: {
          id: "remote-workspace-1",
          root: "ssh://root@9.134.121.208:36000/data/workspace/CodeTrans",
          name: "CodeTrans",
          location: {
            kind: "remote_linux",
            profile_id: "remote-1",
            ssh_target: "root@9.134.121.208",
            ssh_port: 36000,
            remote_path: "/data/workspace/CodeTrans",
            agent_cli: "codex-acp",
            agent_command: "/root/.kodex/remote-agents/codex-acp/current/bin/codex-acp",
          },
        },
        sessions: [],
        active_session_id: "",
        is_active: true,
        connected: true,
      },
    ]);

    render(
      <SessionList
        activeSessionId=""
        activeSessionTitle=""
        activeWorkspaceRoot="ssh://root@9.134.121.208:36000/data/workspace/CodeTrans"
        currentSessionStatus="Idle"
        onOpenSettings={vi.fn()}
        onSessionChanged={onSessionChanged}
        onWorkspaceChanged={vi.fn()}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "在 CodeTrans 中新建会话" }));
    fireEvent.click(await screen.findByRole("button", { name: "创建会话" }));

    await waitFor(() => {
      expect(sessionCreate).toHaveBeenCalledWith(
        "ssh://root@9.134.121.208:36000/data/workspace/CodeTrans",
        "codex-acp",
      );
      expect(onSessionChanged).toHaveBeenCalled();
    });
  });

  it("allows choosing a different agent for a connected remote workspace session", async () => {
    const onSessionChanged = vi.fn();
    vi.mocked(sessionList).mockResolvedValue([
      {
        workspace: {
          id: "remote-workspace-1",
          root: "ssh://root@9.134.121.208:36000/data/workspace/CodeTrans",
          name: "CodeTrans",
          location: {
            kind: "remote_linux",
            profile_id: "remote-1",
            ssh_target: "root@9.134.121.208",
            ssh_port: 36000,
            remote_path: "/data/workspace/CodeTrans",
            agent_cli: "codex-acp",
            agent_command: "/root/.kodex/remote-agents/codex-acp/current/bin/codex-acp",
          },
        },
        sessions: [],
        active_session_id: "",
        is_active: true,
        connected: true,
      },
    ]);

    render(
      <SessionList
        activeSessionId=""
        activeSessionTitle=""
        activeWorkspaceRoot="ssh://root@9.134.121.208:36000/data/workspace/CodeTrans"
        currentSessionStatus="Idle"
        onOpenSettings={vi.fn()}
        onSessionChanged={onSessionChanged}
        onWorkspaceChanged={vi.fn()}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "在 CodeTrans 中新建会话" }));
    fireEvent.click(await screen.findByRole("radio", { name: /Claude/ }));
    expect(screen.queryByText("重新打开远程目录后可切换")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "创建会话" }));

    await waitFor(() => {
      expect(sessionCreate).toHaveBeenCalledWith(
        "ssh://root@9.134.121.208:36000/data/workspace/CodeTrans",
        "claude-agent-acp",
      );
      expect(onSessionChanged).toHaveBeenCalled();
    });
  });

  it("creates sessions from remote metadata even when the workspace root is the remote path", async () => {
    const onSessionChanged = vi.fn();
    vi.mocked(sessionList).mockResolvedValue([
      {
        workspace: {
          id: "remote-workspace-1",
          root: "/data/workspace/CodeTrans",
          name: "CodeTrans",
          location: {
            kind: "remote_linux",
            profile_id: "remote-1",
            ssh_target: "root@9.134.121.208",
            ssh_port: 36000,
            remote_path: "/data/workspace/CodeTrans",
          },
        },
        sessions: [],
        active_session_id: "",
        is_active: true,
        connected: true,
      },
    ]);

    render(
      <SessionList
        activeSessionId=""
        activeSessionTitle=""
        activeWorkspaceRoot="/data/workspace/CodeTrans"
        currentSessionStatus="Idle"
        onOpenSettings={vi.fn()}
        onSessionChanged={onSessionChanged}
        onWorkspaceChanged={vi.fn()}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "在 CodeTrans 中新建会话" }));
    fireEvent.click(await screen.findByRole("button", { name: "创建会话" }));

    await waitFor(() => {
      expect(sessionCreate).toHaveBeenCalledWith(
        "ssh://root@9.134.121.208:36000/data/workspace/CodeTrans",
        "claude-agent-acp",
      );
      expect(onSessionChanged).toHaveBeenCalled();
    });
  });

  it("opens a remote workspace from the sidebar new workspace menu", async () => {
    const onWorkspaceChanged = vi.fn();
    vi.mocked(settingsGetRemoteProfiles).mockResolvedValue(remoteProfilesSnapshotWithTwoMachines());
    render(
      <SessionList
        activeSessionId=""
        activeSessionTitle=""
        activeWorkspaceRoot="/Users/kothchen/code/Kodex"
        currentSessionStatus="Idle"
        onOpenSettings={vi.fn()}
        onSessionChanged={vi.fn()}
        onWorkspaceChanged={onWorkspaceChanged}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "新建工作区" }));
    fireEvent.click(screen.getByRole("menuitem", { name: /打开远程目录/ }));

    const dialog = await screen.findByRole("dialog", { name: "打开远程目录" });
    await waitFor(() => expect(onRemoteOpenProgress).toHaveBeenCalled());
    expect(within(dialog).getByText(/Devbox/)).toBeInTheDocument();
    expect(within(dialog).getByText(/GpuBox/)).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("radio", { name: /GpuBox/ }));
    fireEvent.change(within(dialog).getByLabelText("remote_open_path"), { target: { value: "/root/kodex-remote-acp-test" } });

    const openRemote = within(dialog).getByRole("button", { name: "打开目录" });
    await waitFor(() => expect(openRemote).not.toBeDisabled());
    fireEvent.click(openRemote);

    await waitFor(() =>
      expect(workspaceOpenRemoteProfile).toHaveBeenCalledWith(expect.objectContaining({
        request_id: expect.any(String),
        profile_id: "remote-2",
        remote_path: "/root/kodex-remote-acp-test",
        agent_cli: "claude-agent-acp",
      })),
    );
    await waitFor(() => expect(onWorkspaceChanged).toHaveBeenCalled());
  });

  it("archives a session from the session row action", async () => {
    const onSessionChanged = vi.fn();
    const onSessionArchived = vi.fn();
    vi.mocked(sessionList).mockResolvedValue(
      workspaceWithSessions([
        sessionItem({ id: "session-archive", title: "Old work" }),
      ]),
    );

    render(
      <SessionList
        activeSessionId="session-current"
        activeSessionTitle="Current"
        activeWorkspaceRoot="/Users/kothchen/code/Kodex"
        currentSessionStatus="Idle"
        onOpenSettings={vi.fn()}
        onSessionChanged={onSessionChanged}
        onWorkspaceChanged={vi.fn()}
        onSessionArchived={onSessionArchived}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "归档会话 Old work" }));

    await waitFor(() => {
      expect(sessionArchive).toHaveBeenCalledWith("session-archive", "/Users/kothchen/code/Kodex");
      expect(onSessionArchived).toHaveBeenCalledWith({
        id: "session-archive",
        title: "Old work",
        workspaceRoot: "/Users/kothchen/code/Kodex",
      });
      expect(onSessionChanged).toHaveBeenCalled();
    });
    expect(confirm).not.toHaveBeenCalled();
  });

  it("archives an inactive workspace without changing the active snapshot", async () => {
    const onWorkspaceArchived = vi.fn();
    vi.mocked(sessionList).mockResolvedValue([
      workspaceSessions[0],
      {
        ...workspaceSessions[0],
        workspace: {
          ...workspaceSessions[0].workspace,
          id: "workspace-2",
          root: "/Users/kothchen/code/Other",
          name: "Other",
        },
        sessions: [],
        active_session_id: "",
        is_active: false,
      },
    ]);

    render(
      <SessionList
        activeSessionId="session-current"
        activeSessionTitle="Current"
        activeWorkspaceRoot="/Users/kothchen/code/Kodex"
        currentSessionStatus="Idle"
        onOpenSettings={vi.fn()}
        onSessionChanged={vi.fn()}
        onWorkspaceChanged={vi.fn()}
        onWorkspaceArchived={onWorkspaceArchived}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "归档项目 Other" }));

    await waitFor(() => {
      expect(confirm).toHaveBeenCalledWith("确定归档项目 Other？归档后该项目及其所有会话将从列表中移除，数据仍保留在本地。");
      expect(workspaceArchive).toHaveBeenCalledWith("/Users/kothchen/code/Other");
      expect(onWorkspaceArchived).not.toHaveBeenCalled();
    });
  });

  it("archives the active workspace and returns the replacement snapshot", async () => {
    const onWorkspaceArchived = vi.fn();
    const nextSnapshot = { revision: 42 };
    vi.mocked(workspaceArchive).mockResolvedValue(nextSnapshot as never);

    render(
      <SessionList
        activeSessionId="session-current"
        activeSessionTitle="Current"
        activeWorkspaceRoot="/Users/kothchen/code/Kodex"
        currentSessionStatus="Idle"
        onOpenSettings={vi.fn()}
        onSessionChanged={vi.fn()}
        onWorkspaceChanged={vi.fn()}
        onWorkspaceArchived={onWorkspaceArchived}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "归档项目 Kodex" }));

    await waitFor(() => {
      expect(confirm).toHaveBeenCalledWith("确定归档项目 Kodex？归档后该项目及其所有会话将从列表中移除，数据仍保留在本地。");
      expect(workspaceArchive).toHaveBeenCalledWith("/Users/kothchen/code/Kodex");
      expect(onWorkspaceArchived).toHaveBeenCalledWith(nextSnapshot);
    });
  });

  it("shows a spinner for a background session that is still running", async () => {
    vi.mocked(sessionList).mockResolvedValue(
      workspaceWithSessions([
        sessionItem({ id: "active-session", title: "Active" }),
        sessionItem({
          id: "background-session",
          title: "Background run",
          runtime_status: "background_running",
        }),
      ]),
    );

    render(
      <SessionList
        activeSessionId="active-session"
        activeSessionTitle="Active"
        activeWorkspaceRoot="/Users/kothchen/code/Kodex"
        currentSessionStatus="Idle"
        onOpenSettings={vi.fn()}
        onSessionChanged={vi.fn()}
        onWorkspaceChanged={vi.fn()}
      />,
    );

    const indicator = await screen.findByLabelText("后台会话仍在运行");
    expect(indicator).toHaveClass("is-progress");
    expect(indicator.closest(".sl-item")).toHaveClass("is-background-running");
  });

  it("shows a spinner for an active session in a hidden workspace", async () => {
    vi.mocked(sessionList).mockResolvedValue([
      {
        ...workspaceSessions[0],
        sessions: [sessionItem({ id: "active-session", title: "Active" })],
        active_session_id: "active-session",
        is_active: true,
      },
      {
        workspace: {
          id: "workspace-2",
          root: "/Users/kothchen/code/Other",
          name: "Other",
        },
        sessions: [
          sessionItem({
            id: "hidden-workspace-session",
            title: "Hidden workspace run",
            status: "Streaming",
            runtime_status: "active",
          }),
        ],
        active_session_id: "hidden-workspace-session",
        is_active: false,
        connected: true,
      },
    ]);

    render(
      <SessionList
        activeSessionId="active-session"
        activeSessionTitle="Active"
        activeWorkspaceRoot="/Users/kothchen/code/Kodex"
        currentSessionStatus="Idle"
        onOpenSettings={vi.fn()}
        onSessionChanged={vi.fn()}
        onWorkspaceChanged={vi.fn()}
      />,
    );

    const indicator = await screen.findByLabelText("后台会话仍在运行");
    expect(indicator).toHaveClass("is-progress");
    expect(indicator.closest(".sl-item")).toHaveClass("is-background-running");
  });

  it("refreshes background session indicators when session status events arrive", async () => {
    let callbackRegistered = false;
    let statusCallback: (status: SessionSummary) => void = () => {
      throw new Error("session status listener was not registered");
    };
    vi.mocked(onSessionStatus).mockImplementation(async (callback: (status: SessionSummary) => void) => {
      statusCallback = callback;
      callbackRegistered = true;
      return () => {};
    });
    vi.mocked(sessionList)
      .mockResolvedValueOnce(
        workspaceWithSessions([
          sessionItem({ id: "active-session", title: "Active" }),
          sessionItem({ id: "background-session", title: "Background run" }),
        ]),
      )
      .mockResolvedValueOnce(
        workspaceWithSessions([
          sessionItem({ id: "active-session", title: "Active" }),
          sessionItem({
            id: "background-session",
            title: "Background run",
            attention_state: "needs_attention",
          }),
        ]),
      );

    render(
      <SessionList
        activeSessionId="active-session"
        activeSessionTitle="Active"
        activeWorkspaceRoot="/Users/kothchen/code/Kodex"
        currentSessionStatus="Idle"
        onOpenSettings={vi.fn()}
        onSessionChanged={vi.fn()}
        onWorkspaceChanged={vi.fn()}
      />,
    );

    await screen.findByTitle("Background run · Codex");
    await waitFor(() => expect(callbackRegistered).toBe(true));
    statusCallback({
      id: "active-session",
      workspace_id: "workspace-1",
      title: "Active",
      model: "test-model",
      mode: "Build",
      agent_cli: "Codex",
      status: "Idle",
    });

    expect(await screen.findByLabelText("后台会话需要处理")).toHaveClass("is-attention");
  });

  it("shows attention instead of a spinner when a background session needs permission", async () => {
    vi.mocked(sessionList).mockResolvedValue(
      workspaceWithSessions([
        sessionItem({ id: "active-session", title: "Active" }),
        sessionItem({
          id: "background-session",
          title: "Needs permission",
          runtime_status: "background_running",
          attention_state: "needs_attention",
        }),
      ]),
    );

    render(
      <SessionList
        activeSessionId="active-session"
        activeSessionTitle="Active"
        activeWorkspaceRoot="/Users/kothchen/code/Kodex"
        currentSessionStatus="Idle"
        onOpenSettings={vi.fn()}
        onSessionChanged={vi.fn()}
        onWorkspaceChanged={vi.fn()}
      />,
    );

    const indicator = await screen.findByLabelText("后台会话需要处理");
    expect(indicator).toHaveClass("is-attention");
    expect(indicator).not.toHaveClass("is-progress");
    expect(indicator.closest(".sl-item")).toHaveClass("is-needs-attention");
    expect(indicator.closest(".sl-item")).not.toHaveClass("is-background-running");
  });

  it("shows a spinner for the active session when the conversation is hidden", async () => {
    vi.mocked(sessionList).mockResolvedValue(
      workspaceWithSessions([
        sessionItem({ id: "active-session", title: "Active", status: "Idle" }),
      ]),
    );

    render(
      <SessionList
        activeSessionId="active-session"
        activeSessionTitle="Active"
        activeWorkspaceRoot="/Users/kothchen/code/Kodex"
        currentSessionStatus="Streaming"
        activeConversationVisible={false}
        onOpenSettings={vi.fn()}
        onSessionChanged={vi.fn()}
        onWorkspaceChanged={vi.fn()}
      />,
    );

    const indicator = await screen.findByLabelText("当前会话仍在运行");
    expect(indicator).toHaveClass("is-progress");
    expect(indicator.closest(".sl-item")).toHaveClass("is-active-running");
  });

  it("shows and clears the completed-unviewed dot from refreshed session data", async () => {
    vi.mocked(sessionList)
      .mockResolvedValueOnce(
        workspaceWithSessions([
          sessionItem({ id: "active-session", title: "Active" }),
          sessionItem({
            id: "background-session",
            title: "Done in background",
            attention_state: "completed_unviewed",
          }),
        ]),
      )
      .mockResolvedValueOnce(
        workspaceWithSessions([
          sessionItem({ id: "active-session", title: "Active" }),
          sessionItem({
            id: "background-session",
            title: "Done in background",
            attention_state: "none",
          }),
        ]),
      );

    render(
      <SessionList
        activeSessionId="active-session"
        activeSessionTitle="Active"
        activeWorkspaceRoot="/Users/kothchen/code/Kodex"
        currentSessionStatus="Idle"
        onOpenSettings={vi.fn()}
        onSessionChanged={vi.fn()}
        onWorkspaceChanged={vi.fn()}
      />,
    );

    const indicator = await screen.findByLabelText("后台会话已完成，尚未查看");
    expect(indicator).toHaveClass("is-complete");
    expect(indicator.closest(".sl-item")).toHaveClass("is-completed-unviewed");

    const rowTitle = screen.getByTitle("Done in background · Codex");
    const rowButton = rowTitle.closest("button");
    expect(rowButton).not.toBeNull();
    fireEvent.click(rowButton!);

    await waitFor(() => {
      expect(sessionSwitch).toHaveBeenCalledWith(
        "background-session",
        "/Users/kothchen/code/Kodex",
      );
      expect(screen.queryByLabelText("后台会话已完成，尚未查看")).not.toBeInTheDocument();
    });
  });
});
