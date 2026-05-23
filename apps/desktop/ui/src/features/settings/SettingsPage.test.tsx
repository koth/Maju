import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SettingsPage } from "./SettingsPage";
import {
  settingsGetAgentSnapshot,
  settingsGetLspSnapshot,
  settingsProbeLspServer,
  settingsResetLspServer,
  settingsSaveCodexAcpProviderKey,
  settingsSaveCodexAcpVenusKey,
  settingsSaveClaudeWoaConfig,
  settingsSaveLspServer,
  settingsSelectCodexAcpProvider,
  settingsSelectCodexDefaultMode,
  settingsStartClaudeWoaLogin,
  settingsRefreshClaudeWoaToken,
  settingsGetClaudeWoaLogin,
} from "../../lib/tauri";
import type { AgentSettingsSnapshot, LspSettingsSnapshot } from "../../types";

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
    settingsSaveClaudeWoaConfig: vi.fn(),
    settingsStartClaudeWoaLogin: vi.fn(),
    settingsGetClaudeWoaLogin: vi.fn(),
    settingsCancelClaudeWoaLogin: vi.fn(),
    settingsRefreshClaudeWoaToken: vi.fn(),
    settingsSaveLspServer: vi.fn(),
    settingsResetLspServer: vi.fn(),
  };
});

const agentSnapshot: AgentSettingsSnapshot = {
  settings: {
    selected_agent: "codebuddy",
    acp_port: 0,
    theme: "graphite",
    lsp_servers: {},
    codex_connection_mode: "managed",
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
    connection_mode: "managed",
    venus_key_configured: false,
    deepseek_key_configured: false,
    config_path: "C:\\Users\\yvonchen\\.kodex\\config.toml",
  },
  claude_woa: {
    channel: "default",
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
    vi.mocked(settingsSaveCodexAcpVenusKey).mockResolvedValue({
      ...agentSnapshot,
      codex_acp: {
        ...agentSnapshot.codex_acp,
        venus_key_configured: true,
      },
    });
    vi.mocked(settingsSaveCodexAcpProviderKey).mockResolvedValue({
      ...agentSnapshot,
      codex_acp: {
        ...agentSnapshot.codex_acp,
        provider: "deepseek",
        deepseek_key_configured: true,
      },
    });
    vi.mocked(settingsSelectCodexAcpProvider).mockResolvedValue({
      ...agentSnapshot,
      codex_acp: {
        ...agentSnapshot.codex_acp,
        provider: "venus",
        venus_key_configured: true,
        deepseek_key_configured: true,
      },
    });
    vi.mocked(settingsSelectCodexDefaultMode).mockResolvedValue({
      ...agentSnapshot,
      settings: {
        ...agentSnapshot.settings,
        codex_connection_mode: "default",
      },
      codex_acp: {
        ...agentSnapshot.codex_acp,
        provider: "default",
        connection_mode: "default",
      },
    });
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

    await screen.findByText("Codex");
    expect(screen.getAllByText("未配置").length).toBeGreaterThan(0);
    expect(screen.getByText("C:\\Users\\yvonchen\\.kodex\\config.toml")).toBeInTheDocument();

    const saveButton = screen.getByRole("button", { name: "保存 Venus key" });
    expect(saveButton).toBeDisabled();

    fireEvent.change(screen.getByLabelText("codex_acp_api_key"), { target: { value: "venus-secret" } });
    expect(saveButton).not.toBeDisabled();
    fireEvent.click(saveButton);

    await waitFor(() => expect(settingsSaveCodexAcpVenusKey).toHaveBeenCalledWith("venus-secret"));
    await screen.findByText("Venus API key 已保存");
    expect(screen.getByLabelText("codex_acp_api_key")).toHaveValue("");
    expect(screen.queryByDisplayValue("venus-secret")).not.toBeInTheDocument();
  });

  it("saves DeepSeek provider key without echoing it", async () => {
    render(<SettingsPage onBack={vi.fn()} />);

    await screen.findByText("Codex");
    fireEvent.click(screen.getByRole("button", { name: /DeepSeek/ }));

    const saveButton = screen.getByRole("button", { name: "保存 DeepSeek key" });
    fireEvent.change(screen.getByLabelText("codex_acp_api_key"), { target: { value: "deepseek-secret" } });
    fireEvent.click(saveButton);

    await waitFor(() =>
      expect(settingsSaveCodexAcpProviderKey).toHaveBeenCalledWith("deepseek", "deepseek-secret"),
    );
    await screen.findByText("DeepSeek API key 已保存");
    expect(screen.getByLabelText("codex_acp_api_key")).toHaveValue("");
    expect(screen.queryByDisplayValue("deepseek-secret")).not.toBeInTheDocument();
  });

  it("switches to an already configured Codex provider without requiring a new key", async () => {
    vi.mocked(settingsGetAgentSnapshot).mockResolvedValue({
      ...agentSnapshot,
      codex_acp: {
        ...agentSnapshot.codex_acp,
        provider: "deepseek",
        venus_key_configured: true,
        deepseek_key_configured: true,
      },
    });
    render(<SettingsPage onBack={vi.fn()} />);

    await screen.findByText("当前：DeepSeek");
    fireEvent.click(screen.getByRole("button", { name: /Venus/ }));

    await waitFor(() => expect(settingsSelectCodexAcpProvider).toHaveBeenCalledWith("venus"));
    await screen.findByText("已切换为 Venus 配置");
    expect(settingsSaveCodexAcpVenusKey).not.toHaveBeenCalled();
  });

  it("selects default Codex mode without requiring an API key", async () => {
    render(<SettingsPage onBack={vi.fn()} />);

    await screen.findByText("Codex");
    fireEvent.click(screen.getByRole("button", { name: /默认/ }));

    await waitFor(() => expect(settingsSelectCodexDefaultMode).toHaveBeenCalled());
    await screen.findByText("已切换为默认 Codex 配置");
    expect(screen.getByText(/启动时不设置/)).toBeInTheDocument();
    expect(screen.queryByLabelText("codex_acp_api_key")).not.toBeInTheDocument();
  });

  it("starts Claude WOA login without exposing token secrets", async () => {
    render(<SettingsPage onBack={vi.fn()} />);

    await screen.findByText("Claude");
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

    await screen.findByText("Claude");
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
