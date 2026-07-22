import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import MarkdownBody, {
  clearFilePathLinkCacheForTests,
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
        content={"改动在 `crates/codebuddy-proxy/src/usage.rs:75` 里，另一个是 `README.md`。"}
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

    const plainCode = screen.getByText("README.md");
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
});

describe("resolveClickableFilePath", () => {
  const root = "D:\\work\\kodex";

  it("resolves relative paths with line and column", () => {
    expect(resolveClickableFilePath("crates/acp-core/src/mapping.rs:391", root)).toEqual({
      path: `${root}\\crates\\acp-core\\src\\mapping.rs`,
      lineNumber: 391,
    });
    expect(resolveClickableFilePath("src/lib.rs:10:5", root)).toEqual({
      path: `${root}\\src\\lib.rs`,
      lineNumber: 10,
    });
  });

  it("resolves diff-prefixed and absolute paths", () => {
    expect(resolveClickableFilePath("a/crates/x.rs:3", root)).toEqual({
      path: `${root}\\crates\\x.rs`,
      lineNumber: 3,
    });
    expect(resolveClickableFilePath("D:\\work\\kodex\\src\\main.rs:8", root)).toEqual({
      path: "D:\\work\\kodex\\src\\main.rs",
      lineNumber: 8,
    });
    expect(resolveClickableFilePath("/home/user/repo/src/main.rs", root)).toEqual({
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

  it("requires a workspace root for relative paths", () => {
    expect(resolveClickableFilePath("crates/x.rs:1")).toBeNull();
  });
});
