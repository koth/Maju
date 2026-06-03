import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SettingsPage } from "./SettingsPage";
import {
  settingsGetAgentSnapshot,
  settingsGetLspSnapshot,
  settingsGetRemoteProfiles,
  settingsProbeLspServer,
  settingsResetLspServer,
  settingsResetProviderModels,
  settingsSaveAgentProviderSecret,
  settingsSaveLspServer,
  settingsSaveProviderModels,
  settingsSaveRemoteProfile,
  settingsDeleteRemoteProfile,
  settingsValidateRemoteProfile,
  settingsSelectAgentProviderProfile,
} from "../../lib/tauri";
import {
  checkForAppUpdate,
  getCurrentAppVersion,
  installPendingAppUpdate,
} from "../../lib/updater";
import type { AgentProviderProfile, AgentSettingsSnapshot, LspSettingsSnapshot, RemoteMachineProfilesSnapshot } from "../../types";

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
    settingsGetRemoteProfiles: vi.fn(),
    settingsSaveRemoteProfile: vi.fn(),
    settingsDeleteRemoteProfile: vi.fn(),
    settingsValidateRemoteProfile: vi.fn(),
    settingsProbeLspServer: vi.fn(),
    settingsSaveProviderModels: vi.fn(),
    settingsResetProviderModels: vi.fn(),
    settingsSaveCodexAcpProviderKey: vi.fn(),
    settingsSelectCodexAcpProvider: vi.fn(),
    settingsSelectCodexDefaultMode: vi.fn(),
    settingsSelectAgentProviderProfile: vi.fn(),
    settingsSaveAgentProviderSecret: vi.fn(),
    settingsSaveLspServer: vi.fn(),
    settingsResetLspServer: vi.fn(),
  };
});

vi.mock("../../lib/updater", () => ({
  checkForAppUpdate: vi.fn(),
  getCurrentAppVersion: vi.fn(),
  installPendingAppUpdate: vi.fn(),
}));

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
  const isTimiAi = id === "timiai";
  const isCommandCode = id === "commandcode";
  return {
    family,
    id,
    label,
    proxy_kind: proxyKind,
    selected,
    configured,
    base_url: id === "default" || id === "byok"
      ? null
      : isTimiAi
      ? "http://api.timiai.woa.com/ai_api_manage/llmproxy"
      : isCommandCode
      ? "https://api.commandcode.ai/provider/v1"
      : isXiaomiTokenPlan
      ? family === "codex"
        ? "https://token-plan-cn.xiaomimimo.com/v1"
        : "https://token-plan-cn.xiaomimimo.com/anthropic"
      : `https://${id}.example/v1/chat/completions`,
    default_model: id === "default" || id === "byok"
      ? null
      : isTimiAi
      ? family === "codex"
        ? "gpt-5.5"
        : "claude-opus-4.8"
      : isCommandCode
      ? "claude-sonnet-4-6"
      : isXiaomiTokenPlan
      ? "MiMo-V2.5-Pro"
      : `${id}-model`,
    models: isTimiAi
      ? ["gpt-5.5", "gpt-5.4", "claude-opus-4.8"]
      : isCommandCode
        ? ["claude-sonnet-4-6", "claude-opus-4-8", "deepseek/deepseek-v4-pro"]
      : isXiaomiTokenPlan
        ? ["MiMo-V2.5-Pro", "MiMo-V2.5"]
        : [],
    credential_label: requiresCredential ? `${label} API key` : null,
    requires_credential: requiresCredential,
    help_text: `${label} help`,
  };
}

function codexProfiles(selected = "byok", configured: Partial<Record<string, boolean>> = {}): AgentProviderProfile[] {
  return [
    providerProfile("codex", "default", "默认", "codex_default", selected === "default", true, false),
    providerProfile("codex", "byok", "BYOK", "completion_to_responses", selected === "byok", true, false),
    providerProfile("codex", "timiai", "TimiAI", "responses", selected === "timiai", !!configured.timiai, true),
    providerProfile("codex", "commandcode", "CommandCode", "completion_to_responses", selected === "commandcode", !!configured.commandcode, true),
    providerProfile("codex", "deepseek", "DeepSeek", "completion_to_responses", selected === "deepseek", !!configured.deepseek, true),
    providerProfile("codex", "kimi_code", "Kimi Code", "completion_to_responses", selected === "kimi_code", !!configured.kimi_code, true),
    providerProfile("codex", "xiaomi_mimo", "Xiaomi Token Plan", "completion_to_responses", selected === "xiaomi_mimo", !!configured.xiaomi_mimo, true),
  ];
}

