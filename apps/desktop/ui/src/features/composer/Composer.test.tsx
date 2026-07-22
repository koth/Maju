import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { Composer } from "./Composer";
import { resetAllComposerDrafts } from "./composer-draft-store";
import { editorGetContent, fsMentionSuggest, sessionCancel, sessionSendPrompt, sessionSetConfigControl } from "../../lib/tauri";
import type { UiSnapshot } from "../../types";

vi.mock("../../lib/tauri", () => ({
  editorGetContent: vi.fn(),
  sessionCancel: vi.fn(),
  sessionReconnect: vi.fn(),
  sessionSendPrompt: vi.fn(),
  sessionSetConfigControl: vi.fn(),
  fsMentionSuggest: vi.fn(),
}));

function makeSnapshot(overrides: Partial<UiSnapshot> = {}): UiSnapshot {
  return {
    revision: 1,
    workspace: { id: "ws-1", name: "test", root: "/repo" },
    session: {
      id: "s-1",
      workspace_id: "ws-1",
      title: "test",
      model: "test-model",
      mode: null,
      agent_cli: null,
      status: "Idle",
    },
    session_config: { hydrated: true, controls: [] },
    prompt_capabilities: { image: true, embedded_context: true, session_steer: false },
    available_commands: [],
    agent_plan: [],
    messages: [],
    timeline: [],
    tools: [],
    repository: { branch: "main", head: "abc", changed_files: [] },
    inspector_tab: "Activity",
    inspector_sections: [],
    session_changes: [],
    review_changes: [],
    turn_changes: [],
    thinking_status: null,
    ...overrides,
  };
}

