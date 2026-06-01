import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { WelcomeLauncher } from "./WelcomeLauncher";
import {
  openExternalUrl,
  settingsDetectIoaEnvironment,
  settingsGetAgentSnapshot,
  settingsSaveClaudeWoaConfig,
  settingsSelectAgent,
  settingsSelectAgentProviderProfile,
  settingsStartClaudeWoaLogin,
  startupPerfMark,
  workspaceGetRecent,
  workspaceOpen,
  workspaceRestoreOpen,
} from "../../lib/tauri";
import type { AgentProviderProfile, AgentSettingsSnapshot, IoaEnvironmentStatus } from "../../types";

vi.mock("../../lib/tauri", async () => {
  const actual = await vi.importActual<typeof import("../../lib/tauri")>("../../lib/tauri");
  return {
    ...actual,
    openExternalUrl: vi.fn(),
    settingsCancelClaudeWoaLogin: vi.fn(),
    settingsDetectIoaEnvironment: vi.fn(),
    settingsGetAgentSnapshot: vi.fn(),
    settingsGetClaudeWoaLogin: vi.fn(),
    settingsSaveClaudeWoaConfig: vi.fn(),
    settingsSelectAgent: vi.fn(),
    settingsSelectAgentProviderProfile: vi.fn(),
    settingsStartClaudeWoaLogin: vi.fn(),
    startupPerfMark: vi.fn(),
    workspaceGetRecent: vi.fn(),
    workspaceOpen: vi.fn(),
    workspaceRemoveRecent: vi.fn(),
    workspaceRestoreOpen: vi.fn(),
  };
});

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(),
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
    label: id === "woa" ? "WOA" : "BYOK",
    proxy_kind: id === "woa" ? "claude_woa" : "claude_native",
    selected,
    configured,
    base_url: null,
    default_model: null,
    models: [],
    credential_label: requiresCredential ? "API key" : null,
    requires_credential: requiresCredential,
    help_text: `${id} help`,
  };
}

