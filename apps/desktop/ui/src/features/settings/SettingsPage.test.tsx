import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SettingsPage } from "./SettingsPage";
import {
  settingsGetAgentSnapshot,
  settingsGetLspSnapshot,
  settingsProbeLspServer,
  settingsResetLspServer,
  settingsSaveAgentProviderSecret,
  settingsSaveClaudeWoaConfig,
  settingsSaveLspServer,
  settingsSelectAgentProviderProfile,
  settingsStartClaudeWoaLogin,
  settingsRefreshClaudeWoaToken,
  settingsGetClaudeWoaLogin,
} from "../../lib/tauri";
import type { AgentProviderProfile, AgentSettingsSnapshot, LspSettingsSnapshot } from "../../types";

vi.mock("../../lib/tauri", async () => {
  const actual = await vi.importActual<typeof import("../../lib/tauri")>("../../lib/tauri");
  return {
    ...actual,
    settingsGetAgentSnapshot: vi.fn(),
    settingsDetectAgents: vi.fn(),
    settingsSelectAgent: vi.fn(),
    settingsSelectTheme: vi.fn(),
    settingsInstallAgent: vi.fn(),
    settingsGetLspSnapshot: vi.fn(),
    settingsProbeLspServer: vi.fn(),
    settingsSaveCodexAcpProviderKey: vi.fn(),
    settingsSaveCodexAcpVenusKey: vi.fn(),
    settingsSelectCodexAcpProvider: vi.fn(),
    settingsSelectCodexDefaultMode: vi.fn(),
    settingsSelectAgentProviderProfile: vi.fn(),
    settingsSaveAgentProviderSecret: vi.fn(),
    settingsSaveClaudeWoaConfig: vi.fn(),
    settingsStartClaudeWoaLogin: vi.fn(),
    settingsGetClaudeWoaLogin: vi.fn(),
    settingsCancelClaudeWoaLogin: vi.fn(),
    settingsRefreshClaudeWoaToken: vi.fn(),
    settingsSaveLspServer: vi.fn(),
    settingsResetLspServer: vi.fn(),
  };
});

function providerProfile(
  family: "codex" | "claude",
  id: string,
  label: string,
  proxyKind: AgentProviderProfile["proxy_kind"],
  selected: boolean,
  configured: boolean,
  requiresCredential: boolean,
): AgentProviderProfile {
  const isXiaomiTokenPlan = id === "xiaomi_mimo";
  return {
    family,
    id,
    label,
    proxy_kind: proxyKind,
    selected,
    configured,
    base_url: isXiaomiTokenPlan
      ? family === "codex"
        ? "https://token-plan-cn.xiaomimimo.com/v1"
        : "https://token-plan-cn.xiaomimimo.com/anthropic"
      : id === "default"
        ? null
        : `https://${id}.example/v1/chat/completions`,
    default_model: isXiaomiTokenPlan
      ? "MiMo-V2.5-Pro"
      : id === "default" || id === "woa"
        ? null
        : `${id}-model`,
    models: isXiaomiTokenPlan ? ["MiMo-V2.5-Pro", "MiMo-V2.5"] : [],
    credential_label: requiresCredential ? `${label} API key` : null,
    requires_credential: requiresCredential,
    help_text: `${label} help`,
  };
}

function codexProfiles(selected = "venus", configured: Partial<Record<string, boolean>> = {}): AgentProviderProfile[] {
  return [
    providerProfile("codex", "default", "默认", "codex_default", selected === "default", true, false),
    providerProfile("codex", "venus", "Venus", "completion_to_responses", selected === "venus", !!configured.venus, true),
    providerProfile("codex", "deepseek", "DeepSeek", "completion_to_responses", selected === "deepseek", !!configured.deepseek, true),
    providerProfile("codex", "kimi_code", "Kimi Code", "completion_to_responses", selected === "kimi_code", !!configured.kimi_code, true),
    providerProfile("codex", "xiaomi_mimo", "Xiaomi Token Plan", "completion_to_responses", selected === "xiaomi_mimo", !!configured.xiaomi_mimo, true),
  ];
}

function claudeProfiles(selected = "woa", configured: Partial<Record<string, boolean>> = {}): AgentProviderProfile[] {
  return [
    providerProfile("claude", "woa", "WOA", "claude_woa", selected === "woa", true, false),
    providerProfile("claude", "venus", "Venus", "completion_to_claude", selected === "venus", !!configured.venus, true),
    providerProfile("claude", "deepseek", "DeepSeek", "completion_to_claude", selected === "deepseek", !!configured.deepseek, true),
    providerProfile("claude", "kimi_code", "Kimi Code", "claude_native", selected === "kimi_code", !!configured.kimi_code, true),
    providerProfile("claude", "xiaomi_mimo", "Xiaomi Token Plan", "claude_native", selected === "xiaomi_mimo", !!configured.xiaomi_mimo, true),
  ];
}