describe("Composer", () => {
  beforeEach(() => {
    resetAllComposerDrafts();
    vi.stubGlobal(
      "Image",
      class Image {
        onerror: (() => void) | null = null;
        set src(_value: string) {
          setTimeout(() => this.onerror?.(), 0);
        }
      },
    );
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
    vi.unstubAllGlobals();
  });

  it("marks the composer compact when used over expanded review", () => {
    const { container } = render(
      <Composer snapshot={makeSnapshot()} onStateChange={vi.fn()} compact />,
    );

    expect(container.querySelector(".composer")).toHaveClass("is-compact");
  });

  it("waits for the model list before enabling prompt input", () => {
    const snapshot = makeSnapshot({
      session_config: { hydrated: false, controls: [] },
    });

    render(<Composer snapshot={snapshot} onStateChange={vi.fn()} />);

    expect(screen.getByRole("status", { name: "模型加载中" })).toBeInTheDocument();
    expect(screen.getByRole("textbox")).toBeDisabled();
    expect(screen.getByPlaceholderText("正在加载模型列表...")).toBeDisabled();
    expect(screen.getByRole("button", { name: "附加图片或文件" })).toBeDisabled();
    const sendButton = screen.getByRole("button", { name: "正在加载模型列表" });
    expect(sendButton).toBeDisabled();

    fireEvent.click(sendButton);
    expect(sessionSendPrompt).not.toHaveBeenCalled();
  });

  it("keeps compact composer controls on the input row", () => {
    const snapshot = makeSnapshot({
      session_config: {
        hydrated: true,
        controls: [
          {
            id: "mode",
            label: "Mode",
            description: null,
            category: "Mode",
            source: "LocalMode",
            current_value_id: "Build",
            current_value_label: "Build",
            enabled: true,
            choices: [
              { id: "Build", label: "Build", description: null, provider: null },
              { id: "Plan", label: "Plan", description: null, provider: null },
            ],
          },
          {
            id: "model",
            label: "Model",
            description: null,
            category: "Model",
            source: "SessionModel",
            current_value_id: "deepseek-v4-pro",
            current_value_label: "deepseek-v4-pro",
            enabled: true,
            choices: [
              { id: "deepseek-v4-pro", label: "deepseek-v4-pro", description: null, provider: "deepseek" },
            ],
          },
        ],
      },
    });

    const { container } = render(
      <Composer snapshot={snapshot} onStateChange={vi.fn()} compact />,
    );

    const composer = container.querySelector(".composer");
    expect(composer).toHaveClass("is-compact");
    expect(within(composer as HTMLElement).getByRole("button", { name: "附加图片或文件" })).toBeInTheDocument();
    expect(within(composer as HTMLElement).getByRole("textbox")).toBeInTheDocument();
    expect(within(composer as HTMLElement).getByRole("button", { name: "Build" })).toBeInTheDocument();
    expect(within(composer as HTMLElement).getByRole("button", { name: /Provider.*DeepSeek/ })).toBeInTheDocument();
    expect(within(composer as HTMLElement).getByRole("button", { name: /Model.*deepseek-v4-pro/ })).toBeInTheDocument();
    expect(within(composer as HTMLElement).getByRole("button", { name: "发送提示" })).toBeInTheDocument();
  });

  it("renders image attachments as clickable previews without visible file names", async () => {
    const imageUrl = "data:image/png;base64,iVBORw0KGgo=";
    vi.mocked(editorGetContent).mockResolvedValue({
      content: imageUrl,
      kind: "image",
      mime_type: "image/png",
      path: "assets/screenshot.png",
      version: { content_hash: "hash", modified_ms: null, size: 12 },
    });

    render(
      <Composer
        snapshot={makeSnapshot()}
        onStateChange={vi.fn()}
        referenceRequests={[{ id: "ref-1", path: "assets/screenshot.png" }]}
      />,
    );

    const previewButton = await screen.findByRole("button", {
      name: "预览 screenshot.png",
    });
    expect(screen.queryByText("screenshot.png")).not.toBeInTheDocument();

    fireEvent.click(previewButton);

    const dialog = await screen.findByRole("dialog", { name: "图片预览" });
    await waitFor(() => {
      expect(within(dialog).getByAltText("screenshot.png")).toHaveAttribute("src", imageUrl);
    });
  });

  it("renders workspace file references as mention chips", async () => {
    render(
      <Composer
        snapshot={makeSnapshot()}
        onStateChange={vi.fn()}
        referenceRequests={[
          {
            id: "ref-1",
            path: "src/features/composer/Composer.tsx",
            startLine: 4,
            endLine: 8,
          },
        ]}
      />,
    );

    expect(await screen.findByText("@src/features/composer/Composer.tsx#L4-L8")).toBeInTheDocument();
    expect(screen.queryByText("REF")).not.toBeInTheDocument();
    expect(editorGetContent).not.toHaveBeenCalled();
  });

  it("sends workspace references as structured mentions without eager file content", async () => {
    vi.mocked(sessionSendPrompt).mockResolvedValue(undefined);

    render(
      <Composer
        snapshot={makeSnapshot()}
        onStateChange={vi.fn()}
        referenceRequests={[
          {
            id: "ref-1",
            path: "src/features/composer/Composer.tsx",
            startLine: 4,
            endLine: 8,
            text: "export function Composer() {}",
          },
        ]}
      />,
    );

    await screen.findByText("@src/features/composer/Composer.tsx#L4-L8");
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "看看这里" } });
    fireEvent.click(screen.getByRole("button", { name: "发送提示" }));

    await waitFor(() => {
      expect(sessionSendPrompt).toHaveBeenCalledWith([
        {
          type: "workspace_file",
          path: "src/features/composer/Composer.tsx",
          start_line: 4,
          end_line: 8,
        },
        { type: "text", text: "看看这里" },
      ]);
    });
    expect(editorGetContent).not.toHaveBeenCalled();
  });

  it("switches the active primary action from stop to steer when text is present", async () => {
    vi.mocked(sessionSendPrompt).mockResolvedValue(undefined);
    const onStateChange = vi.fn();
    const snapshot = makeSnapshot({
      session: { ...makeSnapshot().session, status: "Streaming" },
      prompt_capabilities: { image: true, embedded_context: true, session_steer: true },
    });

    render(<Composer snapshot={snapshot} onStateChange={onStateChange} />);

    const textbox = screen.getByRole("textbox");
    expect(textbox).not.toBeDisabled();
    expect(screen.getByRole("button", { name: "停止当前轮次" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "追加指令" })).not.toBeInTheDocument();

    fireEvent.change(textbox, { target: { value: "加一个边界条件" } });
    const steerButton = screen.getByRole("button", { name: "追加指令" });
    expect(screen.queryByRole("button", { name: "停止当前轮次" })).not.toBeInTheDocument();
    fireEvent.click(steerButton);

    await waitFor(() => {
      expect(sessionSendPrompt).toHaveBeenCalledWith([
        { type: "text", text: "加一个边界条件" },
      ]);
    });
    expect(sessionCancel).not.toHaveBeenCalled();
    expect(onStateChange).toHaveBeenCalled();
  });

  it("stops active turns from the shared primary action when text is empty", async () => {
    vi.mocked(sessionCancel).mockResolvedValue(undefined);
    const snapshot = makeSnapshot({
      session: { ...makeSnapshot().session, status: "WaitingForTool" },
      prompt_capabilities: { image: true, embedded_context: true, session_steer: true },
    });

    render(<Composer snapshot={snapshot} onStateChange={vi.fn()} />);

    expect(screen.queryByRole("button", { name: "追加指令" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "停止当前轮次" }));

    await waitFor(() => {
      expect(sessionCancel).toHaveBeenCalled();
    });
    expect(sessionSendPrompt).not.toHaveBeenCalled();
  });

  it("disables active steer input when the session lacks steering support", () => {
    const snapshot = makeSnapshot({
      session: { ...makeSnapshot().session, status: "Streaming" },
      prompt_capabilities: { image: true, embedded_context: true, session_steer: false },
    });

    render(<Composer snapshot={snapshot} onStateChange={vi.fn()} />);

    expect(screen.getByRole("textbox")).toBeDisabled();
    expect(screen.getByRole("button", { name: "停止当前轮次" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "追加指令" })).not.toBeInTheDocument();
  });

  it("keeps active turn controls and attachments disabled", () => {
    const snapshot = makeSnapshot({
      session: { ...makeSnapshot().session, status: "Streaming" },
      prompt_capabilities: { image: true, embedded_context: true, session_steer: true },
      session_config: {
        hydrated: true,
        controls: [
          {
            id: "mode",
            label: "Mode",
            description: null,
            category: "Mode",
            source: "LocalMode",
            current_value_id: "Build",
            current_value_label: "Build",
            enabled: true,
            choices: [
              { id: "Build", label: "Build", description: null, provider: null },
              { id: "Plan", label: "Plan", description: null, provider: null },
            ],
          },
          {
            id: "model",
            label: "Model",
            description: null,
            category: "Model",
            source: "SessionModel",
            current_value_id: "deepseek-v4-pro",
            current_value_label: "deepseek-v4-pro",
            enabled: true,
            choices: [
              { id: "deepseek-v4-pro", label: "deepseek-v4-pro", description: null, provider: "deepseek" },
            ],
          },
        ],
      },
    });

    render(<Composer snapshot={snapshot} onStateChange={vi.fn()} />);

    expect(screen.getByRole("button", { name: "附加图片或文件" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Build" })).toBeDisabled();
    expect(screen.getByRole("button", { name: /Provider.*DeepSeek/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: /Model.*deepseek-v4-pro/ })).toBeDisabled();
  });

  it("splits BYOK model choices by provider in the composer controls", async () => {
    vi.mocked(sessionSetConfigControl).mockResolvedValue({ hydrated: true, controls: [] });
    const snapshot = makeSnapshot({
      session: {
        ...makeSnapshot().session,
        model: "deepseek-v4-pro",
      },
      session_config: {
        hydrated: true,
        controls: [
          {
            id: "model",
            label: "Model",
            description: null,
            category: "Model",
            source: "SessionModel",
            current_value_id: "deepseek-v4-pro",
            current_value_label: "deepseek-v4-pro",
            enabled: true,
            choices: [
              { id: "deepseek-v4-pro", label: "deepseek-v4-pro", description: null, provider: "deepseek" },
              { id: "kimi-for-coding", label: "kimi-for-coding", description: null, provider: "kimi_code" },
              { id: "mimo-v2.5-pro", label: "MiMo-V2.5-Pro", description: null, provider: "xiaomi_mimo" },
            ],
          },
        ],
      },
    });

    render(<Composer snapshot={snapshot} onStateChange={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: /Provider.*DeepSeek/ }));
    fireEvent.click(await screen.findByRole("option", { name: "Kimi Code" }));

    await waitFor(() =>
      expect(sessionSetConfigControl).toHaveBeenCalledWith("model", "kodex-provider/byok/kimi_code/kimi-for-coding", null),
    );
  });

  it("resubmits the visible provider when reselecting an ambiguous bare model", async () => {
    vi.mocked(sessionSetConfigControl).mockResolvedValue({ hydrated: true, controls: [] });
    const snapshot = makeSnapshot({
      session: {
        ...makeSnapshot().session,
        model: "gpt-5.4",
      },
      session_config: {
        hydrated: true,
        controls: [
          {
            id: "model",
            label: "Model",
            description: null,
            category: "Model",
            source: "SessionModel",
            current_value_id: "gpt-5.4",
            current_value_label: "gpt-5.4",
            enabled: true,
            choices: [
              { id: "gpt-5.4", label: "gpt-5.4", description: null, provider: "commandcode" },
              { id: "gpt-5.4", label: "gpt-5.4", description: null, provider: "custom", provider_label: "Lab Provider" },
            ],
          },
        ],
      },
    });

    render(<Composer snapshot={snapshot} onStateChange={vi.fn()} />);

    expect(screen.getByRole("button", { name: /Provider.*CommandCode/ })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /Model.*gpt-5\.4/ }));
    fireEvent.click(await screen.findByRole("option", { name: "gpt-5.4" }));

    await waitFor(() =>
      expect(sessionSetConfigControl).toHaveBeenCalledWith("model", "kodex-provider/byok/commandcode/gpt-5.4", null),
    );
  });
  it("shows the saved custom provider label and selects its encoded model", async () => {
    vi.mocked(sessionSetConfigControl).mockResolvedValue({ hydrated: true, controls: [] });
    const snapshot = makeSnapshot({
      session: {
        ...makeSnapshot().session,
        model: "kodex-provider/byok/commandcode/gpt-5.4",
      },
      session_config: {
        hydrated: true,
        controls: [
          {
            id: "model",
            label: "Model",
            description: null,
            category: "Model",
            source: "SessionModel",
            current_value_id: "kodex-provider/byok/commandcode/gpt-5.4",
            current_value_label: "kodex-provider/byok/commandcode/gpt-5.4",
            enabled: true,
            choices: [
              {
                id: "kodex-provider/byok/commandcode/gpt-5.4",
                label: "kodex-provider/byok/commandcode/gpt-5.4",
                description: null,
                provider: "commandcode",
                provider_label: "CommandCode",
              },
              {
                id: "kodex-provider/byok/custom/lab-model",
                label: "kodex-provider/byok/custom/lab-model",
                description: null,
                provider: "custom",
                provider_label: "Lab Provider",
              },
            ],
          },
        ],
      },
    });

    render(<Composer snapshot={snapshot} onStateChange={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: /Provider.*CommandCode/ }));

    expect(await screen.findByRole("option", { name: "Lab Provider" })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /^custom$/i })).toBeNull();

    fireEvent.click(screen.getByRole("option", { name: "Lab Provider" }));

    await waitFor(() =>
      expect(sessionSetConfigControl).toHaveBeenCalledWith(
        "model",
        "kodex-provider/byok/custom/lab-model",
        null,
      ),
    );
  });
  it("encodes bare custom provider model choices before updating the session", async () => {
    vi.mocked(sessionSetConfigControl).mockResolvedValue({ hydrated: true, controls: [] });
    const snapshot = makeSnapshot({
      session: {
        ...makeSnapshot().session,
        model: "kodex-provider/byok/commandcode/gpt-5.4",
      },
      session_config: {
        hydrated: true,
        controls: [
          {
            id: "model",
            label: "Model",
            description: null,
            category: "Model",
            source: "SessionModel",
            current_value_id: "kodex-provider/byok/commandcode/gpt-5.4",
            current_value_label: "kodex-provider/byok/commandcode/gpt-5.4",
            enabled: true,
            choices: [
              {
                id: "kodex-provider/byok/commandcode/gpt-5.4",
                label: "kodex-provider/byok/commandcode/gpt-5.4",
                description: null,
                provider: "commandcode",
                provider_label: "CommandCode",
              },
              {
                id: "lab-model",
                label: "lab-model",
                description: null,
                provider: "custom",
                provider_label: "Lab Provider",
              },
            ],
          },
        ],
      },
    });

    render(<Composer snapshot={snapshot} onStateChange={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: /Provider.*CommandCode/ }));
    fireEvent.click(await screen.findByRole("option", { name: "Lab Provider" }));

    await waitFor(() =>
      expect(sessionSetConfigControl).toHaveBeenCalledWith(
        "model",
        "kodex-provider/byok/custom/lab-model",
        null,
      ),
    );
  });
  it("selects the provider encoded in the current model value", () => {
    const snapshot = makeSnapshot({
      session: {
        ...makeSnapshot().session,
        model: "kodex-provider/byok/kimi_code/kimi-for-coding",
      },
      session_config: {
        hydrated: true,
        controls: [
          {
            id: "model",
            label: "Model",
            description: null,
            category: "Model",
            source: "SessionModel",
            current_value_id: "kodex-provider/byok/kimi_code/kimi-for-coding",
            current_value_label: "kodex-provider/byok/kimi_code/kimi-for-coding",
            enabled: true,
            choices: [
              { id: "agent-default", label: "Agent default", description: null, provider: null },
              { id: "gpt-5.5", label: "gpt-5.5", description: null, provider: "commandcode" },
              { id: "kimi-for-coding", label: "kimi-for-coding", description: null, provider: "kimi_code" },
            ],
          },
        ],
      },
    });

    render(<Composer snapshot={snapshot} onStateChange={vi.fn()} />);

    expect(screen.getByRole("button", { name: /Provider.*Kimi Code/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Model.*kimi-for-coding/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Provider.*CommandCode/ })).toBeNull();
  });

  it("infers Kimi Code for a bare current Kimi model shared by multiple providers", () => {
    const snapshot = makeSnapshot({
      session: {
        ...makeSnapshot().session,
        model: "kimi-for-coding",
      },
      session_config: {
        hydrated: true,
        controls: [
          {
            id: "model",
            label: "Model",
            description: null,
            category: "Model",
            source: "SessionModel",
            current_value_id: "kimi-for-coding",
            current_value_label: "kimi-for-coding",
            enabled: true,
            choices: [
              { id: "kimi-for-coding", label: "kimi-for-coding", description: null, provider: "commandcode" },
              { id: "kimi-for-coding", label: "kimi-for-coding", description: null, provider: "kimi_code" },
            ],
          },
        ],
      },
    });

    render(<Composer snapshot={snapshot} onStateChange={vi.fn()} />);

    expect(screen.getByRole("button", { name: /Provider.*Kimi Code/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Model.*kimi-for-coding/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Provider.*CommandCode/ })).toBeNull();
  });

  it("does not expose generic BYOK as a remote model provider group", () => {
    const snapshot = makeSnapshot({
      session: {
        ...makeSnapshot().session,
        model: "kimi-for-coding",
      },
      session_config: {
        hydrated: true,
        controls: [
          {
            id: "model",
            label: "Model",
            description: null,
            category: "Model",
            source: "SessionModel",
            current_value_id: "kimi-for-coding",
            current_value_label: "kimi-for-coding",
            enabled: true,
            choices: [
              { id: "qwen/qwen3-coder", label: "qwen/qwen3-coder", description: null, provider: "commandcode" },
              { id: "kimi-for-coding", label: "kimi-for-coding", description: null, provider: "byok" },
            ],
          },
        ],
      },
    });

    render(<Composer snapshot={snapshot} onStateChange={vi.fn()} />);

    expect(screen.getByRole("button", { name: /Provider.*Kimi Code/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Model.*kimi-for-coding/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Provider.*byok/i })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: /Provider.*Kimi Code/ }));

    expect(screen.getByRole("option", { name: "CommandCode" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Kimi Code" })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /byok/i })).toBeNull();
  });

  it("splits encoded provider model ids without duplicating the provider request", async () => {
    vi.mocked(sessionSetConfigControl).mockResolvedValue({ hydrated: true, controls: [] });
    const snapshot = makeSnapshot({
      session: {
        ...makeSnapshot().session,
        model: "kodex-provider/byok/commandcode/gpt-5.5",
      },
      session_config: {
        hydrated: true,
        controls: [
          {
            id: "model",
            label: "Model",
            description: null,
            category: "Model",
            source: "SessionModel",
            current_value_id: "kodex-provider/byok/commandcode/gpt-5.5",
            current_value_label: "kodex-provider/byok/commandcode/gpt-5.5",
            enabled: true,
            choices: [
              { id: "agent-default", label: "Agent default", description: null, provider: null },
              { id: "kodex-provider/byok/commandcode/gpt-5.5", label: "kodex-provider/byok/commandcode/gpt-5.5", description: null, provider: null },
              { id: "kodex-provider/byok/kimi_code/kimi-for-coding", label: "kodex-provider/byok/kimi_code/kimi-for-coding", description: null, provider: null },
            ],
          },
        ],
      },
    });

    render(<Composer snapshot={snapshot} onStateChange={vi.fn()} />);

    expect(screen.getByRole("button", { name: /Provider.*CommandCode/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Model.*gpt-5\.5/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /kodex-provider\/commandcode/ })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: /Model.*gpt-5\.5/ }));
    expect(await screen.findByRole("option", { name: "gpt-5.5" })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /kodex-provider\/commandcode/ })).toBeNull();
    fireEvent.keyDown(document, { key: "Escape" });

    fireEvent.click(screen.getByRole("button", { name: /Provider.*CommandCode/ }));
    fireEvent.click(await screen.findByRole("option", { name: "Kimi Code" }));

    await waitFor(() =>
      expect(sessionSetConfigControl).toHaveBeenCalledWith(
        "model",
        "kodex-provider/byok/kimi_code/kimi-for-coding",
        null,
      ),
    );
  });

  it("inserts a file reference from the @ mention menu", async () => {
    vi.mocked(fsMentionSuggest).mockResolvedValue([
      {
        name: "Composer.tsx",
        kind: "File",
        path: "apps/desktop/ui/src/features/composer/Composer.tsx",
      },
    ]);
    render(<Composer snapshot={makeSnapshot()} onStateChange={vi.fn()} />);

    const textbox = screen.getByRole("textbox") as HTMLTextAreaElement;
    // jsdom leaves the caret at the start after a value assignment; move it
    // to the end and re-dispatch so the mention scanner observes the caret.
    fireEvent.change(textbox, { target: { value: "@comp" } });
    textbox.setSelectionRange(5, 5);
    fireEvent.change(textbox);

    const option = await screen.findByRole("option", { name: /Composer\.tsx/ });
    fireEvent.mouseDown(option);

    // The `@query` text is removed and a reference chip appears.
    await waitFor(() => expect((screen.getByRole("textbox") as HTMLTextAreaElement).value).toBe(""));
    expect(
      await screen.findByText("@apps/desktop/ui/src/features/composer/Composer.tsx"),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "发送提示" }));
    await waitFor(() => {
      expect(sessionSendPrompt).toHaveBeenCalledWith([
        {
          type: "workspace_file",
          path: "apps/desktop/ui/src/features/composer/Composer.tsx",
          start_line: null,
          end_line: null,
        },
      ]);
    });
  });
});