function agentSnapshot(tokenExists = false): AgentSettingsSnapshot {
  return {
    settings: {
      selected_agent: "claude-agent-acp",
      acp_port: 0,
      theme: "graphite",
      lsp_servers: {},
      codex_connection_mode: "managed",
      selected_codex_provider_profile_id: "default",
      selected_claude_provider_profile_id: "woa",
      claude_woa: {
        channel: "default",
        token_path: null,
        available_models: ["claude-opus-4-7[1m]"],
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
      venus_key_configured: false,
      deepseek_key_configured: false,
      config_path: "C:\\Users\\yvonchen\\.kodex\\config.toml",
    },
    claude_woa: {
      channel: "default",
      selected_profile_id: "woa",
      profiles: [
        profile("claude", "woa", true, true, false),
        profile("claude", "byok", false, false, true),
      ],
      token_path: "C:\\Users\\yvonchen\\.kodex\\claude-woa-token.json",
      token: {
        exists: tokenExists,
        malformed: false,
        access_token: null,
        refresh_token: null,
        expires_at: null,
        valid_for_minutes: null,
        refresh_needed: false,
        message: tokenExists ? "WOA token ready." : "Run WOA login to create a token.",
      },
    },
  };
}

function ioaEnvironment(company = true): IoaEnvironmentStatus {
  return {
    is_company_export_ip: company,
    is_internal: false,
    company_environment: company,
    recommended_setup: company ? "woa" as const : "codex_byok" as const,
    detected: true,
    timestamp_ms: Date.now(),
    message: null,
  };
}

function inconclusiveIoaEnvironment(): IoaEnvironmentStatus {
  return {
    is_company_export_ip: false,
    is_internal: false,
    company_environment: false,
    recommended_setup: "codex_byok",
    detected: false,
    timestamp_ms: Date.now(),
    message: "request timed out",
  };
}

describe("WelcomeLauncher WOA onboarding", () => {
  beforeEach(() => {
    vi.mocked(startupPerfMark).mockResolvedValue(undefined);
    vi.mocked(workspaceGetRecent).mockResolvedValue([
      { path: "D:\\work\\kodex", exists: true },
    ]);
    vi.mocked(settingsGetAgentSnapshot).mockResolvedValue(agentSnapshot(false));
    vi.mocked(settingsDetectIoaEnvironment).mockResolvedValue(ioaEnvironment(true));
    vi.mocked(settingsSaveClaudeWoaConfig).mockResolvedValue(agentSnapshot(false));
    vi.mocked(settingsSelectAgent).mockResolvedValue(agentSnapshot(false));
    vi.mocked(settingsSelectAgentProviderProfile).mockResolvedValue(agentSnapshot(false));
    vi.mocked(settingsStartClaudeWoaLogin).mockResolvedValue({
      login_id: "login-1",
      verification_uri: "https://copilot.code.woa.com/login",
      verification_uri_complete: null,
      user_code: "ABCD-EFGH",
      expires_at_ms: Date.now() + 600_000,
      interval_ms: 5000,
      channel: "default",
      token_path: "C:\\Users\\yvonchen\\.kodex\\claude-woa-token.json",
    });
    vi.mocked(workspaceRestoreOpen).mockResolvedValue(null);
    vi.mocked(workspaceOpen).mockResolvedValue({} as never);
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("opens Settings with a WOA warning before auto-opening a WOA-backed workspace", async () => {
    const onOpenSettings = vi.fn();
    render(<WelcomeLauncher onWorkspaceOpened={vi.fn()} onOpenSettings={onOpenSettings} />);

    await waitFor(() =>
      expect(onOpenSettings).toHaveBeenCalledWith({
        startupNotice: { kind: "woa" },
        initialAgentTab: "claude-agent-acp",
      }),
    );

    expect(screen.getByText("Run WOA login to create a token.")).toBeInTheDocument();
    expect(workspaceRestoreOpen).not.toHaveBeenCalled();
    expect(workspaceOpen).not.toHaveBeenCalled();
  });

  it("falls back to Codex BYOK onboarding when network detection is inconclusive", async () => {
    vi.mocked(settingsDetectIoaEnvironment).mockResolvedValue(inconclusiveIoaEnvironment());
    const onOpenSettings = vi.fn();

    render(<WelcomeLauncher onWorkspaceOpened={vi.fn()} onOpenSettings={onOpenSettings} />);

    await waitFor(() =>
      expect(onOpenSettings).toHaveBeenCalledWith({
        startupNotice: { kind: "codex_byok", message: "request timed out" },
        initialAgentTab: "codex-acp",
      }),
    );

    expect(screen.getByText("request timed out")).toBeInTheDocument();
    expect(workspaceRestoreOpen).not.toHaveBeenCalled();
    expect(workspaceOpen).not.toHaveBeenCalled();
  });

  it("starts WOA login directly from the welcome screen", async () => {
    render(<WelcomeLauncher onWorkspaceOpened={vi.fn()} onOpenSettings={vi.fn()} />);

    fireEvent.click(await screen.findByRole("button", { name: "使用 Claude WOA" }));

    await waitFor(() => expect(settingsSelectAgent).toHaveBeenCalledWith("claude-agent-acp"));
    await waitFor(() => expect(settingsSelectAgentProviderProfile).toHaveBeenCalledWith("claude", "woa"));
    await waitFor(() =>
      expect(settingsSaveClaudeWoaConfig).toHaveBeenCalledWith({
        channel: "default",
        tokenPath: null,
        availableModels: ["claude-opus-4-7[1m]"],
      }),
    );
    await waitFor(() => expect(settingsStartClaudeWoaLogin).toHaveBeenCalled());
    expect(await screen.findByText(/ABCD-EFGH/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "https://copilot.code.woa.com/login" }));
    await waitFor(() => expect(openExternalUrl).toHaveBeenCalledWith("https://copilot.code.woa.com/login"));
  });

  it("opens Settings with a Codex BYOK warning on external networks instead of WOA login", async () => {
    vi.mocked(settingsDetectIoaEnvironment).mockResolvedValue(ioaEnvironment(false));
    const onOpenSettings = vi.fn();

    render(<WelcomeLauncher onWorkspaceOpened={vi.fn()} onOpenSettings={onOpenSettings} />);

    await waitFor(() =>
      expect(onOpenSettings).toHaveBeenCalledWith({
        startupNotice: { kind: "codex_byok" },
        initialAgentTab: "codex-acp",
      }),
    );

    expect(screen.queryByRole("heading", { name: "初始化内网通道" })).not.toBeInTheDocument();
    expect(workspaceRestoreOpen).not.toHaveBeenCalled();
    expect(workspaceOpen).not.toHaveBeenCalled();
  });

  it("auto-opens an internal workspace when TimiAI is configured", async () => {
    const snapshot = agentSnapshot(false);
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

  it("auto-opens an internal workspace when Codex WOA has the shared WOA token", async () => {
    const snapshot = agentSnapshot(true);
    snapshot.settings.selected_agent = "claude-agent-acp";
    snapshot.settings.selected_codex_provider_profile_id = "woa";
    snapshot.settings.selected_claude_provider_profile_id = "byok";
    snapshot.codex_acp.selected_profile_id = "woa";
    snapshot.codex_acp.profiles = [
      profile("codex", "default", false, true, false),
      profile("codex", "woa", true, true, false),
    ];
    snapshot.claude_woa.selected_profile_id = "byok";
    snapshot.claude_woa.profiles = [
      profile("claude", "woa", false, true, false),
      profile("claude", "byok", true, false, true),
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

  it("auto-opens an internal workspace when Venus is configured", async () => {
    const snapshot = agentSnapshot(false);
    snapshot.settings.selected_agent = "claude-agent-acp";
    snapshot.settings.selected_claude_provider_profile_id = "venus";
    snapshot.claude_woa.selected_profile_id = "venus";
    snapshot.claude_woa.profiles = [
      profile("claude", "woa", false, true, false),
      profile("claude", "venus", true, true, true),
      profile("claude", "byok", false, false, true),
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

  it("does not treat TimiAI credentials as external Codex BYOK setup", async () => {
    const snapshot = agentSnapshot(false);
    snapshot.codex_acp.profiles = [
      profile("codex", "default", true, true, false),
      profile("codex", "timiai", false, true, true),
    ];
    vi.mocked(settingsGetAgentSnapshot).mockResolvedValue(snapshot);
    vi.mocked(settingsDetectIoaEnvironment).mockResolvedValue(ioaEnvironment(false));
    const onOpenSettings = vi.fn();

    render(<WelcomeLauncher onWorkspaceOpened={vi.fn()} onOpenSettings={onOpenSettings} />);

    await waitFor(() =>
      expect(onOpenSettings).toHaveBeenCalledWith({
        startupNotice: { kind: "codex_byok" },
        initialAgentTab: "codex-acp",
      }),
    );

    expect(workspaceRestoreOpen).not.toHaveBeenCalled();
    expect(workspaceOpen).not.toHaveBeenCalled();
  });
});