const agentSnapshot: AgentSettingsSnapshot = {
  settings: {
    selected_agent: "codebuddy",
    acp_port: 0,
    theme: "graphite",
    lsp_servers: {},
    codex_connection_mode: "managed",
    selected_codex_provider_profile_id: "venus",
    selected_claude_provider_profile_id: "woa",
    claude_woa: {
      channel: "default",
      token_path: null,
      available_models: [],
    },
  },
  agents: [
    {
      id: "codebuddy",
      label: "CodeBuddy",
      binary: "codebuddy.exe",
      installed: true,
      detected_path: "C:\\tools\\codebuddy.exe",
      selected: true,
    },
    {
      id: "codex-acp",
      label: "Codex",
      binary: "codex-acp.exe",
      installed: true,
      detected_path: "C:\\tools\\codex-acp.exe",
      selected: false,
    },
    {
      id: "claude-agent-acp",
      label: "Claude",
      binary: "claude-agent-acp.exe",
      installed: true,
      detected_path: "C:\\tools\\claude-agent-acp.exe",
      selected: false,
    },
  ],
  env_override: null,
  codex_acp: {
    provider: "venus",
    selected_profile_id: "venus",
    profiles: codexProfiles("venus"),
    connection_mode: "managed",
    venus_key_configured: false,
    deepseek_key_configured: false,
    config_path: "C:\\Users\\yvonchen\\.kodex\\config.toml",
  },
  claude_woa: {
    channel: "default",
    selected_profile_id: "woa",
    profiles: claudeProfiles("woa"),
    token_path: "C:\\Users\\yvonchen\\.kodex\\claude-woa-token.json",
    token: {
      exists: false,
      malformed: false,
      access_token: null,
      refresh_token: null,
      expires_at: null,
      valid_for_minutes: null,
      refresh_needed: false,
      message: "Run WOA login to create a token.",
    },
  },
};

function lspSnapshot(command = "typescript-language-server", enabled = true): LspSettingsSnapshot {
  return {
    servers: [
      {
        languageId: "typescript",
        displayName: "TypeScript",
        enabled,
        command,
        args: ["--stdio"],
        defaultCommand: "typescript-language-server",
        defaultArgs: ["--stdio"],
        available: true,
        resolvedPath: "C:\\tools\\typescript-language-server.cmd",
        running: false,
        message: null,
        customized: command !== "typescript-language-server" || !enabled,
      },
    ],
  };
}

async function openAgentSettingsTab(label: "CodeBuddy" | "Codex" | "Claude") {
  const tab = await screen.findByRole("tab", { name: label });
  fireEvent.click(tab);
}