function claudeProfiles(selected = "byok", configured: Partial<Record<string, boolean>> = {}): AgentProviderProfile[] {
  return [
    providerProfile("claude", "byok", "BYOK", "claude_native", selected === "byok", true, false),
    providerProfile("claude", "timiai", "TimiAI", "claude_native", selected === "timiai", !!configured.timiai, true),
    providerProfile("claude", "commandcode", "CommandCode", "claude_native", selected === "commandcode", !!configured.commandcode, true),
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
    selected_codex_provider_profile_id: "byok",
    selected_claude_provider_profile_id: "byok",
    claude: {
      available_models: ["claude-opus-4-7[1m]", "claude-opus-4-6[1m]"],
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
    provider: "byok",
    selected_profile_id: "byok",
    profiles: codexProfiles("byok"),
    connection_mode: "managed",
    deepseek_key_configured: false,
    config_path: "C:\\Users\\yvonchen\\.kodex\\config.toml",
  },
  claude: {
    selected_profile_id: "byok",
    profiles: claudeProfiles("byok"),
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

function remoteProfilesSnapshot(validated = false): RemoteMachineProfilesSnapshot {
  return {
    profiles: [
      {
        id: "remote-1",
        display_name: "Devbox",
        ssh_target: "root@devbox",
        ssh_port: 36000,
        created_at_ms: 1,
        updated_at_ms: 2,
        last_validation: validated
          ? {
              ok: true,
              checked_at_ms: 1_700_000_000_000,
              remote_path: "/srv/project",
              phases: [
                { phase: "ssh", status: "succeeded", elapsed_ms: 12, message: null },
                { phase: "remote_path", status: "succeeded", elapsed_ms: 10, message: null },
                { phase: "agent_command", status: "skipped", elapsed_ms: 0, message: "No agent selected" },
                { phase: "acp_ready", status: "skipped", elapsed_ms: 0, message: "ACP readiness probe not requested" },
              ],
            }
          : null,
      },
    ],
  };
}

function remoteProfilesWithDuplicateTarget(): RemoteMachineProfilesSnapshot {
  return {
    profiles: [
      ...remoteProfilesSnapshot().profiles,
      {
        id: "remote-2",
        display_name: "Staging",
        ssh_target: "root@staging",
        ssh_port: 22,
        created_at_ms: 3,
        updated_at_ms: 4,
        last_validation: null,
      },
    ],
  };
}

function remoteProfilesValidationFailed(): RemoteMachineProfilesSnapshot {
  const [profile] = remoteProfilesSnapshot().profiles;
  return {
    profiles: [
      {
        ...profile,
        last_validation: {
          ok: false,
          checked_at_ms: 1_700_000_000_000,
          remote_path: "/srv/missing",
          phases: [
            { phase: "ssh", status: "succeeded", elapsed_ms: 12, message: null },
            { phase: "remote_path", status: "failed", elapsed_ms: 9, message: "No such directory" },
            { phase: "agent_command", status: "skipped", elapsed_ms: 0, message: "Skipped after remote path failure" },
            { phase: "acp_ready", status: "skipped", elapsed_ms: 0, message: "ACP readiness probe not requested" },
          ],
        },
      },
    ],
  };
}

async function openAgentSettingsTab(label: "CodeBuddy" | "Codex" | "Claude") {
  const tab = await screen.findByRole("tab", { name: label });
  fireEvent.click(tab);
}

async function selectByokProvider(name: string | RegExp) {
  const trigger = await screen.findByLabelText("byok_provider_profile");
  fireEvent.click(trigger);
  fireEvent.click(await screen.findByRole("option", { name }));
  return screen.getByLabelText("byok_provider_profile");
}

describe("SettingsPage LSP settings", () => {
  beforeEach(() => {
    vi.mocked(getCurrentAppVersion).mockResolvedValue("0.1.0");
    vi.mocked(checkForAppUpdate).mockResolvedValue(null);
    vi.mocked(installPendingAppUpdate).mockResolvedValue(undefined);
    vi.mocked(settingsGetAgentSnapshot).mockResolvedValue(agentSnapshot);
    vi.mocked(settingsGetLspSnapshot).mockResolvedValue(lspSnapshot());
    vi.mocked(settingsGetRemoteProfiles).mockResolvedValue({ profiles: [] });
    vi.mocked(settingsSaveRemoteProfile).mockResolvedValue(remoteProfilesSnapshot());
    vi.mocked(settingsDeleteRemoteProfile).mockResolvedValue({ profiles: [] });
    vi.mocked(settingsValidateRemoteProfile).mockResolvedValue(remoteProfilesSnapshot(true));
    vi.mocked(settingsProbeLspServer).mockResolvedValue({
      available: true,
      resolvedPath: "C:\\tools\\custom-ts-lsp.cmd",
      message: null,
    });
    vi.mocked(settingsSaveLspServer).mockResolvedValue(lspSnapshot("custom-ts-lsp"));
    vi.mocked(settingsResetLspServer).mockResolvedValue(lspSnapshot());
    vi.mocked(settingsSaveProviderModels).mockImplementation(async (profileId, models) => ({
      ...agentSnapshot,
      codex_acp: {
        ...agentSnapshot.codex_acp,
        profiles: codexProfiles("byok").map((profile) =>
          profile.id === profileId ? { ...profile, models } : profile,
        ),
      },
      claude: {
        ...agentSnapshot.claude,
        profiles: claudeProfiles("byok").map((profile) =>
          profile.id === profileId ? { ...profile, models } : profile,
        ),
      },
    }));
    vi.mocked(settingsResetProviderModels).mockResolvedValue(agentSnapshot);
    vi.mocked(settingsSaveAgentProviderSecret).mockImplementation(async (family, profileId) => {
      if (family === "codex") {
        const selectedProfileId = profileId === "default" ? "default" : "byok";
        return {
          ...agentSnapshot,
          codex_acp: {
            ...agentSnapshot.codex_acp,
            provider: selectedProfileId,
            selected_profile_id: selectedProfileId,
            profiles: codexProfiles(selectedProfileId, { [profileId]: true }),
            deepseek_key_configured: profileId === "deepseek",
          },
        };
      }
      return {
        ...agentSnapshot,
        claude: {
          ...agentSnapshot.claude,
          selected_profile_id: profileId,
          profiles: claudeProfiles(profileId, { [profileId]: true }),
        },
      };
    });
    vi.mocked(settingsSelectAgentProviderProfile).mockImplementation(async (family, profileId) => {
      const selectedCodexProfile = family === "codex" ? profileId : agentSnapshot.settings.selected_codex_provider_profile_id;
      const selectedClaudeProfile = family === "claude" ? profileId : agentSnapshot.settings.selected_claude_provider_profile_id;
      return {
        ...agentSnapshot,
        settings: {
          ...agentSnapshot.settings,
          selected_codex_provider_profile_id: selectedCodexProfile,
          selected_claude_provider_profile_id: selectedClaudeProfile,
        },
        codex_acp: {
          ...agentSnapshot.codex_acp,
          provider: family === "codex" ? profileId : agentSnapshot.codex_acp.provider,
          selected_profile_id: family === "codex" ? profileId : agentSnapshot.codex_acp.selected_profile_id,
          profiles: family === "codex" ? codexProfiles(profileId) : agentSnapshot.codex_acp.profiles,
        },
        claude: {
          ...agentSnapshot.claude,
          selected_profile_id: family === "claude" ? profileId : agentSnapshot.claude.selected_profile_id,
          profiles: family === "claude" ? claudeProfiles(profileId) : agentSnapshot.claude.profiles,
        },
      };
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

  it("shows a startup warning and keeps the user on the requested settings tab", async () => {
    const onStartupNoticeDismissed = vi.fn();
    render(
      <SettingsPage
        initialAgentTab="codex-acp"
        startupNotice={{ kind: "codex_byok" }}
        onBack={vi.fn()}
        onStartupNoticeDismissed={onStartupNoticeDismissed}
      />,
    );

    const dialog = await screen.findByRole("alertdialog", { name: "模型来源还没设置好" });
    expect(within(dialog).getByText(/还没有可用于新建会话的 provider/)).toBeInTheDocument();

    fireEvent.click(within(dialog).getByRole("button", { name: "去设置" }));

    await waitFor(() => expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument());
    expect(onStartupNoticeDismissed).toHaveBeenCalled();
    expect(screen.getByRole("tab", { name: "Codex" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByText("Codex 通道")).toBeInTheDocument();
  });

  it("checks for updates and reports the current version as up to date", async () => {
    render(<SettingsPage onBack={vi.fn()} />);

    fireEvent.click(await screen.findByRole("button", { name: "检查更新" }));

    await waitFor(() => expect(checkForAppUpdate).toHaveBeenCalled());
    expect(await screen.findByText("当前已是最新版本")).toBeInTheDocument();
  });

  it("installs an available update from settings", async () => {
    vi.mocked(checkForAppUpdate).mockResolvedValueOnce({
      currentVersion: "0.1.0",
      version: "0.1.1",
      date: null,
      body: "Release notes",
    });
    vi.mocked(installPendingAppUpdate).mockImplementationOnce(async (onProgress) => {
      onProgress?.({ phase: "started", downloadedBytes: 0, contentLength: 2048 });
      onProgress?.({ phase: "progress", downloadedBytes: 1024, contentLength: 2048 });
      onProgress?.({ phase: "finished", downloadedBytes: 2048, contentLength: 2048 });
    });
    render(<SettingsPage onBack={vi.fn()} />);

    fireEvent.click(await screen.findByRole("button", { name: "检查更新" }));

    expect(await screen.findByText("发现新版本 0.1.1")).toBeInTheDocument();
    expect(screen.getByText("Release notes")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "安装并重启" }));

    await waitFor(() => expect(installPendingAppUpdate).toHaveBeenCalled());
    expect(await screen.findByText("更新已安装，正在重启")).toBeInTheDocument();
  });

  it("loads, probes, saves, disables, and resets a language server", async () => {
    render(<SettingsPage onBack={vi.fn()} />);

    fireEvent.click(await screen.findByRole("button", { name: "LSP" }));
    await screen.findByText("TypeScript");
    const commandInput = screen.getByLabelText("命令") as HTMLInputElement;
    fireEvent.change(commandInput, { target: { value: "custom-ts-lsp" } });
    fireEvent.click(screen.getByText("探测"));

    await waitFor(() => expect(settingsProbeLspServer).toHaveBeenCalledWith("custom-ts-lsp", null));
    await screen.findByText("已找到：C:\\tools\\custom-ts-lsp.cmd");

    fireEvent.click(screen.getByText("保存"));
    await waitFor(() =>
      expect(settingsSaveLspServer).toHaveBeenCalledWith({
        languageId: "typescript",
        enabled: true,
        command: "custom-ts-lsp",
        args: ["--stdio"],
      }, null),
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
      }, null),
    );

    fireEvent.click(screen.getByText("重置"));
    await waitFor(() => expect(settingsResetLspServer).toHaveBeenCalledWith("typescript", null));
  });

  it("creates, validates, and deletes a remote machine profile", async () => {
    render(<SettingsPage onBack={vi.fn()} />);

    fireEvent.click(await screen.findByRole("button", { name: "远程" }));
    expect(await screen.findByText("还没有远程机器")).toBeInTheDocument();

    fireEvent.click(screen.getAllByRole("button", { name: "添加远程机器" })[0]);
    expect(screen.queryByLabelText("remote_profile_agent")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("remote_profile_agent_command")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("remote_profile_auth_hint")).not.toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("remote_profile_name"), { target: { value: "Devbox" } });
    fireEvent.change(screen.getByLabelText("remote_profile_ssh_target"), { target: { value: "root@devbox" } });
    fireEvent.change(screen.getByLabelText("remote_profile_ssh_port"), { target: { value: "36000" } });
    fireEvent.click(screen.getByRole("button", { name: "保存远程机器" }));

    await waitFor(() =>
      expect(settingsSaveRemoteProfile).toHaveBeenCalledWith({
        id: null,
        display_name: "Devbox",
        ssh_target: "root@devbox",
        ssh_port: 36000,
      }),
    );
    expect(await screen.findByText("Devbox")).toBeInTheDocument();
    expect(screen.getByLabelText("remote_validate_path_remote-1")).toHaveAttribute("placeholder", "~");

    fireEvent.change(screen.getByLabelText("remote_validate_path_remote-1"), { target: { value: "/srv/project" } });
    fireEvent.click(screen.getByRole("button", { name: "验证" }));
    await waitFor(() =>
      expect(settingsValidateRemoteProfile).toHaveBeenCalledWith({
        profile_id: "remote-1",
        remote_path: "/srv/project",
        include_acp: false,
      }),
    );
    expect(await screen.findByText("SSH · 通过")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "删除" }));
    await waitFor(() => expect(settingsDeleteRemoteProfile).toHaveBeenCalledWith("remote-1"));
  });

  it("passes a one-time SSH password for remote profile validation", async () => {
    vi.mocked(settingsGetRemoteProfiles).mockResolvedValue(remoteProfilesSnapshot());
    render(<SettingsPage onBack={vi.fn()} />);

    fireEvent.click(await screen.findByRole("button", { name: "远程" }));
    const passwordInput = await screen.findByLabelText("remote_validate_password_remote-1");
    fireEvent.change(passwordInput, { target: { value: "ssh-secret" } });
    fireEvent.click(screen.getByRole("button", { name: "验证" }));

    await waitFor(() =>
      expect(settingsValidateRemoteProfile).toHaveBeenCalledWith({
        profile_id: "remote-1",
        remote_path: "~",
        include_acp: false,
        ssh_password: "ssh-secret",
      }),
    );
    await waitFor(() => expect(passwordInput).toHaveValue(""));
  });

  it("opens remote settings with the active remote context", async () => {
    vi.mocked(settingsGetRemoteProfiles).mockResolvedValue(remoteProfilesSnapshot(true));

    render(
      <SettingsPage
        initialPane="remote"
        remoteContext={{
          profileId: "remote-1",
          workspaceName: "project",
          sshTarget: "root@devbox",
          sshPort: 36000,
          remotePath: "/srv/project",
          agentLabel: "CodeBuddy",
        }}
        onBack={vi.fn()}
      />,
    );

    expect(await screen.findByText("当前远程上下文")).toBeInTheDocument();
    expect(settingsGetAgentSnapshot).toHaveBeenCalledWith("remote-1");
    expect(settingsGetLspSnapshot).toHaveBeenCalledWith("remote-1");
    expect(screen.getByText("project · root@devbox:36000")).toBeInTheDocument();
    expect(screen.getByText("/srv/project")).toBeInTheDocument();
    expect(screen.getAllByText("CodeBuddy").length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "Claude 通道" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Codex 通道" })).toBeInTheDocument();
  });

  it("writes runtime settings to the active remote profile", async () => {
    render(
      <SettingsPage
        remoteContext={{
          profileId: "remote-1",
          workspaceName: "project",
          sshTarget: "root@devbox",
          sshPort: 36000,
          remotePath: "/srv/project",
          agentLabel: "Claude",
        }}
        onBack={vi.fn()}
      />,
    );

    await openAgentSettingsTab("Codex");
    await selectByokProvider(/TimiAI/);
    fireEvent.change(screen.getByLabelText("byok_api_key"), { target: { value: "remote-timiai-secret" } });
    fireEvent.click(screen.getByRole("button", { name: "保存 TimiAI key" }));

    await waitFor(() =>
      expect(settingsSaveAgentProviderSecret).toHaveBeenCalledWith(
        "codex",
        "timiai",
        "remote-timiai-secret",
        "remote-1",
      ),
    );
    await waitFor(() =>
      expect(settingsSaveAgentProviderSecret).toHaveBeenCalledWith(
        "claude",
        "timiai",
        "remote-timiai-secret",
        "remote-1",
      ),
    );
  });

  it("edits a remote profile and warns about duplicate SSH targets", async () => {
    vi.mocked(settingsGetRemoteProfiles).mockResolvedValue(remoteProfilesWithDuplicateTarget());
    vi.mocked(settingsSaveRemoteProfile).mockResolvedValue({
      profiles: [
        {
          ...remoteProfilesSnapshot().profiles[0],
          display_name: "Devbox Renamed",
          ssh_target: "root@staging",
          ssh_port: 22,
        },
      ],
    });

    render(<SettingsPage onBack={vi.fn()} />);

    fireEvent.click(await screen.findByRole("button", { name: "远程" }));
    await screen.findByText("Devbox");
    fireEvent.click(screen.getAllByRole("button", { name: "编辑" })[0]);
    fireEvent.change(screen.getByLabelText("remote_profile_name"), { target: { value: "Devbox Renamed" } });
    fireEvent.change(screen.getByLabelText("remote_profile_ssh_target"), { target: { value: "root@staging" } });
    fireEvent.change(screen.getByLabelText("remote_profile_ssh_port"), { target: { value: "22" } });

    expect(await screen.findByText("已有同一 SSH 目标：Staging")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "保存远程机器" }));

    await waitFor(() =>
      expect(settingsSaveRemoteProfile).toHaveBeenCalledWith({
        id: "remote-1",
        display_name: "Devbox Renamed",
        ssh_target: "root@staging",
        ssh_port: 22,
      }),
    );
    expect(await screen.findByText("Devbox Renamed")).toBeInTheDocument();
  });

  it("renders remote validation failures with phase diagnostics", async () => {
    vi.mocked(settingsGetRemoteProfiles).mockResolvedValue(remoteProfilesSnapshot());
    vi.mocked(settingsValidateRemoteProfile).mockResolvedValue(remoteProfilesValidationFailed());

    render(<SettingsPage onBack={vi.fn()} />);

    fireEvent.click(await screen.findByRole("button", { name: "远程" }));
    fireEvent.change(await screen.findByLabelText("remote_validate_path_remote-1"), {
      target: { value: "/srv/missing" },
    });
    fireEvent.click(screen.getByRole("button", { name: "验证" }));

    await waitFor(() =>
      expect(settingsValidateRemoteProfile).toHaveBeenCalledWith({
        profile_id: "remote-1",
        remote_path: "/srv/missing",
        include_acp: false,
      }),
    );
    expect(await screen.findByText("远程机器验证失败")).toBeInTheDocument();
    expect(screen.getByText("目录 · 失败")).toHaveAttribute("title", "No such directory");
  });

  it("recovers from remote settings load and save errors", async () => {
    vi.mocked(settingsGetRemoteProfiles)
      .mockRejectedValueOnce(new Error("remote load failed"))
      .mockResolvedValueOnce({ profiles: [] });
    vi.mocked(settingsSaveRemoteProfile)
      .mockRejectedValueOnce(new Error("remote save failed"))
      .mockResolvedValueOnce(remoteProfilesSnapshot());

    render(<SettingsPage onBack={vi.fn()} />);

    expect(await screen.findByText("Error: remote load failed")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    fireEvent.click(await screen.findByRole("button", { name: "远程" }));
    expect(await screen.findByText("还没有远程机器")).toBeInTheDocument();

    fireEvent.click(screen.getAllByRole("button", { name: "添加远程机器" })[0]);
    fireEvent.change(screen.getByLabelText("remote_profile_name"), { target: { value: "Devbox" } });
    fireEvent.change(screen.getByLabelText("remote_profile_ssh_target"), { target: { value: "root@devbox" } });
    fireEvent.click(screen.getByRole("button", { name: "保存远程机器" }));
    expect(await screen.findByText("Error: remote save failed")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "保存远程机器" }));
    expect(await screen.findByText("Devbox")).toBeInTheDocument();
  });

  it("renders codex-acp configuration and saves a TimiAI BYOK key without echoing it", async () => {
    render(<SettingsPage onBack={vi.fn()} />);

    await openAgentSettingsTab("Codex");
    expect(screen.queryByText("goose")).not.toBeInTheDocument();
    expect(screen.getByLabelText("byok_provider_profile")).toBeInTheDocument();
    expect(screen.getAllByText("未配置").length).toBeGreaterThan(0);
    expect(screen.queryByText("C:\\Users\\yvonchen\\.kodex\\config.toml")).not.toBeInTheDocument();

    await selectByokProvider(/TimiAI/);
    const saveButton = screen.getByRole("button", { name: "保存 TimiAI key" });
    expect(saveButton).toBeDisabled();

    fireEvent.change(screen.getByLabelText("byok_api_key"), { target: { value: "timiai-secret" } });
    expect(saveButton).not.toBeDisabled();
    fireEvent.click(saveButton);

    await waitFor(() => expect(settingsSaveAgentProviderSecret).toHaveBeenCalledWith("codex", "timiai", "timiai-secret", null));
    expect(settingsSaveAgentProviderSecret).toHaveBeenCalledWith("claude", "timiai", "timiai-secret", null);
    await screen.findByText("TimiAI API key 已更新，后续新建会话生效");
    expect(screen.getByLabelText("byok_api_key")).toHaveValue("");
    expect(screen.queryByDisplayValue("timiai-secret")).not.toBeInTheDocument();
  });

  it("switches Codex between default and BYOK channels", async () => {
    render(<SettingsPage onBack={vi.fn()} />);

    await openAgentSettingsTab("Codex");
    const codexChannel = screen.getByRole("radiogroup", { name: "Codex channel" });
    fireEvent.click(within(codexChannel).getByRole("button", { name: /默认/ }));
    await waitFor(() => expect(settingsSelectAgentProviderProfile).toHaveBeenCalledWith("codex", "default", null));
    await screen.findByText("Codex 通道已切换到 默认");
  });

  it("saves DeepSeek provider key without echoing it", async () => {
    render(<SettingsPage onBack={vi.fn()} />);

    await openAgentSettingsTab("Codex");
    await selectByokProvider(/DeepSeek/);

    const saveButton = screen.getByRole("button", { name: "保存 DeepSeek key" });
    fireEvent.change(screen.getByLabelText("byok_api_key"), { target: { value: "deepseek-secret" } });
    fireEvent.click(saveButton);

    await waitFor(() =>
      expect(settingsSaveAgentProviderSecret).toHaveBeenCalledWith("codex", "deepseek", "deepseek-secret", null),
    );
    expect(settingsSaveAgentProviderSecret).toHaveBeenCalledWith("claude", "deepseek", "deepseek-secret", null);
    await screen.findByText("DeepSeek API key 已更新，后续新建会话生效");
    expect(screen.getByLabelText("byok_api_key")).toHaveValue("");
    expect(screen.queryByDisplayValue("deepseek-secret")).not.toBeInTheDocument();
  });

  it("adds a Kimi Code key to the shared BYOK model pool without echoing it", async () => {
    render(<SettingsPage onBack={vi.fn()} />);

    await openAgentSettingsTab("Codex");
    await screen.findByText("BYOK 模型池");
    await selectByokProvider(/Kimi Code/);

    fireEvent.change(screen.getByLabelText("byok_api_key"), { target: { value: "kimi-secret" } });
    fireEvent.click(screen.getByRole("button", { name: "保存 Kimi Code key" }));

    await waitFor(() => expect(settingsSaveAgentProviderSecret).toHaveBeenCalledWith("codex", "kimi_code", "kimi-secret", null));
    await waitFor(() => expect(settingsSaveAgentProviderSecret).toHaveBeenCalledWith("claude", "kimi_code", "kimi-secret", null));
    await screen.findByText("Kimi Code API key 已更新，后续新建会话生效");
    expect(screen.getByLabelText("byok_api_key")).toHaveValue("");
    expect(screen.queryByDisplayValue("kimi-secret")).not.toBeInTheDocument();
  });

  it("lets BYOK source selection diverge from the current Codex channel", async () => {
    vi.mocked(settingsGetAgentSnapshot).mockResolvedValue({
      ...agentSnapshot,
      codex_acp: {
        ...agentSnapshot.codex_acp,
        provider: "byok",
        selected_profile_id: "byok",
        profiles: codexProfiles("deepseek", { deepseek: true }),
      },
    });
    render(<SettingsPage onBack={vi.fn()} />);

    await openAgentSettingsTab("Codex");
    const sourceSelect = screen.getByLabelText("byok_provider_profile");
    expect(sourceSelect).toHaveTextContent("DeepSeek · 已配置");

    await selectByokProvider(/Xiaomi Token Plan/);
    expect(screen.getByLabelText("byok_provider_profile")).toHaveTextContent("Xiaomi Token Plan · 未配置");
    const xiaomiModels = "模型：MiMo-V2.5-Pro、MiMo-V2.5";
    expect(screen.getByLabelText(xiaomiModels)).toHaveAttribute("title", xiaomiModels);
    expect(screen.queryByText("https://token-plan-cn.xiaomimimo.com/v1")).not.toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("byok_api_key"), { target: { value: "mimo-secret" } });
    fireEvent.click(screen.getByRole("button", { name: "保存 Xiaomi Token Plan key" }));

    await waitFor(() =>
      expect(settingsSaveAgentProviderSecret).toHaveBeenCalledWith("codex", "xiaomi_mimo", "mimo-secret", null),
    );
    expect(settingsSaveAgentProviderSecret).toHaveBeenCalledWith("claude", "xiaomi_mimo", "mimo-secret", null);
    await screen.findByText("Xiaomi Token Plan API key 已更新，后续新建会话生效");
    expect(screen.getByLabelText("byok_provider_profile")).toHaveTextContent("Xiaomi Token Plan");
  });

  it("saves and resets editable BYOK provider models", async () => {
    render(<SettingsPage onBack={vi.fn()} />);

    await openAgentSettingsTab("Codex");
    await selectByokProvider(/TimiAI/);

    const modelsInput = screen.getByLabelText("byok_provider_models");
    expect(modelsInput).toHaveValue("gpt-5.5\ngpt-5.4\nclaude-opus-4.8");
    fireEvent.change(modelsInput, {
      target: { value: "gpt-5.6\n\nclaude-opus-4.9\ngpt-5.6" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存模型列表" }));

    await waitFor(() =>
      expect(settingsSaveProviderModels).toHaveBeenCalledWith(
        "timiai",
        ["gpt-5.6", "claude-opus-4.9"],
        null,
      ),
    );
    await screen.findByText("TimiAI 模型列表已更新，后续新建会话生效");
    expect(screen.getByLabelText("byok_provider_models")).toHaveValue("gpt-5.6\nclaude-opus-4.9");

    fireEvent.click(screen.getByRole("button", { name: "恢复默认" }));
    await waitFor(() => expect(settingsResetProviderModels).toHaveBeenCalledWith("timiai", null));
    await screen.findByText("TimiAI 模型列表已恢复默认，后续新建会话生效");
  });

  it("shows configured BYOK providers as a single shared model pool", async () => {
    vi.mocked(settingsGetAgentSnapshot).mockResolvedValue({
      ...agentSnapshot,
      codex_acp: {
        ...agentSnapshot.codex_acp,
        provider: "byok",
        selected_profile_id: "byok",
        profiles: codexProfiles("byok", { timiai: true, deepseek: true }),
        deepseek_key_configured: true,
      },
    });
    render(<SettingsPage onBack={vi.fn()} />);

    await openAgentSettingsTab("Codex");
    await screen.findByText("2/5 已配置");
    fireEvent.click(screen.getByLabelText("byok_provider_profile"));
    expect(screen.getByRole("option", { name: "TimiAI · 已配置" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "CommandCode · 未配置" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "DeepSeek · 已配置" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Kimi Code · 未配置" })).toBeInTheDocument();
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
    expect(screen.queryByText("Venus")).not.toBeInTheDocument();
    expect(screen.getAllByText(/BYOK/).length).toBeGreaterThan(0);
    expect(screen.queryByLabelText("codex_provider_profile")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("claude_provider_profile")).not.toBeInTheDocument();
  });

  it("does not render Venus as a provider option", async () => {
    render(<SettingsPage onBack={vi.fn()} />);

    await openAgentSettingsTab("Claude");
    await screen.findByText("Claude 通道");
    expect(screen.queryByText("Venus")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("claude_venus_api_key")).not.toBeInTheDocument();
  });

  it("saves the shared TimiAI key from the BYOK pool on the Claude tab", async () => {
    render(<SettingsPage onBack={vi.fn()} />);

    await openAgentSettingsTab("Claude");
    await selectByokProvider(/TimiAI/);
    const timiaiModels = "模型：gpt-5.5、gpt-5.4、claude-opus-4.8";
    expect(screen.queryByText(/llmproxy\/v1\/messages/)).not.toBeInTheDocument();
    expect(screen.getByLabelText(timiaiModels)).toHaveAttribute("title", timiaiModels);

    fireEvent.change(screen.getByLabelText("byok_api_key"), { target: { value: "timiai-secret" } });
    fireEvent.click(screen.getByRole("button", { name: "保存 TimiAI key" }));

    await waitFor(() =>
      expect(settingsSaveAgentProviderSecret).toHaveBeenCalledWith("codex", "timiai", "timiai-secret", null),
    );
    expect(settingsSaveAgentProviderSecret).toHaveBeenCalledWith("claude", "timiai", "timiai-secret", null);
    await waitFor(() =>
      expect(settingsSaveAgentProviderSecret).toHaveBeenCalledWith("claude", "timiai", "timiai-secret", null),
    );
    await screen.findByText("TimiAI API key 已更新，后续新建会话生效");
    expect(screen.getByLabelText("byok_api_key")).toHaveValue("");
  });

});
