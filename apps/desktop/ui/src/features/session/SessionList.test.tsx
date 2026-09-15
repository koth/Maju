import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { appConfirm } from "../../lib/confirm";
import { SessionList } from "./SessionList";
import { onRemoteOpenProgress, onSessionStatus } from "../../lib/events";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  sessionCreate,
  sessionArchive,
  sessionList,
  sessionSwitch,
  workspaceArchive,
  workspaceChatsRoot,
  workspaceOpen,
  workspaceSetActive,
  settingsGetAgentSnapshot,
  settingsGetRemoteProfiles,
  settingsListDshPresets,
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
    workspaceOpen: vi.fn(),
    settingsGetAgentSnapshot: vi.fn(),
    settingsGetRemoteProfiles: vi.fn(),
    settingsListDshPresets: vi.fn(),
    settingsValidateRemoteProfile: vi.fn(),
    workspaceChatsRoot: vi.fn(),
    workspaceSetActive: vi.fn(),
    workspaceOpenRemoteProfile: vi.fn(),
    // AccountButton (rendered in the footer) polls status on mount.
    remoteControlStatus: vi.fn().mockResolvedValue({
      enabled: true,
      connected: false,
      bound: false,
      logged_in: false,
    }),
  };
});

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(),
}));

vi.mock("../../lib/confirm", () => ({
  appConfirm: vi.fn(),
  archiveWorkspaceConfirmRequest: (label: string) => ({ label }),
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
    custom: false,
    protocol: null,
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
      {
        id: "deepseek-harness",
        label: "DeepSeek Harness",
        binary: "dsh",
        installed: true,
        detected_path: "/opt/homebrew/bin/dsh",
        selected: false,
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
    vi.mocked(workspaceOpen).mockResolvedValue({} as never);
    vi.mocked(appConfirm).mockResolvedValue(true);
    vi.mocked(workspaceChatsRoot).mockResolvedValue("");
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

    await waitFor(() => expect(sessionCreate).toHaveBeenCalledWith("/Users/kothchen/code/Kodex", "claude-agent-acp", null));
  });

  it("shows a preset dropdown for DeepSeek Harness and passes the selected preset", async () => {
    vi.mocked(settingsListDshPresets).mockResolvedValue([
      { id: "code", label: "Code", description: "写代码模式" },
      { id: "standard", label: "Standard", description: null },
    ]);

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
    const dialog = await screen.findByRole("dialog");
    // Select the DeepSeek Harness agent radio.
    fireEvent.click(within(dialog).getByRole("radio", { name: /DeepSeek Harness/ }));

    // The preset dropdown appears once the roster loads.
    await screen.findByText("跟随 dsh 默认");
    const presetSelect = within(dialog).getByLabelText("Agent 预设") as HTMLSelectElement;
    expect(within(presetSelect).getByRole("option", { name: /跟随 dsh 默认/ })).toBeInTheDocument();
    expect(within(presetSelect).getByRole("option", { name: /Code/ })).toBeInTheDocument();

    // Pick the "Standard" preset and create the session.
    fireEvent.change(presetSelect, { target: { value: "standard" } });
    fireEvent.click(screen.getByRole("button", { name: "创建会话" }));

    await waitFor(() =>
      expect(sessionCreate).toHaveBeenCalledWith("/Users/kothchen/code/Kodex", "deepseek-harness", "standard"),
    );
  });

  it("passes a null preset for DeepSeek Harness when following the dsh default", async () => {
    vi.mocked(settingsListDshPresets).mockResolvedValue([
      { id: "code", label: "Code", description: null },
    ]);

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
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("radio", { name: /DeepSeek Harness/ }));
    await screen.findByText("跟随 dsh 默认");
    fireEvent.click(screen.getByRole("button", { name: "创建会话" }));

    await waitFor(() =>
      expect(sessionCreate).toHaveBeenCalledWith("/Users/kothchen/code/Kodex", "deepseek-harness", null),
    );
  });

  it("resets the preset selection when the new-session modal is reopened", async () => {
    vi.mocked(settingsListDshPresets).mockResolvedValue([
      { id: "code", label: "Code", description: null },
      { id: "minimal", label: "极简", description: null },
    ]);

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

    // First open: pick a non-default preset, then cancel the modal.
    fireEvent.click(await screen.findByRole("button", { name: "在 Kodex 中新建会话" }));
    let dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("radio", { name: /DeepSeek Harness/ }));
    await screen.findByText("跟随 dsh 默认");
    let presetSelect = within(dialog).getByLabelText("Agent 预设") as HTMLSelectElement;
    fireEvent.change(presetSelect, { target: { value: "minimal" } });
    expect(presetSelect.value).toBe("minimal");
    fireEvent.click(within(dialog).getByRole("button", { name: "取消" }));

    // Second open: the preset must be back to "follow dsh default".
    fireEvent.click(await screen.findByRole("button", { name: "在 Kodex 中新建会话" }));
    dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("radio", { name: /DeepSeek Harness/ }));
    await screen.findByText("跟随 dsh 默认");
    presetSelect = within(dialog).getByLabelText("Agent 预设") as HTMLSelectElement;
    expect(presetSelect.value).toBe("");

    fireEvent.click(within(dialog).getByRole("button", { name: "创建会话" }));
    await waitFor(() =>
      expect(sessionCreate).toHaveBeenCalledWith("/Users/kothchen/code/Kodex", "deepseek-harness", null),
    );
  });

  it("shows a preset dropdown when creating a workspace with DeepSeek Harness", async () => {
    vi.mocked(settingsListDshPresets).mockResolvedValue([
      { id: "code", label: "Code", description: null },
      { id: "standard", label: "Standard", description: null },
    ]);
    vi.mocked(openDialog).mockResolvedValue("/Users/kothchen/code/Other");

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

    // Open the "new workspace" menu and pick "打开本地文件夹".
    fireEvent.click(await screen.findByRole("button", { name: "新建项目" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: /打开本地文件夹/ }));

    const dialog = await screen.findByRole("dialog");
    // Choose a directory via the dialog mock. The "选择..." button is wrapped
    // by a <label>, so its accessible name absorbs the label text — query by
    // class instead.
    const chooseDirBtn = dialog.querySelector(".sl-directory-btn") as HTMLButtonElement;
    fireEvent.click(chooseDirBtn);

    // Open the agent dropdown and pick DeepSeek Harness. The dropdown trigger
    // is wrapped by a <label> (accessible name "Agent"), and the menu items
    // select via onPointerDown, so query by class/text and fire pointerDown.
    const agentSelectBtn = dialog.querySelector(".sl-agent-select-btn") as HTMLButtonElement;
    fireEvent.click(agentSelectBtn);
    const dshSpan = await within(dialog).findByText("DeepSeek Harness");
    const dshButton = dshSpan.closest("button") as HTMLButtonElement;
    fireEvent.pointerDown(dshButton);

    // The preset dropdown appears once the roster loads.
    await screen.findByText("跟随 dsh 默认");
    const presetSelect = within(dialog).getByLabelText("Agent 预设") as HTMLSelectElement;
    fireEvent.change(presetSelect, { target: { value: "standard" } });

    fireEvent.click(screen.getByRole("button", { name: "创建工作区" }));

    await waitFor(() =>
      expect(workspaceOpen).toHaveBeenCalledWith("/Users/kothchen/code/Other", "deepseek-harness", "standard"),
    );
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
        custom: false,
        protocol: null,
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

    await waitFor(() => expect(sessionCreate).toHaveBeenCalledWith("/Users/kothchen/code/Kodex", "codex-acp", null));
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
    expect(screen.getByLabelText("远程")).toBeInTheDocument();

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
        null,
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
        null,
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
        null,
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

    fireEvent.click(await screen.findByRole("button", { name: "新建项目" }));
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
    expect(appConfirm).not.toHaveBeenCalled();
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
      expect(appConfirm).toHaveBeenCalledWith({ label: "Other" });
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
      expect(appConfirm).toHaveBeenCalledWith({ label: "Kodex" });
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

    // The inactive "Other" workspace defaults to collapsed. Its running session
    // surfaces on the header's single status dot, which pulses — there is no
    // second dot next to the folder icon.
    const indicator = await screen.findByLabelText("有会话进行中");
    expect(indicator).toHaveClass("sl-workspace-state");
    expect(indicator.closest(".sl-workspace-section")).toHaveClass("has-collapsed-running");
  });

  it("previews the five most recent sessions and reveals the rest on demand", async () => {
    const sessions = Array.from({ length: 8 }, (_, index) =>
      sessionItem({
        id: `session-${index}`,
        title: `Session ${index}`,
        // Newest first once sortSessions() orders by updated_at.
        updated_at: `2026-05-${String(30 - index).padStart(2, "0")}T00:00:00Z`,
      }),
    );
    vi.mocked(sessionList).mockResolvedValue(workspaceWithSessions(sessions));

    render(
      <SessionList
        activeSessionId="session-0"
        activeSessionTitle=""
        activeWorkspaceRoot="/Users/kothchen/code/Kodex"
        currentSessionStatus="Idle"
        onOpenSettings={vi.fn()}
        onSessionChanged={vi.fn()}
        onWorkspaceChanged={vi.fn()}
      />,
    );

    expect(await screen.findByText("Session 0")).toBeInTheDocument();
    expect(screen.getByText("Session 4")).toBeInTheDocument();
    expect(screen.queryByText("Session 5")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "展开显示" }));
    expect(await screen.findByText("Session 7")).toBeInTheDocument();
    // Everything fits in the first reveal, so the toggle is done for good.
    expect(screen.queryByRole("button", { name: "展开显示" })).not.toBeInTheDocument();
  });

  it("reveals double the previous batch on each expand click", async () => {
    const sessions = Array.from({ length: 40 }, (_, index) =>
      sessionItem({
        id: `session-${index}`,
        title: `Session ${index}`,
        // Strictly descending timestamps: index 0 is the most recent session.
        updated_at: new Date(Date.UTC(2026, 3, 1) - index * 86_400_000).toISOString(),
      }),
    );
    vi.mocked(sessionList).mockResolvedValue(workspaceWithSessions(sessions));

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

    await screen.findByText("Session 0");

    // 5 by default, then +10, then +20, then +40 until the list runs out.
    fireEvent.click(screen.getByRole("button", { name: "展开显示" }));
    expect(await screen.findByText("Session 14")).toBeInTheDocument();
    expect(screen.queryByText("Session 15")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "展开显示" }));
    expect(await screen.findByText("Session 34")).toBeInTheDocument();
    expect(screen.queryByText("Session 35")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "展开显示" }));
    expect(await screen.findByText("Session 39")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "展开显示" })).not.toBeInTheDocument();
  });

  it("resets an expanded project back to the preview when it is collapsed", async () => {
    const sessions = Array.from({ length: 8 }, (_, index) =>
      sessionItem({
        id: `session-${index}`,
        title: `Session ${index}`,
        updated_at: `2026-05-${String(30 - index).padStart(2, "0")}T00:00:00Z`,
      }),
    );
    vi.mocked(sessionList).mockResolvedValue(workspaceWithSessions(sessions));

    render(
      <SessionList
        activeSessionId="session-0"
        activeSessionTitle=""
        activeWorkspaceRoot="/Users/kothchen/code/Kodex"
        currentSessionStatus="Idle"
        onOpenSettings={vi.fn()}
        onSessionChanged={vi.fn()}
        onWorkspaceChanged={vi.fn()}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "展开显示" }));
    expect(await screen.findByText("Session 7")).toBeInTheDocument();

    // Collapse the project, then open it again: the preview is the default.
    fireEvent.click(screen.getByRole("button", { name: "折叠 Kodex 的会话列表" }));
    await waitFor(() => expect(screen.queryByText("Session 7")).not.toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "展开 Kodex 的会话列表" }));

    expect(await screen.findByText("Session 0")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "展开显示" })).toBeInTheDocument();
    expect(screen.queryByText("Session 5")).not.toBeInTheDocument();
  });

  it("keeps the open session visible when it falls outside the preview", async () => {
    const sessions = Array.from({ length: 8 }, (_, index) =>
      sessionItem({
        id: `session-${index}`,
        title: `Session ${index}`,
        updated_at: `2026-05-${String(30 - index).padStart(2, "0")}T00:00:00Z`,
      }),
    );
    vi.mocked(sessionList).mockResolvedValue(workspaceWithSessions(sessions));

    render(
      <SessionList
        activeSessionId="session-7"
        activeSessionTitle="Session 7"
        activeWorkspaceRoot="/Users/kothchen/code/Kodex"
        currentSessionStatus="Idle"
        onOpenSettings={vi.fn()}
        onSessionChanged={vi.fn()}
        onWorkspaceChanged={vi.fn()}
      />,
    );

    // The oldest session is open, so it stays on screen next to the preview.
    expect(await screen.findByText("Session 7")).toBeInTheDocument();
    expect(screen.queryByText("Session 5")).not.toBeInTheDocument();
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

  it("collapses and expands a workspace session list via the folder toggle", async () => {
    vi.mocked(sessionList).mockResolvedValue(
      workspaceWithSessions([
        sessionItem({ id: "session-1", title: "Feature work" }),
        sessionItem({ id: "session-2", title: "Bugfix" }),
      ]),
    );

    render(
      <SessionList
        activeSessionId="session-1"
        activeSessionTitle="Feature work"
        activeWorkspaceRoot="/Users/kothchen/code/Kodex"
        currentSessionStatus="Idle"
        onOpenSettings={vi.fn()}
        onSessionChanged={vi.fn()}
        onWorkspaceChanged={vi.fn()}
      />,
    );

    expect(await screen.findByText("Feature work")).toBeInTheDocument();
    expect(screen.getByText("Bugfix")).toBeInTheDocument();

    const collapseBtn = await screen.findByRole("button", { name: /折叠 Kodex 的会话列表/ });
    fireEvent.click(collapseBtn);

    await waitFor(() => expect(screen.queryByText("Feature work")).not.toBeInTheDocument());
    expect(screen.queryByText("Bugfix")).not.toBeInTheDocument();

    const expandBtn = await screen.findByRole("button", { name: /展开 Kodex 的会话列表/ });
    fireEvent.click(expandBtn);

    await waitFor(() => expect(screen.getByText("Feature work")).toBeInTheDocument());
    expect(screen.getByText("Bugfix")).toBeInTheDocument();
  });

  it("clicking the project name toggles collapse without activating the workspace", async () => {
    vi.mocked(sessionList).mockResolvedValue(
      workspaceWithSessions([
        sessionItem({ id: "session-1", title: "Feature work" }),
      ]),
    );

    render(
      <SessionList
        activeSessionId="session-1"
        activeSessionTitle="Feature work"
        activeWorkspaceRoot="/Users/kothchen/code/Kodex"
        currentSessionStatus="Idle"
        onOpenSettings={vi.fn()}
        onSessionChanged={vi.fn()}
        onWorkspaceChanged={vi.fn()}
      />,
    );

    expect(await screen.findByText("Feature work")).toBeInTheDocument();

    // Clicking the project name (the workspace node) collapses the session list
    // and must NOT call workspaceSetActive (no auto-activate / first-session open).
    fireEvent.click(screen.getByText("Kodex"));
    await waitFor(() => expect(screen.queryByText("Feature work")).not.toBeInTheDocument());
    expect(workspaceSetActive).not.toHaveBeenCalled();

    // Clicking again expands.
    fireEvent.click(screen.getByText("Kodex"));
    await waitFor(() => expect(screen.getByText("Feature work")).toBeInTheDocument());
    expect(workspaceSetActive).not.toHaveBeenCalled();
  });

  it("shows chat sessions in the chats group even when the chats workspace is not active", async () => {
    const chatsRoot = "/Users/kothchen/.kodex/chats";
    vi.mocked(workspaceChatsRoot).mockResolvedValue(chatsRoot);
    vi.mocked(sessionList).mockResolvedValue([
      workspaceSessions[0],
      {
        workspace: {
          ...workspaceSessions[0].workspace,
          id: "chats-workspace",
          root: chatsRoot,
          name: "chats",
        },
        sessions: [
          sessionItem({ id: "chat-1", title: "历史聊天一" }),
          sessionItem({ id: "chat-2", title: "历史聊天二" }),
        ],
        active_session_id: "",
        is_active: false,
        connected: false,
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
      />,
    );

    // The chats workspace is not the active one, but its sessions must still
    // render under the "聊天" group (no inner collapse hides them).
    expect(await screen.findByText("历史聊天一")).toBeInTheDocument();
    expect(screen.getByText("历史聊天二")).toBeInTheDocument();
  });
});
