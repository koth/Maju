import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { cleanup, render, screen, fireEvent, waitFor } from "@testing-library/react";
import {
  BrowserSettingsPane,
  browserModeFromValue,
  preflightBlocksEnable,
  wantsInstallAction,
} from "./BrowserSettingsPane";
import {
  browserInstall,
  browserPreflight,
  browserRefreshPreflight,
  settingsSaveBrowserSettings,
} from "../../lib/tauri";
import { onBrowserInstallProgress } from "../../lib/events";
import type { BrowserPreflight, BrowserSettings } from "../../types";

vi.mock("../../lib/events", () => ({
  onBrowserInstallProgress: vi.fn(),
}));

vi.mock("../../lib/tauri", () => ({
  browserInstall: vi.fn(),
  browserPreflight: vi.fn(),
  browserRefreshPreflight: vi.fn(),
  settingsSaveBrowserSettings: vi.fn(),
}));

function settings(overrides: Partial<BrowserSettings> = {}): BrowserSettings {
  return {
    enabled: false,
    mode: "launch",
    headless: true,
    executable_path: "",
    endpoint: "",
    allow_attach: false,
    profile_name: "default",
    provider_version: "0.1.6-alpha.1",
    tool_call_timeout_ms: 30000,
    ...overrides,
  };
}

const ready: BrowserPreflight = {
  state: { state: "ready" },
  node_executable: "/usr/bin/node",
  provider_version: "0.1.6-alpha.1",
};

const missing: BrowserPreflight = {
  state: {
    state: "missing",
    detail: "No Playwright Chromium install was found.",
    remedy: "Run `npx playwright install chromium` once, then re-run preflight.",
    fix: "install",
  },
  provider_version: "0.1.6-alpha.1",
};

describe("preflightBlocksEnable", () => {
  it("does not block when preflight has not run", () => {
    expect(preflightBlocksEnable(null, settings())).toBeNull();
  });

  it("does not block a ready launch configuration", () => {
    expect(preflightBlocksEnable(ready, settings())).toBeNull();
  });

  it("blocks with the remedy when a dependency is missing", () => {
    expect(preflightBlocksEnable(missing, settings())).toContain(
      "npx playwright install",
    );
  });

  it("does not block attach mode, which needs no local install", () => {
    // Attach talks to a browser the user already runs; a missing local
    // Chromium is irrelevant there.
    expect(
      preflightBlocksEnable(missing, settings({ mode: "attach" })),
    ).toBeNull();
  });
});

describe("browserModeFromValue", () => {
  it("keeps every mode the picker offers", () => {
    expect(browserModeFromValue("launch")).toBe("launch");
    expect(browserModeFromValue("attach")).toBe("attach");
    expect(browserModeFromValue("persistent")).toBe("persistent");
  });

  it("falls back to launch for an unknown value", () => {
    expect(browserModeFromValue("")).toBe("launch");
    expect(browserModeFromValue("nonsense")).toBe("launch");
  });
});

