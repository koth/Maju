import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SessionList } from "./SessionList";
import {
  sessionCreate,
  sessionDelete,
  sessionList,
  sessionSwitch,
  workspaceSetActive,
  settingsGetAgentSnapshot,
} from "../../lib/tauri";
import type { AgentProviderProfile, AgentSettingsSnapshot, SessionListItem, WorkspaceSessionList } from "../../types";

vi.mock("../../lib/tauri", async () => {
  const actual = await vi.importActual<typeof import("../../lib/tauri")>("../../lib/tauri");
  return {
    ...actual,
    sessionList: vi.fn(),
    sessionSwitch: vi.fn(),
    sessionCreate: vi.fn(),
    sessionDelete: vi.fn(),
    sessionCancel: vi.fn(),
    settingsGetAgentSnapshot: vi.fn(),
    workspaceSetActive: vi.fn(),
  };
});

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(),
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

describe("SessionList agent picker", () => {
  beforeEach(() => {
    vi.mocked(sessionList).mockResolvedValue(workspaceSessions);
    vi.mocked(settingsGetAgentSnapshot).mockResolvedValue(agentSnapshot());
    vi.mocked(sessionCreate).mockResolvedValue(undefined);
    vi.mocked(sessionSwitch).mockResolvedValue(undefined);
    vi.mocked(sessionDelete).mockResolvedValue(undefined);
    vi.mocked(workspaceSetActive).mockResolvedValue({} as never);
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
