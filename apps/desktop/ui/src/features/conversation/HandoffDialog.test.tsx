import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  sessionHandoffSummary,
  settingsGetAgentSnapshot,
  settingsListDshPresets,
} from "../../lib/tauri";
import { HandoffDialog } from "./HandoffDialog";

vi.mock("../../lib/tauri", () => ({
  sessionHandoffSummary: vi.fn(),
  settingsGetAgentSnapshot: vi.fn(),
  settingsListDshPresets: vi.fn(),
}));

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

beforeEach(() => {
  vi.mocked(settingsGetAgentSnapshot).mockResolvedValue({
    agents: [
      { id: "codex-acp", label: "Codex", binary: "codex-acp", installed: true },
      { id: "deepseek-harness", label: "DeepSeek Harness", binary: "dsh", installed: true },
    ],
    settings: { selected_agent: "codex-acp" },
  } as unknown as Awaited<ReturnType<typeof settingsGetAgentSnapshot>>);
  vi.mocked(settingsListDshPresets).mockResolvedValue([
    { id: "standard", label: "标准" },
  ] as Awaited<ReturnType<typeof settingsListDshPresets>>);
  vi.mocked(sessionHandoffSummary).mockReset();
});

const DIGEST = "# 交接说明（来自上一个会话）\n\n## 目标 / 用户请求\n- 把会话列表项目行改成呼吸灯\n";

describe("HandoffDialog", () => {
  it("shows the digest in an editable field", async () => {
    render(<HandoffDialog digest={DIGEST} onStartNewSession={vi.fn()} onClose={vi.fn()} />);
    // Let the agent picker finish its async load before asserting.
    await screen.findByRole("radio", { name: /Codex/ });

    const textarea = screen.getByLabelText("交接说明") as HTMLTextAreaElement;
    expect(textarea.value).toBe(DIGEST);

    fireEvent.change(textarea, { target: { value: "改过的交接内容" } });
    expect(textarea.value).toBe("改过的交接内容");
  });

  it("copies the edited text to the clipboard", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });

    render(<HandoffDialog digest={DIGEST} onStartNewSession={vi.fn()} onClose={vi.fn()} />);

    fireEvent.change(screen.getByLabelText("交接说明"), { target: { value: "改过的交接内容" } });
    fireEvent.click(screen.getByRole("button", { name: "复制" }));

    await waitFor(() => expect(writeText).toHaveBeenCalledWith("改过的交接内容"));
    expect(await screen.findByRole("button", { name: "已复制" })).toBeTruthy();
  });

  it("hands the edited text and the chosen agent to the new session", async () => {
    const onStartNewSession = vi.fn().mockResolvedValue(undefined);
    const onClose = vi.fn();

    render(
      <HandoffDialog digest={DIGEST} onStartNewSession={onStartNewSession} onClose={onClose} />,
    );

    // The picker defaults to the settings default agent.
    await waitFor(() =>
      expect(screen.getByRole("radio", { name: /Codex/ })).toBeChecked(),
    );

    fireEvent.click(screen.getByRole("radio", { name: /DeepSeek Harness/ }));
    const preset = await screen.findByLabelText("Agent 预设");
    fireEvent.change(preset, { target: { value: "standard" } });
    fireEvent.change(screen.getByLabelText("交接说明"), { target: { value: "交接内容 v2" } });
    fireEvent.click(screen.getByRole("button", { name: "新建会话并带入" }));

    await waitFor(() =>
      expect(onStartNewSession).toHaveBeenCalledWith("交接内容 v2", "deepseek-harness", "standard"),
    );
    await waitFor(() => expect(onClose).toHaveBeenCalled());
  });

  it("replaces the text with the model's rewrite", async () => {
    vi.mocked(sessionHandoffSummary).mockResolvedValue("# 交接说明\n\n## 下一步建议\n- 跑测试");

    render(<HandoffDialog digest={DIGEST} onStartNewSession={vi.fn()} onClose={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "用模型润色" }));

    await waitFor(() => expect(sessionHandoffSummary).toHaveBeenCalledWith(DIGEST));
    await waitFor(() =>
      expect((screen.getByLabelText("交接说明") as HTMLTextAreaElement).value).toContain(
        "## 下一步建议",
      ),
    );
  });

  it("keeps the local digest and reports why when polishing fails", async () => {
    vi.mocked(sessionHandoffSummary).mockRejectedValue(new Error("未配置交接摘要模型"));

    render(<HandoffDialog digest={DIGEST} onStartNewSession={vi.fn()} onClose={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "用模型润色" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "模型润色失败，仍使用本地摘要：未配置交接摘要模型",
    );
    expect((screen.getByLabelText("交接说明") as HTMLTextAreaElement).value).toBe(DIGEST);
  });

  it("keeps the dialog open and reports why when the new session fails", async () => {
    const onStartNewSession = vi.fn().mockRejectedValue(new Error("工作区未打开"));
    const onClose = vi.fn();

    render(
      <HandoffDialog digest={DIGEST} onStartNewSession={onStartNewSession} onClose={onClose} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "新建会话并带入" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("新建会话失败：工作区未打开");
    expect(onClose).not.toHaveBeenCalled();
  });

  it("explains that an empty conversation has nothing to hand off", () => {
    render(<HandoffDialog digest="" onStartNewSession={vi.fn()} onClose={vi.fn()} />);

    expect(screen.getByText("这段对话还没有可以交接的内容。")).toBeTruthy();
    expect(screen.queryByLabelText("交接说明")).toBeNull();
    expect(screen.getByRole("button", { name: "新建会话并带入" })).toBeDisabled();
  });
});
