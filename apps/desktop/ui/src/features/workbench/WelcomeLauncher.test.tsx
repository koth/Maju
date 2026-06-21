import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { WelcomeLauncher } from "./WelcomeLauncher";
import { onRemoteOpenProgress } from "../../lib/events";
import {
  settingsGetAgentSnapshot,
  settingsGetRemoteProfiles,
  settingsSelectAgent,
  settingsSelectAgentProviderProfile,
  startupPerfMark,
  workspaceGetRecent,
  workspaceOpen,
  workspaceOpenRemoteProfile,
  workspaceRestoreOpen,
} from "../../lib/tauri";
import type { AgentProviderProfile, AgentSettingsSnapshot, RemoteMachineProfilesSnapshot } from "../../types";

vi.mock("../../lib/tauri", async () => {
  const actual = await vi.importActual<typeof import("../../lib/tauri")>("../../lib/tauri");
  return {
    ...actual,
    settingsGetAgentSnapshot: vi.fn(),
    settingsGetRemoteProfiles: vi.fn(),
    settingsSelectAgent: vi.fn(),
    settingsSelectAgentProviderProfile: vi.fn(),
    startupPerfMark: vi.fn(),
    workspaceGetRecent: vi.fn(),
    workspaceOpen: vi.fn(),
    workspaceOpenRemoteProfile: vi.fn(),
    workspaceRemoveRecent: vi.fn(),
    workspaceRestoreOpen: vi.fn(),
  };
});

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(),
}));

vi.mock("../../lib/events", () => ({
  onRemoteOpenProgress: vi.fn(async () => {
    return vi.fn();
  }),
}));

vi.mock("./WindowControls", () => ({
  WindowControls: () => null,
}));

function profile(
  family: "codex" | "claude",
  id: string,
  selected: boolean,
  configured: boolean,
  requiresCredential: boolean,
): AgentProviderProfile {
  return {
    family,
    id,
    label: id,
    proxy_kind: "claude_native",
    selected,
    configured,
    base_url: null,
    default_model: null,
    models: [],
    model_list_url: null,
    credential_label: requiresCredential ? "API key" : null,
    requires_credential: requiresCredential,
    help_text: `${id} help`,
  };
}

