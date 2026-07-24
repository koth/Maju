import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import MarkdownBody, {
  clearFilePathLinkCacheForTests,
  pathMatchesFragment,
  resolveClickableFilePath,
} from "./MarkdownBody";
import { fsPathExists } from "../../lib/tauri";

vi.mock("../../lib/tauri", async () => {
  const actual = await vi.importActual<typeof import("../../lib/tauri")>(
    "../../lib/tauri",
  );
  return {
    ...actual,
    fsPathExists: vi.fn(async (paths: string[]) => paths.map(() => true)),
  };
});

const originalClipboard = navigator.clipboard;

describe("MarkdownBody", () => {
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    vi.clearAllMocks();
    clearFilePathLinkCacheForTests();
    if (originalClipboard) {
      Object.defineProperty(navigator, "clipboard", {
        value: originalClipboard,
        configurable: true,
      });
    } else {
      Reflect.deleteProperty(navigator, "clipboard");
    }
  });

  it("removes leaked repeated course break noise", () => {
    render(
      <MarkdownBody
        content={[
          "放在 `toggleAllArmourGroupCollapsed` 之后。",
          "",
          "course",
          "<br>",
          "course",
          "",
          "course",
          "",
          "Let me add the derived values.",
        ].join("\n")}
      />,
    );

    expect(screen.getByText(/toggleAllArmourGroupCollapsed/)).toBeInTheDocument();
    expect(screen.getByText(/Let me add the derived values/)).toBeInTheDocument();
    expect(screen.queryByText("course")).not.toBeInTheDocument();
    expect(screen.queryByText("<br>")).not.toBeInTheDocument();
  });

  it("keeps normal course text", () => {
    render(<MarkdownBody content="This course of action is reasonable." />);

    expect(screen.getByText("This course of action is reasonable.")).toBeInTheDocument();
  });

  it("copies fenced code block content", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText },
      configurable: true,
    });

    render(
      <MarkdownBody
        content={[
          "```cpp",
          "AActor* Actor = World->SpawnActor<AActor>(...);",
          "PMC->RegisterComponent();",
          "```",
        ].join("\n")}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "复制代码" }));

    await waitFor(() => {
      expect(writeText).toHaveBeenCalledWith(
        "AActor* Actor = World->SpawnActor<AActor>(...);\nPMC->RegisterComponent();",
      );
    });
    expect(screen.getByRole("button", { name: "已复制代码" })).toBeInTheDocument();
  });

  it("renders existing inline-code file paths as clickable links with line numbers", async () => {
    const onFilePathClick = vi.fn();
    const root = "D:\\work\\kodex";
    render(
      <MarkdownBody
        content={"改动在 `crates/codebuddy-proxy/src/usage.rs:75` 里，另一个是 `to_openai_usage`。"}
        workspaceRoot={root}
        onFilePathClick={onFilePathClick}
      />,
    );

    const pathCode = screen.getByText("crates/codebuddy-proxy/src/usage.rs:75");
    // Renders as plain code until the existence probe resolves.
    expect(pathCode).not.toHaveClass("md-file-path");
    await waitFor(() =>
      expect(screen.getByText("crates/codebuddy-proxy/src/usage.rs:75")).toHaveClass(
        "md-file-path",
      ),
    );
    expect(fsPathExists).toHaveBeenCalledWith([`${root}\\crates\\codebuddy-proxy\\src\\usage.rs`]);
    fireEvent.click(screen.getByText("crates/codebuddy-proxy/src/usage.rs:75"));
    expect(onFilePathClick).toHaveBeenCalledWith(
      `${root}\\crates\\codebuddy-proxy\\src\\usage.rs`,
      75,
    );

    const plainCode = screen.getByText("to_openai_usage");
    expect(plainCode).not.toHaveClass("md-file-path");
    fireEvent.click(plainCode);
    expect(onFilePathClick).toHaveBeenCalledTimes(1);
  });

  it("keeps non-existent paths as plain code", async () => {
    vi.mocked(fsPathExists).mockResolvedValueOnce([false]);
    render(
      <MarkdownBody
        content={"残缺的 `codex_api_proxy/mod.rs:3880` 不应渲染成链接。"}
        workspaceRoot="D:\\work\\kodex"
        onFilePathClick={vi.fn()}
      />,
    );

    const code = screen.getByText("codex_api_proxy/mod.rs:3880");
    await waitFor(() => expect(fsPathExists).toHaveBeenCalled());
    expect(code).not.toHaveClass("md-file-path");
  });

  it("does not mark identifiers or prose as file paths", () => {
    render(
      <MarkdownBody
        content={"函数 `to_openai_usage` 和命令 `cargo test` 不是路径。"}
        workspaceRoot="D:\\work\\kodex"
        onFilePathClick={vi.fn()}
      />,
    );
    expect(screen.getByText("to_openai_usage")).not.toHaveClass("md-file-path");
    expect(screen.getByText("cargo test")).not.toHaveClass("md-file-path");
  });

  it("resolves bare file names via the changeset as the priority source", async () => {
    const onFilePathClick = vi.fn();
    const root = "D:\\work\\kodex";
    render(
      <MarkdownBody
        content={"改在 `Composer.tsx:548` 里，同时 `ConversationTimeline.css:848` 也改了。"}
        workspaceRoot={root}
        onFilePathClick={onFilePathClick}
        changedFiles={[
          "apps/desktop/ui/src/features/composer/Composer.tsx",
          "apps/desktop/ui/src/features/conversation/ConversationTimeline.css",
        ]}
      />,
    );
    await waitFor(() =>
      expect(screen.getByText("Composer.tsx:548")).toHaveClass("md-file-path"),
    );
    expect(screen.getByText("ConversationTimeline.css:848")).toHaveClass("md-file-path");
    fireEvent.click(screen.getByText("Composer.tsx:548"));
    expect(onFilePathClick).toHaveBeenCalledWith(
      `${root}\\apps\\desktop\\ui\\src\\features\\composer\\Composer.tsx`,
      548,
    );
  });

  it("resolves partial relative paths via the changeset", async () => {
    const onFilePathClick = vi.fn();
    const root = "D:\\work\\kodex";
    render(
      <MarkdownBody
        content={"改在 `commands/fs.rs:138` 里。"}
        workspaceRoot={root}
        onFilePathClick={onFilePathClick}
        changedFiles={["apps/desktop/src-tauri/src/commands/fs.rs"]}
      />,
    );
    await waitFor(() =>
      expect(screen.getByText("commands/fs.rs:138")).toHaveClass("md-file-path"),
    );
    fireEvent.click(screen.getByText("commands/fs.rs:138"));
    expect(onFilePathClick).toHaveBeenCalledWith(
      `${root}\\apps\\desktop\\src-tauri\\src\\commands\\fs.rs`,
      138,
    );
  });

  it("resolves bare file names without a line number via the changeset", async () => {
    const onFilePathClick = vi.fn();
    const root = "D:\\work\\kodex";
    render(
      <MarkdownBody
        content={"改在 `MarkdownBody.tsx` 里。"}
        workspaceRoot={root}
        onFilePathClick={onFilePathClick}
        changedFiles={["apps/desktop/ui/src/features/conversation/MarkdownBody.tsx"]}
      />,
    );
    await waitFor(() =>
      expect(screen.getByText("MarkdownBody.tsx")).toHaveClass("md-file-path"),
    );
    fireEvent.click(screen.getByText("MarkdownBody.tsx"));
    expect(onFilePathClick).toHaveBeenCalledWith(
      `${root}\\apps\\desktop\\ui\\src\\features\\conversation\\MarkdownBody.tsx`,
      undefined,
    );
  });

  it("matches space-separated path fragments as a whole against the candidate pool", async () => {
    const onFilePathClick = vi.fn();
    const root = "D:\\work\\kodex";
    render(
      <MarkdownBody
        content={"2. `app-core / state.rs`：创建会话流程允许不绑定 workspace。"}
        workspaceRoot={root}
        onFilePathClick={onFilePathClick}
        candidatePaths={["crates/app-core/src/state.rs"]}
      />,
    );

    await waitFor(() =>
      expect(screen.getByText("app-core / state.rs")).toHaveClass("md-file-path"),
    );
    fireEvent.click(screen.getByText("app-core / state.rs"));
    expect(onFilePathClick).toHaveBeenCalledWith(
      `${root}\\crates\\app-core\\src\\state.rs`,
      undefined,
    );
  });

  it("matches partial relative paths against the candidate pool without a changeset", async () => {
    const onFilePathClick = vi.fn();
    const root = "D:\\work\\kodex";
    render(
      <MarkdownBody
        content={"输出里提到的 `commands/fs.rs:144` 可以直接跳转。"}
        workspaceRoot={root}
        onFilePathClick={onFilePathClick}
        candidatePaths={["apps/desktop/src-tauri/src/commands/fs.rs"]}
      />,
    );

    await waitFor(() =>
      expect(screen.getByText("commands/fs.rs:144")).toHaveClass("md-file-path"),
    );
    fireEvent.click(screen.getByText("commands/fs.rs:144"));
    expect(onFilePathClick).toHaveBeenCalledWith(
      `${root}\\apps\\desktop\\src-tauri\\src\\commands\\fs.rs`,
      144,
    );
  });

  it("keeps spans as plain code when neither the changeset nor the candidate pool matches", async () => {
    vi.mocked(fsPathExists).mockImplementation(async (paths: string[]) => paths.map(() => false));
    render(
      <MarkdownBody
        content={"`SomeUnrelated.tsx:12` 不在本轮上下文里。"}
        workspaceRoot="D:\\work\\kodex"
        onFilePathClick={vi.fn()}
        changedFiles={["apps/desktop/ui/src/features/composer/Composer.tsx"]}
        candidatePaths={["crates/app-core/src/state.rs"]}
      />,
    );

    await waitFor(() => expect(fsPathExists).toHaveBeenCalled());
    expect(screen.getByText("SomeUnrelated.tsx:12")).not.toHaveClass("md-file-path");
  });
});