describe("SettingsPage LSP settings", () => {
  beforeEach(() => {
    vi.mocked(settingsGetAgentSnapshot).mockResolvedValue(agentSnapshot);
    vi.mocked(settingsGetLspSnapshot).mockResolvedValue(lspSnapshot());
    vi.mocked(settingsProbeLspServer).mockResolvedValue({
      available: true,
      resolvedPath: "C:\\tools\\custom-ts-lsp.cmd",
      message: null,
    });
    vi.mocked(settingsSaveLspServer).mockResolvedValue(lspSnapshot("custom-ts-lsp"));
    vi.mocked(settingsResetLspServer).mockResolvedValue(lspSnapshot());
    vi.mocked(settingsSaveAgentProviderSecret).mockImplementation(async (family, profileId) => {
      if (family === "codex") {
        return {
          ...agentSnapshot,
          codex_acp: {
            ...agentSnapshot.codex_acp,
            provider: profileId,
            selected_profile_id: profileId,
            profiles: codexProfiles(profileId, { [profileId]: true }),
            venus_key_configured: profileId === "venus",
            deepseek_key_configured: profileId === "deepseek",
          },
        };
      }
      return {
        ...agentSnapshot,
        claude_woa: {
          ...agentSnapshot.claude_woa,
          profiles: claudeProfiles("woa", { [profileId]: true }),
        },
      };
    });
    vi.mocked(settingsSelectAgentProviderProfile).mockImplementation(async (family, profileId) => ({
      ...agentSnapshot,
      settings: {
        ...agentSnapshot.settings,
        selected_codex_provider_profile_id:
          family === "codex" ? profileId : agentSnapshot.settings.selected_codex_provider_profile_id,
        selected_claude_provider_profile_id:
          family === "claude" ? profileId : agentSnapshot.settings.selected_claude_provider_profile_id,
      },
      codex_acp: {
        ...agentSnapshot.codex_acp,
        provider: family === "codex" ? profileId : agentSnapshot.codex_acp.provider,
        selected_profile_id: family === "codex" ? profileId : agentSnapshot.codex_acp.selected_profile_id,
        profiles: family === "codex" ? codexProfiles(profileId) : agentSnapshot.codex_acp.profiles,
      },
      claude_woa: {
        ...agentSnapshot.claude_woa,
        selected_profile_id: family === "claude" ? profileId : agentSnapshot.claude_woa.selected_profile_id,
        profiles: family === "claude" ? claudeProfiles(profileId) : agentSnapshot.claude_woa.profiles,
      },
    }));
    vi.mocked(settingsSaveClaudeWoaConfig).mockResolvedValue(agentSnapshot);
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
    vi.mocked(settingsRefreshClaudeWoaToken).mockResolvedValue({
      ...agentSnapshot,
      claude_woa: {
        ...agentSnapshot.claude_woa,
        token: {
          ...agentSnapshot.claude_woa.token,
          exists: true,
          access_token: "acce...alue",
          refresh_token: "refr...alue",
        },
      },
    });
    vi.mocked(settingsGetClaudeWoaLogin).mockResolvedValue({
      login_id: "login-1",
      state: "pending",
      message: null,
      snapshot: null,
    });
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("shows Claude first in the agent settings tabs", async () => {
    render(<SettingsPage onBack={vi.fn()} />);

    const tabs = await screen.findAllByRole("tab");

    expect(tabs.map((tab) => tab.textContent)).toEqual(["Claude", "Codex", "CodeBuddy"]);
  });

  it("loads, probes, saves, disables, and resets a language server", async () => {
    render(<SettingsPage onBack={vi.fn()} />);

    fireEvent.click(await screen.findByRole("button", { name: "LSP" }));
    await screen.findByText("TypeScript");
    const commandInput = screen.getByLabelText("命令") as HTMLInputElement;
    fireEvent.change(commandInput, { target: { value: "custom-ts-lsp" } });
    fireEvent.click(screen.getByText("探测"));

    await waitFor(() => expect(settingsProbeLspServer).toHaveBeenCalledWith("custom-ts-lsp"));
    await screen.findByText("已找到：C:\\tools\\custom-ts-lsp.cmd");

    fireEvent.click(screen.getByText("保存"));
    await waitFor(() =>
      expect(settingsSaveLspServer).toHaveBeenCalledWith({
        languageId: "typescript",
        enabled: true,
        command: "custom-ts-lsp",
        args: ["--stdio"],
      }),
    );

    const enableToggle = screen.getByRole("checkbox", { name: "启用" });
    fireEvent.click(enableToggle);
    fireEvent.click(screen.getByText("保存"));
    await waitFor(() =>
      expect(settingsSaveLspServer).toHaveBeenLastCalledWith({
        languageId: "typescript",
        enabled: false,
        command: "custom-ts-lsp",
        args: ["--stdio"],
      }),
    );

    fireEvent.click(screen.getByText("重置"));
    await waitFor(() => expect(settingsResetLspServer).toHaveBeenCalledWith("typescript"));
  });

  it("renders codex-acp configuration and saves Venus key without echoing it", async () => {
    render(<SettingsPage onBack={vi.fn()} />);

    await openAgentSettingsTab("Codex");
    expect(screen.queryByText("goose")).not.toBeInTheDocument();
    expect(screen.getByLabelText("byok_provider_profile")).toBeInTheDocument();
    expect(screen.getAllByText("未配置").length).toBeGreaterThan(0);
    expect(screen.getByText("C:\\Users\\yvonchen\\.kodex\\config.toml")).toBeInTheDocument();

    const saveButton = screen.getByRole("button", { name: "保存 Codex Venus key" });
    expect(saveButton).toBeDisabled();

    fireEvent.change(screen.getByLabelText("codex_venus_api_key"), { target: { value: "venus-secret" } });
    expect(saveButton).not.toBeDisabled();
    fireEvent.click(saveButton);

    await waitFor(() => expect(settingsSaveAgentProviderSecret).toHaveBeenCalledWith("codex", "venus", "venus-secret"));
    expect(settingsSaveAgentProviderSecret).not.toHaveBeenCalledWith("claude", "venus", "venus-secret");
    await screen.findByText("Venus API key 已保存，Codex 通道已切换到 Venus");
    expect(screen.getByLabelText("codex_venus_api_key")).toHaveValue("");
    expect(screen.queryByDisplayValue("venus-secret")).not.toBeInTheDocument();
  });

  it("saves DeepSeek provider key without echoing it", async () => {
    render(<SettingsPage onBack={vi.fn()} />);

    await openAgentSettingsTab("Codex");
    fireEvent.change(screen.getByLabelText("byok_provider_profile"), { target: { value: "deepseek" } });

    const saveButton = screen.getByRole("button", { name: "保存 DeepSeek key" });
    fireEvent.change(screen.getByLabelText("byok_api_key"), { target: { value: "deepseek-secret" } });
    fireEvent.click(saveButton);

    await waitFor(() =>
      expect(settingsSaveAgentProviderSecret).toHaveBeenCalledWith("codex", "deepseek", "deepseek-secret"),
    );
    expect(settingsSaveAgentProviderSecret).toHaveBeenCalledWith("claude", "deepseek", "deepseek-secret");
    await screen.findByText("DeepSeek API key 已更新，后续新建会话生效");
    expect(screen.getByLabelText("byok_api_key")).toHaveValue("");
    expect(screen.queryByDisplayValue("deepseek-secret")).not.toBeInTheDocument();
  });

  it("adds a Kimi Code key to the shared BYOK model pool without echoing it", async () => {
    render(<SettingsPage onBack={vi.fn()} />);

    await openAgentSettingsTab("Codex");
    await screen.findByText("BYOK 模型池");
    fireEvent.change(screen.getByLabelText("byok_provider_profile"), { target: { value: "kimi_code" } });

    fireEvent.change(screen.getByLabelText("byok_api_key"), { target: { value: "kimi-secret" } });
    fireEvent.click(screen.getByRole("button", { name: "保存 Kimi Code key" }));

    await waitFor(() => expect(settingsSaveAgentProviderSecret).toHaveBeenCalledWith("codex", "kimi_code", "kimi-secret"));
    await waitFor(() => expect(settingsSaveAgentProviderSecret).toHaveBeenCalledWith("claude", "kimi_code", "kimi-secret"));
    await screen.findByText("Kimi Code API key 已更新，后续新建会话生效");
    expect(screen.getByLabelText("byok_api_key")).toHaveValue("");
    expect(screen.queryByDisplayValue("kimi-secret")).not.toBeInTheDocument();
  });

  it("lets BYOK source selection diverge from the current Codex channel", async () => {
    vi.mocked(settingsGetAgentSnapshot).mockResolvedValue({
      ...agentSnapshot,
      codex_acp: {
        ...agentSnapshot.codex_acp,
        provider: "deepseek",
        selected_profile_id: "deepseek",
        profiles: codexProfiles("deepseek", { deepseek: true }),
      },
    });
    render(<SettingsPage onBack={vi.fn()} />);

    await openAgentSettingsTab("Codex");
    const sourceSelect = screen.getByLabelText("byok_provider_profile");
    expect(sourceSelect).toHaveValue("deepseek");

    fireEvent.change(sourceSelect, { target: { value: "xiaomi_mimo" } });
    expect(sourceSelect).toHaveValue("xiaomi_mimo");
    expect(screen.getByText("模型：MiMo-V2.5-Pro、MiMo-V2.5")).toBeInTheDocument();
    expect(screen.getByText("https://token-plan-cn.xiaomimimo.com/v1")).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("byok_api_key"), { target: { value: "mimo-secret" } });
    fireEvent.click(screen.getByRole("button", { name: "保存 Xiaomi Token Plan key" }));

    await waitFor(() =>
      expect(settingsSaveAgentProviderSecret).toHaveBeenCalledWith("codex", "xiaomi_mimo", "mimo-secret"),
    );
    expect(settingsSaveAgentProviderSecret).toHaveBeenCalledWith("claude", "xiaomi_mimo", "mimo-secret");
    await screen.findByText("Xiaomi Token Plan API key 已更新，后续新建会话生效");
    expect(sourceSelect).toHaveValue("xiaomi_mimo");
  });

  it("shows configured BYOK providers as a single shared model pool", async () => {
    vi.mocked(settingsGetAgentSnapshot).mockResolvedValue({
      ...agentSnapshot,
      codex_acp: {
        ...agentSnapshot.codex_acp,
        provider: "deepseek",
        selected_profile_id: "deepseek",
        profiles: codexProfiles("deepseek", { venus: true, deepseek: true }),
        venus_key_configured: true,
        deepseek_key_configured: true,
      },
    });
    render(<SettingsPage onBack={vi.fn()} />);

    await openAgentSettingsTab("Codex");
    await screen.findByText("1/3 已配置");
    expect(screen.getByText("DeepSeek · 已配置")).toBeInTheDocument();
    expect(screen.getByText("Kimi Code · 未配置")).toBeInTheDocument();
    expect(screen.queryByText("Venus · 已配置")).not.toBeInTheDocument();
    expect(settingsSaveAgentProviderSecret).not.toHaveBeenCalled();
  });

  it("describes Codex and Claude as channels backed by BYOK models", async () => {
    render(<SettingsPage onBack={vi.fn()} />);

    await openAgentSettingsTab("Codex");
    expect(screen.getByText("Codex 通道")).toBeInTheDocument();
    const codexChannel = screen.getByRole("radiogroup", { name: "Codex channel" });
    expect(within(codexChannel).getByRole("button", { name: /默认/ })).toBeInTheDocument();
    await openAgentSettingsTab("Claude");
    expect(screen.getByText("Claude 通道")).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: /WOA/ }).length).toBeGreaterThan(0);
    expect(screen.getAllByRole("button", { name: /Venus/ }).length).toBeGreaterThan(0);
    expect(screen.getAllByText(/BYOK/).length).toBeGreaterThan(0);
    expect(screen.queryByLabelText("codex_provider_profile")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("claude_provider_profile")).not.toBeInTheDocument();
  });

  it("saves a Claude Venus key without adding Venus to the BYOK pool", async () => {
    render(<SettingsPage onBack={vi.fn()} />);

    await openAgentSettingsTab("Claude");
    await screen.findByText("Claude 通道");
    const claudeChannel = screen.getByRole("radiogroup", { name: "Claude channel" });
    fireEvent.click(within(claudeChannel).getByRole("button", { name: /Venus/ }));
    await waitFor(() => expect(settingsSelectAgentProviderProfile).toHaveBeenCalledWith("claude", "venus"));

    const saveButton = screen.getByRole("button", { name: "保存 Claude Venus key" });
    expect(saveButton).toBeDisabled();

    fireEvent.change(screen.getByLabelText("claude_venus_api_key"), { target: { value: "claude-venus-secret" } });
    fireEvent.click(saveButton);

    await waitFor(() =>
      expect(settingsSaveAgentProviderSecret).toHaveBeenCalledWith("claude", "venus", "claude-venus-secret"),
    );
    await waitFor(() => expect(settingsSelectAgentProviderProfile).toHaveBeenCalledWith("claude", "venus"));
    await screen.findByText("Venus API key 已保存，Claude 通道已切换到 Venus");
    expect(screen.getByLabelText("claude_venus_api_key")).toHaveValue("");
    expect(screen.queryByText("Venus · 未配置")).not.toBeInTheDocument();
    expect(screen.queryByDisplayValue("claude-venus-secret")).not.toBeInTheDocument();
  });

  it("starts Claude WOA login without exposing token secrets", async () => {
    render(<SettingsPage onBack={vi.fn()} />);

    await openAgentSettingsTab("Claude");
    fireEvent.click(screen.getByRole("button", { name: "WOA 登录" }));

    await waitFor(() =>
      expect(settingsSaveClaudeWoaConfig).toHaveBeenCalledWith({
        channel: "default",
        tokenPath: null,
        availableModels: [],
      }),
    );
    await waitFor(() => expect(settingsStartClaudeWoaLogin).toHaveBeenCalled());
    await waitFor(() => {
      expect(screen.getAllByText(/ABCD-EFGH/).length).toBeGreaterThan(0);
    });
    expect(screen.queryByText(/access-secret/)).not.toBeInTheDocument();
  });

  it("saves a custom Claude WOA model list", async () => {
    render(<SettingsPage onBack={vi.fn()} />);

    await openAgentSettingsTab("Claude");
    fireEvent.change(screen.getByLabelText("claude_woa_models"), {
      target: {
        value: " claude-sonnet-4-6[1m]\nclaude-opus-4-7[1m]\nclaude-sonnet-4-6[1m]\n",
      },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存模型列表" }));

    await waitFor(() =>
      expect(settingsSaveClaudeWoaConfig).toHaveBeenCalledWith({
        channel: "default",
        tokenPath: null,
        availableModels: ["claude-sonnet-4-6[1m]", "claude-opus-4-7[1m]"],
      }),
    );
  });
});