describe("BrowserSettingsPane", () => {
  beforeEach(() => {
    vi.mocked(browserPreflight).mockResolvedValue(ready);
    vi.mocked(onBrowserInstallProgress).mockResolvedValue(() => {});
    vi.mocked(browserRefreshPreflight).mockResolvedValue(ready);
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("shows the preflight result from the snapshot without a re-check", () => {
    render(
      <BrowserSettingsPane
        onSaved={vi.fn()}
        preflight={ready}
        settings={settings()}
      />,
    );
    expect(screen.getByText(/\/usr\/bin\/node/)).toBeTruthy();
  });

  it("shows the remedy when a dependency is missing", () => {
    render(
      <BrowserSettingsPane
        onSaved={vi.fn()}
        preflight={missing}
        settings={settings()}
      />,
    );
    expect(screen.getByText(/npx playwright install/)).toBeTruthy();
    expect(
      screen.getByText(/解决之前浏览器工具无法启动/),
    ).toBeTruthy();
  });

  it("re-runs preflight on demand", async () => {
    vi.mocked(browserPreflight).mockResolvedValueOnce(ready);
    render(
      <BrowserSettingsPane
        onSaved={vi.fn()}
        preflight={missing}
        settings={settings()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "重新检查" }));

    await waitFor(() => expect(browserPreflight).toHaveBeenCalled());
    await waitFor(() => {
      expect(screen.queryByText(/npx playwright install/)).toBeNull();
    });
  });

  it("keeps 跨会话保留登录状态 selected and offers the profile field", () => {
    // The picker used to map every value that was not "attach" back to
    // "launch", so this option could be clicked but never stayed selected.
    render(
      <BrowserSettingsPane
        onSaved={vi.fn()}
        preflight={ready}
        settings={settings()}
      />,
    );

    fireEvent.change(screen.getByRole("combobox"), {
      target: { value: "persistent" },
    });

    expect(screen.getByRole("combobox")).toHaveProperty("value", "persistent");
    expect(screen.getByText(/配置档名/)).toBeTruthy();
    expect(screen.queryByLabelText(/我理解这会让 agent 操作我已登录的浏览器/)).toBeNull();
  });

  it("switches to 接管我已经在运行的浏览器 and asks for consent", () => {
    render(
      <BrowserSettingsPane
        onSaved={vi.fn()}
        preflight={ready}
        settings={settings()}
      />,
    );

    fireEvent.change(screen.getByRole("combobox"), {
      target: { value: "attach" },
    });

    expect(screen.getByRole("combobox")).toHaveProperty("value", "attach");
    expect(
      screen.getByLabelText(/我理解这会让 agent 操作我已登录的浏览器/),
    ).toBeTruthy();
  });

  it("saves the edited settings and reports the snapshot back", async () => {
    // The command re-runs preflight server-side, so the pane hands the
    // returned snapshot upward rather than the draft it just built: the draft
    // cannot know whether preflight now passes.
    const snapshot = { settings: { browser: settings({ enabled: true }) } };
    vi.mocked(settingsSaveBrowserSettings).mockResolvedValue(snapshot as never);
    const onSaved = vi.fn();

    render(
      <BrowserSettingsPane
        onSaved={onSaved}
        preflight={ready}
        settings={settings()}
      />,
    );

    fireEvent.click(screen.getByLabelText("启用浏览器工具"));
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(settingsSaveBrowserSettings).toHaveBeenCalled());
    const sent = vi.mocked(settingsSaveBrowserSettings).mock.calls[0][0];
    expect(sent.enabled).toBe(true);
    await waitFor(() => expect(onSaved).toHaveBeenCalledWith(snapshot));
  });

  it("surfaces a validation failure from the backend", async () => {
    vi.mocked(settingsSaveBrowserSettings).mockRejectedValueOnce(
      new Error("invalid browser settings: attach mode requires a browser endpoint"),
    );
    render(
      <BrowserSettingsPane
        onSaved={vi.fn()}
        preflight={ready}
        settings={settings()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => {
      expect(screen.getByText(/attach mode requires a browser endpoint/)).toBeTruthy();
    });
  });

  it("offers a profile name only in persistent mode", () => {
    const { rerender } = render(
      <BrowserSettingsPane
        onSaved={vi.fn()}
        preflight={ready}
        settings={settings()}
      />,
    );
    expect(screen.queryByLabelText("Profile")).toBeNull();

    rerender(
      <BrowserSettingsPane
        onSaved={vi.fn()}
        preflight={ready}
        settings={settings({ mode: "persistent" })}
      />,
    );
    expect(screen.getByLabelText("配置档名")).toBeTruthy();
  });

  it("still gates persistent mode on the local browser being present", () => {
    // Persistent mode uses Kodex's own Chromium, so a missing local browser
    // blocks it the same way it blocks launch. Only attach is exempt.
    expect(preflightBlocksEnable(missing, settings({ mode: "persistent" }))).toContain(
      "npx playwright install",
    );
    expect(
      preflightBlocksEnable(missing, settings({ mode: "attach" })),
    ).toBeNull();
  });

  it("shows the attach opt-in only in attach mode", () => {
    const { rerender } = render(
      <BrowserSettingsPane
        onSaved={vi.fn()}
        preflight={ready}
        settings={settings()}
      />,
    );
    expect(screen.queryByLabelText(/logged-in browser/i)).toBeNull();

    rerender(
      <BrowserSettingsPane
        onSaved={vi.fn()}
        preflight={ready}
        settings={settings({ mode: "attach" })}
      />,
    );
    expect(screen.getByLabelText(/已登录的浏览器/)).toBeTruthy();
  });

  it("keeps the endpoint field for attach mode", () => {
    render(
      <BrowserSettingsPane
        onSaved={vi.fn()}
        preflight={ready}
        settings={settings({ mode: "attach" })}
      />,
    );
    expect(screen.getByPlaceholderText("http://127.0.0.1:9222")).toBeTruthy();
  });

  it("explains how to expose the debug endpoint in attach mode", () => {
    // The endpoint field cannot create the endpoint: a browser only listens on
    // one when launched with `--remote-debugging-port`, and nothing else in the
    // pane says so. Without this the field asks for an address the user has no
    // way to know they have to bring into existence first.
    render(
      <BrowserSettingsPane
        onSaved={vi.fn()}
        preflight={ready}
        settings={settings({ mode: "attach" })}
      />,
    );
    expect(
      screen.getByText(/google-chrome --remote-debugging-port/),
    ).toBeTruthy();
    // The trap worth spelling out: a flag on an already-running browser is
    // silently ignored, so attaching to a logged-in session needs a restart.
    expect(screen.getByText(/完全退出/)).toBeTruthy();
  });

  it("does not show the endpoint guide outside attach mode", () => {
    render(
      <BrowserSettingsPane
        onSaved={vi.fn()}
        preflight={ready}
        settings={settings()}
      />,
    );
    expect(
      screen.queryByText(/google-chrome --remote-debugging-port/),
    ).toBeNull();
  });

  it("offers the headless toggle only in launch mode", () => {
    const { rerender } = render(
      <BrowserSettingsPane
        onSaved={vi.fn()}
        preflight={ready}
        settings={settings()}
      />,
    );
    expect(screen.getByLabelText(/不显示浏览器窗口/)).toBeTruthy();

    rerender(
      <BrowserSettingsPane
        onSaved={vi.fn()}
        preflight={ready}
        settings={settings({ mode: "attach" })}
      />,
    );
    expect(screen.queryByLabelText(/without a visible window/i)).toBeNull();
  });

  it("offers an install only for a dependency the installer can supply", () => {
    // A missing Node or a bad configuration has a different remedy, and an
    // "Install" button for either sends the user down a path that cannot work.
    expect(wantsInstallAction(missing, settings())).toBe(true);
    expect(
      wantsInstallAction(
        { state: { state: "missing", detail: "no node", remedy: "install node", fix: "configure" }, provider_version: "1" },
        settings(),
      ),
    ).toBe(false);
    expect(wantsInstallAction(ready, settings())).toBe(false);
    expect(wantsInstallAction(null, settings())).toBe(false);
  });

  it("does not offer an install in attach mode", () => {
    // Attaching needs no local provider: the browser is the user's own.
    expect(wantsInstallAction(missing, settings({ mode: "attach" }))).toBe(false);
  });

  it("installs on request and re-checks afterwards", async () => {
    vi.mocked(browserInstall).mockResolvedValue({
      phase: { phase: "complete", verified: true },
      label: "浏览器 provider 已就绪。",
      running: false,
      verified: true,
      installed: true,
    });
    vi.mocked(browserRefreshPreflight).mockResolvedValue(ready);

    render(
      <BrowserSettingsPane onSaved={vi.fn()} preflight={missing} settings={settings()} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "帮我安装" }));

    await waitFor(() => expect(browserInstall).toHaveBeenCalled());
    // Without the re-check the pane would keep warning about a missing
    // provider that is now installed.
    await waitFor(() => expect(browserRefreshPreflight).toHaveBeenCalled());
    await waitFor(() => {
      expect(screen.getByText("浏览器 provider 已就绪。")).toBeTruthy();
    });
    expect(screen.queryByRole("button", { name: "Install for me" })).toBeNull();
  });

  it("surfaces an install failure with its step", async () => {
    vi.mocked(browserInstall).mockRejectedValueOnce(
      new Error("the installer did not finish in time"),
    );

    render(
      <BrowserSettingsPane onSaved={vi.fn()} preflight={missing} settings={settings()} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "帮我安装" }));

    await waitFor(() => {
      expect(screen.getByText("安装失败。")).toBeTruthy();
    });
  });

  /// The label only names the step. A user staring at 「安装浏览器 provider
  /// 失败。」 cannot act on it, and the installer's own reason — npm's error
  /// text, which tool could not start — has nowhere else to go.
  it("shows the installer's own reason for a failed phase", async () => {
    let emit: ((state: unknown) => void) | undefined;
    vi.mocked(onBrowserInstallProgress).mockImplementation((callback) => {
      emit = callback as (state: unknown) => void;
      return Promise.resolve(() => {});
    });

    render(
      <BrowserSettingsPane onSaved={vi.fn()} preflight={missing} settings={settings()} />,
    );
    await waitFor(() => expect(onBrowserInstallProgress).toHaveBeenCalled());

    emit?.({
      phase: {
        phase: "failed",
        step: "安装浏览器 provider",
        detail: "无法启动安装程序：npm: No such file or directory (os error 2)",
      },
      label: "安装浏览器 provider 失败。",
      running: false,
      verified: false,
      installed: false,
    });

    await waitFor(() => {
      expect(screen.getByText(/npm: No such file or directory/)).toBeTruthy();
    });
  });

  it("does not show a detail line for a phase that is not a failure", () => {
    let emit: ((state: unknown) => void) | undefined;
    vi.mocked(onBrowserInstallProgress).mockImplementation((callback) => {
      emit = callback as (state: unknown) => void;
      return Promise.resolve(() => {});
    });

    render(
      <BrowserSettingsPane onSaved={vi.fn()} preflight={missing} settings={settings()} />,
    );
    // The label alone is enough while work is in flight; a stray detail box
    // next to "正在安装…" would be noise.
    emit?.({
      phase: { phase: "installingChromium" },
      label: "正在安装 Chromium…",
      running: true,
      verified: false,
      installed: false,
    });

    expect(
      document.querySelector(".browser-settings__install-detail"),
    ).toBeNull();
  });

  it("reflects pushed progress while an install runs", async () => {
    let emit: ((state: unknown) => void) | undefined;
    vi.mocked(onBrowserInstallProgress).mockImplementation((callback) => {
      emit = callback as (state: unknown) => void;
      return Promise.resolve(() => {});
    });

    render(
      <BrowserSettingsPane onSaved={vi.fn()} preflight={missing} settings={settings()} />,
    );
    await waitFor(() => expect(onBrowserInstallProgress).toHaveBeenCalled());

    emit?.({
      phase: { phase: "installingChromium" },
      label: "正在安装 Chromium…",
      running: true,
      verified: false,
      installed: false,
    });

    await waitFor(() => {
      expect(screen.getByText("正在安装 Chromium…")).toBeTruthy();
    });
    expect(
      (screen.getByRole("button", { name: "安装中…" }) as HTMLButtonElement).disabled,
    ).toBe(true);
  });

  it("falls back to a readable message when re-check cannot run", async () => {
    vi.mocked(browserPreflight).mockRejectedValueOnce(new Error("backend down"));
    render(
      <BrowserSettingsPane
        onSaved={vi.fn()}
        preflight={missing}
        settings={settings()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "重新检查" }));

    await waitFor(() => {
      expect(screen.getByText(/无法运行浏览器预检/)).toBeTruthy();
    });
  });
});