function agentSnapshot(): AgentSettingsSnapshot {
  return {
    settings: {
      selected_agent: "claude-agent-acp",
      acp_port: 0,
      theme: "graphite",
      lsp_servers: {},
      codex_connection_mode: "managed",
      selected_codex_provider_profile_id: "default",
      selected_claude_provider_profile_id: "byok",
      claude: {
        available_models: ["claude-opus-4-7[1m]"],
        fast_model: null,
      },
      web_tools: {
        enabled: false,
        provider: "brave",
      },
    },
    agents: [
      {
        id: "claude-agent-acp",
        label: "Claude",
        binary: "claude-agent-acp",
        installed: true,
        detected_path: "C:\\tools\\claude-agent-acp.exe",
        selected: true,
      },
    ],
    env_override: null,
    codex_acp: {
      provider: "default",
      selected_profile_id: "default",
      profiles: [profile("codex", "default", true, true, false)],
      connection_mode: "managed",
      deepseek_key_configured: false,
      config_path: "C:\\Users\\yvonchen\\.kodex\\config.toml",
    },
    claude: {
      selected_profile_id: "byok",
      profiles: [
        profile("claude", "byok", true, false, true),
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

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

describe("WelcomeLauncher BYOK onboarding", () => {
  beforeEach(() => {
    vi.mocked(startupPerfMark).mockResolvedValue(undefined);
    vi.mocked(workspaceGetRecent).mockResolvedValue([
      { path: "D:\\work\\kodex", exists: true },
    ]);
    vi.mocked(settingsGetAgentSnapshot).mockResolvedValue(agentSnapshot());
    vi.mocked(settingsGetRemoteProfiles).mockResolvedValue(remoteProfilesSnapshot());
    vi.mocked(settingsSelectAgent).mockResolvedValue(agentSnapshot());
    vi.mocked(settingsSelectAgentProviderProfile).mockResolvedValue(agentSnapshot());
    vi.mocked(workspaceRestoreOpen).mockResolvedValue(null);
    vi.mocked(workspaceOpen).mockResolvedValue({} as never);
    vi.mocked(workspaceOpenRemoteProfile).mockResolvedValue({} as never);
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("opens Codex BYOK settings when no provider is configured", async () => {
    const onOpenSettings = vi.fn();
    render(<WelcomeLauncher onWorkspaceOpened={vi.fn()} onOpenSettings={onOpenSettings} />);

    await waitFor(() => expect(settingsSelectAgent).toHaveBeenCalledWith("codex-acp"));
    await waitFor(() => expect(settingsSelectAgentProviderProfile).toHaveBeenCalledWith("codex", "byok"));
    await waitFor(() =>
      expect(onOpenSettings).toHaveBeenCalledWith({
        startupNotice: { kind: "codex_byok" },
        initialAgentTab: "codex-acp",
      }),
    );
    expect(workspaceRestoreOpen).not.toHaveBeenCalled();
    expect(workspaceOpen).not.toHaveBeenCalled();
  });

  it("auto-opens an internal workspace when TimiAI is configured", async () => {
    const snapshot = agentSnapshot();
    snapshot.settings.selected_agent = "codex-acp";
    snapshot.settings.selected_codex_provider_profile_id = "timiai";
    snapshot.codex_acp.selected_profile_id = "timiai";
    snapshot.codex_acp.profiles = [
      profile("codex", "default", false, true, false),
      profile("codex", "timiai", true, true, true),
    ];
    vi.mocked(settingsGetAgentSnapshot).mockResolvedValue(snapshot);
    const onOpenSettings = vi.fn();
    const onWorkspaceOpened = vi.fn();

    render(<WelcomeLauncher onWorkspaceOpened={onWorkspaceOpened} onOpenSettings={onOpenSettings} />);

    await waitFor(() => expect(workspaceRestoreOpen).toHaveBeenCalled());
    await waitFor(() => expect(workspaceOpen).toHaveBeenCalledWith("D:\\work\\kodex"));
    await waitFor(() => expect(onWorkspaceOpened).toHaveBeenCalled());
    expect(onOpenSettings).not.toHaveBeenCalled();
  });

  it("auto-opens when CodeBuddy is installed", async () => {
    const snapshot = agentSnapshot();
    snapshot.agents.push({
      id: "codebuddy",
      label: "CodeBuddy",
      binary: "codebuddy",
      installed: true,
      detected_path: "C:\\tools\\codebuddy.exe",
      selected: false,
    });
    vi.mocked(settingsGetAgentSnapshot).mockResolvedValue(snapshot);
    const onOpenSettings = vi.fn();
    const onWorkspaceOpened = vi.fn();

    render(<WelcomeLauncher onWorkspaceOpened={onWorkspaceOpened} onOpenSettings={onOpenSettings} />);

    await waitFor(() => expect(workspaceRestoreOpen).toHaveBeenCalled());
    await waitFor(() => expect(workspaceOpen).toHaveBeenCalledWith("D:\\work\\kodex"));
    await waitFor(() => expect(onWorkspaceOpened).toHaveBeenCalled());
    expect(onOpenSettings).not.toHaveBeenCalled();
  });

  it("auto-opens an internal workspace when BYOK is configured", async () => {
    const snapshot = agentSnapshot();
    snapshot.settings.selected_agent = "claude-agent-acp";
    snapshot.settings.selected_claude_provider_profile_id = "byok";
    snapshot.claude.selected_profile_id = "byok";
    snapshot.claude.profiles = [
      profile("claude", "byok", true, true, false),
      profile("claude", "timiai", false, true, true),
    ];
    vi.mocked(settingsGetAgentSnapshot).mockResolvedValue(snapshot);
    const onOpenSettings = vi.fn();
    const onWorkspaceOpened = vi.fn();

    render(<WelcomeLauncher onWorkspaceOpened={onWorkspaceOpened} onOpenSettings={onOpenSettings} />);

    await waitFor(() => expect(workspaceRestoreOpen).toHaveBeenCalled());
    await waitFor(() => expect(workspaceOpen).toHaveBeenCalledWith("D:\\work\\kodex"));
    await waitFor(() => expect(onWorkspaceOpened).toHaveBeenCalled());
    expect(onOpenSettings).not.toHaveBeenCalled();
  });

  it("opens a guided remote Linux workspace flow from a saved machine", async () => {
    vi.mocked(workspaceGetRecent).mockResolvedValue([]);
    const snapshot = agentSnapshot();
    snapshot.codex_acp.profiles = [
      profile("codex", "default", false, true, false),
      profile("codex", "timiai", true, true, true),
    ];
    vi.mocked(settingsGetAgentSnapshot).mockResolvedValue(snapshot);
    const onWorkspaceOpened = vi.fn();

    render(<WelcomeLauncher onWorkspaceOpened={onWorkspaceOpened} onOpenSettings={vi.fn()} />);

    expect(screen.queryByLabelText("SSH 目标")).not.toBeInTheDocument();
    fireEvent.click(await screen.findByRole("button", { name: "打开远程目录" }));

    const panel = await screen.findByRole("region", { name: "打开远程目录" });
    await waitFor(() => expect(onRemoteOpenProgress).toHaveBeenCalled());
    const openRemote = within(panel).getByRole("button", { name: "打开目录" });
    expect(await screen.findByText(/Devbox/)).toBeInTheDocument();
    const agentGroup = screen.getByRole("radiogroup", { name: "remote_open_agent" });
    expect(within(agentGroup).getByRole("radio", { name: /Claude/ })).toHaveAttribute("aria-checked", "true");
    fireEvent.change(screen.getByLabelText("remote_open_path"), { target: { value: "/root/kodex-remote-acp-test" } });
    fireEvent.change(screen.getByLabelText("remote_open_password"), { target: { value: "ssh-secret" } });

    await waitFor(() => expect(openRemote).not.toBeDisabled());
    fireEvent.click(openRemote);

    await waitFor(() =>
      expect(workspaceOpenRemoteProfile).toHaveBeenCalledWith(expect.objectContaining({
        request_id: expect.any(String),
        profile_id: "remote-1",
        remote_path: "/root/kodex-remote-acp-test",
        ssh_password: "ssh-secret",
        agent_cli: "claude-agent-acp",
      })),
    );
    await waitFor(() => expect(onWorkspaceOpened).toHaveBeenCalled());
  });

  it("shows an immediate waiting state while opening a remote workspace", async () => {
    vi.mocked(workspaceGetRecent).mockResolvedValue([]);
    const snapshot = agentSnapshot();
    snapshot.codex_acp.profiles = [
      profile("codex", "default", false, true, false),
      profile("codex", "timiai", true, true, true),
    ];
    vi.mocked(settingsGetAgentSnapshot).mockResolvedValue(snapshot);
    const pendingOpen = deferred<never>();
    vi.mocked(workspaceOpenRemoteProfile).mockReturnValueOnce(pendingOpen.promise);
    const onWorkspaceOpened = vi.fn();

    render(<WelcomeLauncher onWorkspaceOpened={onWorkspaceOpened} onOpenSettings={vi.fn()} />);

    fireEvent.click(await screen.findByRole("button", { name: "打开远程目录" }));
    const panel = await screen.findByRole("region", { name: "打开远程目录" });
    fireEvent.change(within(panel).getByLabelText("remote_open_path"), { target: { value: "/srv/project" } });
    fireEvent.change(within(panel).getByLabelText("remote_open_password"), { target: { value: "ssh-secret" } });

    const openRemote = within(panel).getByRole("button", { name: "打开目录" });
    fireEvent.click(openRemote);

    const status = await within(panel).findByRole("status", { name: "远程工作区准备状态" });
    expect(within(status).getByText("正在准备远程工作区")).toBeInTheDocument();
    expect(within(status).getByText("正在建立连接并准备远程工作区")).toBeInTheDocument();
    expect(within(panel).getByLabelText("remote_open_progress")).toBeInTheDocument();
    expect(openRemote).toBeDisabled();
    expect(within(panel).getByLabelText("remote_open_path")).toBeDisabled();

    pendingOpen.resolve({} as never);
    await waitFor(() => expect(onWorkspaceOpened).toHaveBeenCalled());
  });

  it("reopens recent remote workspaces through the password flow", async () => {
    vi.mocked(workspaceGetRecent).mockResolvedValue([
      {
        path: "ssh://root@devbox:36000/srv/project",
        exists: true,
        remote: {
          profile_id: "remote-1",
          ssh_target: "root@devbox",
          ssh_port: 36000,
          remote_path: "/srv/project",
          agent_cli: "codex-acp",
          agent_command: "codex-acp",
          local_port: null,
          remote_port: null,
        },
      },
    ]);
    const onWorkspaceOpened = vi.fn();

    render(<WelcomeLauncher onWorkspaceOpened={onWorkspaceOpened} onOpenSettings={vi.fn()} />);

    fireEvent.click(await screen.findByRole("button", { name: /project/ }));
    const panel = await screen.findByRole("region", { name: "打开远程目录" });
    expect(within(panel).getByLabelText("remote_open_path")).toHaveValue("/srv/project");
    fireEvent.change(within(panel).getByLabelText("remote_open_password"), { target: { value: "ssh-secret" } });

    const openRemote = within(panel).getByRole("button", { name: "打开目录" });
    await waitFor(() => expect(openRemote).not.toBeDisabled());
    fireEvent.click(openRemote);

    await waitFor(() =>
      expect(workspaceOpenRemoteProfile).toHaveBeenCalledWith(expect.objectContaining({
        profile_id: "remote-1",
        remote_path: "/srv/project",
        ssh_password: "ssh-secret",
        agent_cli: "codex-acp",
      })),
    );
    expect(workspaceOpen).not.toHaveBeenCalled();
    await waitFor(() => expect(onWorkspaceOpened).toHaveBeenCalled());
  });
});