describe("resolveClickableFilePath", () => {
  const root = "D:\\work\\kodex";

  it("resolves relative paths with line and column", () => {
    expect(resolveClickableFilePath("crates/acp-core/src/mapping.rs:391", root)).toMatchObject({
      path: `${root}\\crates\\acp-core\\src\\mapping.rs`,
      lineNumber: 391,
    });
    expect(resolveClickableFilePath("src/lib.rs:10:5", root)).toMatchObject({
      path: `${root}\\src\\lib.rs`,
      lineNumber: 10,
    });
  });

  it("resolves diff-prefixed and absolute paths", () => {
    expect(resolveClickableFilePath("a/crates/x.rs:3", root)).toMatchObject({
      path: `${root}\\crates\\x.rs`,
      lineNumber: 3,
    });
    expect(resolveClickableFilePath("D:\\work\\kodex\\src\\main.rs:8", root)).toMatchObject({
      path: "D:\\work\\kodex\\src\\main.rs",
      lineNumber: 8,
    });
    expect(resolveClickableFilePath("/home/user/repo/src/main.rs", root)).toMatchObject({
      path: "/home/user/repo/src/main.rs",
      lineNumber: undefined,
    });
  });

  it("rejects identifiers, commands, urls, and directories", () => {
    expect(resolveClickableFilePath("to_openai_usage", root)).toBeNull();
    expect(resolveClickableFilePath("cargo test -p foo", root)).toBeNull();
    expect(resolveClickableFilePath("https://example.com/a.rs", root)).toBeNull();
    expect(resolveClickableFilePath("crates/acp-core/src/", root)).toBeNull();
    expect(resolveClickableFilePath("README", root)).toBeNull();
  });

  it("accepts space-separated path fragments and normalises them", () => {
    expect(resolveClickableFilePath("app-core / state.rs", root)).toEqual({
      path: `${root}\\app-core\\state.rs`,
      lineNumber: undefined,
      matchTail: "app-core/state.rs",
    });
    expect(resolveClickableFilePath("crates / app-core / src / state.rs:12", root)).toEqual({
      path: `${root}\\crates\\app-core\\src\\state.rs`,
      lineNumber: 12,
      matchTail: "crates/app-core/src/state.rs",
    });
    expect(resolveClickableFilePath("foo bar.rs", root)).toBeNull();
  });

  it("requires a workspace root for relative paths", () => {
    expect(resolveClickableFilePath("crates/x.rs:1")).toBeNull();
  });

  it("treats bare file names with a line reference as name-search candidates", () => {
    expect(resolveClickableFilePath("Composer.tsx:548", root)).toEqual({
      path: "Composer.tsx",
      lineNumber: 548,
      matchTail: "Composer.tsx",
    });
    expect(resolveClickableFilePath("taxonomy.ts:103", root)).toEqual({
      path: "taxonomy.ts",
      lineNumber: 103,
      matchTail: "taxonomy.ts",
    });
  });

  it("accepts bare names with or without a line reference", () => {
    expect(resolveClickableFilePath("tagger.ts", root)).toMatchObject({
      path: "tagger.ts",
      matchTail: "tagger.ts",
    });
    expect(resolveClickableFilePath("Composer.tsx:548", root)).toMatchObject({
      path: "Composer.tsx",
      lineNumber: 548,
      matchTail: "Composer.tsx",
    });
  });

  it("rejects bare names without an extension", () => {
    expect(resolveClickableFilePath("README", root)).toBeNull();
    expect(resolveClickableFilePath("{ }", root)).toBeNull();
  });
});

describe("pathMatchesFragment", () => {
  it("matches fragments as an ordered segment subsequence", () => {
    expect(pathMatchesFragment("crates/app-core/src/state.rs", "app-core/state.rs")).toBe(true);
    expect(pathMatchesFragment("apps/desktop/src-tauri/src/commands/fs.rs", "commands/fs.rs")).toBe(true);
    expect(pathMatchesFragment("apps/desktop/ui/src/features/composer/Composer.tsx", "Composer.tsx")).toBe(true);
    expect(pathMatchesFragment("crates/app-core/src/state.rs", "app-core/state.rs:12")).toBe(true);
  });

  it("rejects out-of-order or foreign fragments", () => {
    expect(pathMatchesFragment("crates/app-core/src/state.rs", "state.rs/app-core")).toBe(false);
    expect(pathMatchesFragment("crates/app-core/src/state.rs", "other/state.rs")).toBe(false);
    expect(pathMatchesFragment("crates/app-core/src/state.rs", "app-core/lib.rs")).toBe(false);
  });
});
