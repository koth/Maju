import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import MarkdownBody, {
  pathMatchesFragment,
  resolveClickableFilePath,
} from "./MarkdownBody";

const originalClipboard = navigator.clipboard;

describe("MarkdownBody", () => {
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    vi.clearAllMocks();
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

  it("reports image clicks for large previews", () => {
    const onImagePreview = vi.fn();
    render(
      <MarkdownBody
        content="![生成的图片](data:image/png;base64,aaaa)"
        onImagePreview={onImagePreview}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "预览 生成的图片" }));
    expect(onImagePreview).toHaveBeenCalledWith(
      "data:image/png;base64,aaaa",
      "生成的图片",
    );
  });

  it("renders inline-code file paths as fixed file links with line numbers", () => {
    // Deterministic: the shape decides at render time — no existence probe,
    // no async flip. Click-time resolution hands the editor the file.
    const onFilePathClick = vi.fn();
    const root = "D:\\work\\kodex";
    render(
      <MarkdownBody
        content={"改动在 `crates/codebuddy-proxy/src/usage.rs:75` 里，另一个是 `to_openai_usage`。"}
        workspaceRoot={root}
        onFilePathClick={onFilePathClick}
      />,
    );

    const fileLink = screen.getByText("crates/codebuddy-proxy/src/usage.rs:75");
    expect(fileLink).toHaveClass("md-file-path");
    expect(fileLink).toHaveAttribute("tabindex", "0");
    expect(fileLink.querySelector(".md-file-path-icon")).toHaveAttribute(
      "aria-hidden",
      "true",
    );
    expect(fileLink.getAttribute("data-file-path")).toBe(
      "crates/codebuddy-proxy/src/usage.rs#75",
    );
    fireEvent.keyDown(fileLink, { key: "Enter" });
    expect(onFilePathClick).toHaveBeenCalledWith(
      "crates/codebuddy-proxy/src/usage.rs",
      75,
    );
    onFilePathClick.mockClear();
    fireEvent.click(fileLink);
    expect(onFilePathClick).toHaveBeenCalledWith(
      "crates/codebuddy-proxy/src/usage.rs",
      75,
    );

    const plainCode = screen.getByText("to_openai_usage");
    expect(plainCode).not.toHaveClass("md-file-path");
    fireEvent.click(plainCode);
    expect(onFilePathClick).toHaveBeenCalledTimes(1);
  });

  it("renders identical markup across re-renders (fixed, no per-render re-computation)", () => {
    // The render must be a pure function of content + workspace root. An
    // earlier probe-based design recomputed clickability per render and could
    // flip or lose links depending on async timing.
    const content = "改了 `crates/dsh-bridge/src/process.rs` 这个文件";
    const { container, rerender } = render(
      <MarkdownBody content={content} workspaceRoot="/test" onFilePathClick={vi.fn()} />,
    );
    const first = container.querySelector(".md-body")!.innerHTML;
    rerender(
      <MarkdownBody content={content} workspaceRoot="/test" onFilePathClick={vi.fn()} />,
    );
    expect(container.querySelector(".md-body")!.innerHTML).toBe(first);
    expect(container.querySelector("code")!).toHaveClass("md-file-path");
  });

  it("markdown links to workspace files render as fixed file links that open", () => {
    // Regression: `[label](crates/foo.rs#L12)` rendered as a dead relative-URL
    // anchor — the `#L…` fragment broke path resolution and the url transform
    // stripped the href, so clicking did nothing. File links open the editor
    // like inline-code paths do.
    const onFilePathClick = vi.fn();
    render(
      <MarkdownBody
        content={"见 [usage.rs](crates/codebuddy-proxy/src/usage.rs#L75) 里的改动。"}
        workspaceRoot="/test"
        onFilePathClick={onFilePathClick}
      />,
    );
    const fileLink = screen.getByRole("link", { name: /usage\.rs/ });
    expect(fileLink).toHaveClass("md-file-path");
    expect(fileLink.getAttribute("data-file-path")).toBe(
      "crates/codebuddy-proxy/src/usage.rs#75",
    );
    fireEvent.click(fileLink);
    expect(onFilePathClick).toHaveBeenCalledWith(
      "crates/codebuddy-proxy/src/usage.rs",
      75,
    );
  });

  it("keeps external links as plain external anchors", async () => {
    render(
      <MarkdownBody
        content={"见 [官网](https://example.com/docs) 了解详情。"}
        workspaceRoot="/test"
        onFilePathClick={vi.fn()}
      />,
    );
    const link = screen.getByRole("link", { name: "官网" });
    expect(link).toHaveAttribute("href", "https://example.com/docs");
    expect(link).not.toHaveClass("md-file-path");
  });

  it("renders non-existent file references as fixed links too", () => {
    // Deterministic rendering: the shape decides, never an existence probe —
    // a hallucinated path still renders as a link, and the editor reports the
    // miss on open. Stable markup beats precise-but-flaky markup.
    const onFilePathClick = vi.fn();
    render(
      <MarkdownBody
        content={"见 [ghost](crates/nope/ghost.rs) 和 `crates/nope/missing.rs:9`。"}
        workspaceRoot="/test"
        onFilePathClick={onFilePathClick}
      />,
    );
    for (const label of ["ghost", "crates/nope/missing.rs:9"]) {
      const el = screen.getByText(label);
      if (label === "ghost") {
        // The LABEL is not a file — no file treatment (no icon, no file
        // typography), but the link still opens its target on click.
        expect(el).not.toHaveClass("md-file-path");
        expect(el.querySelector(".md-file-path-icon")).toBeNull();
      } else {
        expect(el).toHaveClass("md-file-path");
      }
      fireEvent.click(el);
    }
    expect(onFilePathClick).toHaveBeenCalledWith("crates/nope/ghost.rs", undefined);
    expect(onFilePathClick).toHaveBeenCalledWith("crates/nope/missing.rs", 9);
  });

  it("renders markdown links with non-file labels without the file treatment", () => {
    // `[test-name](file.rs)` — the target is a file but the LABEL is not:
    // rendering it as a file chip (icon + file typography) claimed the text
    // was a file and its colors clashed with the surrounding inline code.
    const onFilePathClick = vi.fn();
    render(
      <MarkdownBody
        content={
          "新增 [`host_status_idle_racing_turn_end_does_not_interrupt`](crates/dsh-bridge/tests/harness_integration.rs) 回归测试。"
        }
        workspaceRoot="/test"
        onFilePathClick={onFilePathClick}
      />,
    );
    const link = screen.getByRole("link", { name: /harness_integration\.rs/ });
    expect(link).not.toHaveClass("md-file-path");
    expect(link.querySelector(".md-file-path-icon")).toBeNull();
    // The label keeps its own inline-code rendering…
    expect(link.querySelector("code")).toHaveClass("md-inline-code");
    expect(link.querySelector("code")).not.toHaveClass("md-file-path");
    // …and the link still opens the target file.
    fireEvent.click(link);
    expect(onFilePathClick).toHaveBeenCalledWith(
      "crates/dsh-bridge/tests/harness_integration.rs",
      undefined,
    );
  });

  it("renders markdown links with code file labels with exactly one icon", () => {
    // `[`live_turn_probe.rs`](path)` used to grow TWO file icons — one from
    // the anchor and one from the nested code span file-linkifying on its own
    // — and the nested chip's colors fought the link's file typography.
    const onFilePathClick = vi.fn();
    const { container } = render(
      <MarkdownBody
        content={
          "探针保留为 [`live_turn_probe.rs`](crates/dsh-bridge/tests/live_turn_probe.rs) 诊断工具。"
        }
        workspaceRoot="/test"
        onFilePathClick={onFilePathClick}
      />,
    );
    const link = screen.getByRole("link", { name: /live_turn_probe\.rs/ });
    expect(link).toHaveClass("md-file-path");
    expect(container.querySelectorAll(".md-file-path-icon")).toHaveLength(1);
    // The nested code span is just the label: no icon, no second click surface.
    const label = link.querySelector("code");
    expect(label).toHaveClass("md-file-path-label");
    expect(label).not.toHaveAttribute("data-file-path");
    fireEvent.click(link);
    expect(onFilePathClick).toHaveBeenCalledTimes(1);
    expect(onFilePathClick).toHaveBeenCalledWith(
      "crates/dsh-bridge/tests/live_turn_probe.rs",
      undefined,
    );
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
    expect(screen.getByText("to_openai_usage")).not.toHaveAttribute("role", "link");
  });

  it("resolves bare file names via the changeset at click time", () => {
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
    // Fixed chips at render; the pool resolves the real path on click.
    expect(screen.getByText("Composer.tsx:548")).toHaveClass("md-file-path");
    expect(screen.getByText("ConversationTimeline.css:848")).toHaveClass("md-file-path");
    fireEvent.click(screen.getByText("Composer.tsx:548"));
    expect(onFilePathClick).toHaveBeenCalledWith(
      "apps/desktop/ui/src/features/composer/Composer.tsx",
      548,
    );
  });

  it("resolves partial relative paths via the changeset at click time", () => {
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
    fireEvent.click(screen.getByText("commands/fs.rs:138"));
    expect(onFilePathClick).toHaveBeenCalledWith(
      "apps/desktop/src-tauri/src/commands/fs.rs",
      138,
    );
  });

  it("resolves bare file names without a line number via the changeset at click time", () => {
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
    fireEvent.click(screen.getByText("MarkdownBody.tsx"));
    expect(onFilePathClick).toHaveBeenCalledWith(
      "apps/desktop/ui/src/features/conversation/MarkdownBody.tsx",
      undefined,
    );
  });

  it("matches space-separated path fragments as a whole at click time", () => {
    const onFilePathClick = vi.fn();
    const root = "D:\\work\\kodex";
    render(
      <MarkdownBody
        content={"2. `app-core / src / state.rs`：创建会话流程允许不绑定 workspace。"}
        workspaceRoot={root}
        onFilePathClick={onFilePathClick}
        candidatePaths={["crates/app-core/src/state.rs"]}
      />,
    );
    fireEvent.click(screen.getByText("app-core / src / state.rs"));
    expect(onFilePathClick).toHaveBeenCalledWith(
      "crates/app-core/src/state.rs",
      undefined,
    );
  });

  it("does not resolve a relative path to a deeper sibling that shares its segments", () => {
    // Regression: `runtime/tests.rs` used to link to
    // `.../runtime/permissions/tests.rs` because the matcher only anchored on
    // the trailing file name and treated the fragment as a loose subsequence.
    // The whole fragment must line up contiguously, so the contiguous
    // `.../runtime/tests.rs` wins and the deeper sibling never matches.
    const onFilePathClick = vi.fn();
    const root = "D:\\work\\kodex";
    render(
      <MarkdownBody
        content={"改在 `runtime/tests.rs` 里。"}
        workspaceRoot={root}
        onFilePathClick={onFilePathClick}
        candidatePaths={[
          "crates/acp-core/src/runtime/permissions/tests.rs",
          "crates/acp-core/src/runtime/tests.rs",
        ]}
      />,
    );
    fireEvent.click(screen.getByText("runtime/tests.rs"));
    expect(onFilePathClick).toHaveBeenCalledWith(
      "crates/acp-core/src/runtime/tests.rs",
      undefined,
    );
  });

  it("matches partial relative paths against the candidate pool at click time", () => {
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
    fireEvent.click(screen.getByText("commands/fs.rs:144"));
    expect(onFilePathClick).toHaveBeenCalledWith(
      "apps/desktop/src-tauri/src/commands/fs.rs",
      144,
    );
  });

  it("opens the raw span when neither the changeset nor the candidate pool matches", () => {
    const onFilePathClick = vi.fn();
    render(
      <MarkdownBody
        content={"`SomeUnrelated.tsx:12` 不在本轮上下文里。"}
        workspaceRoot="D:\\work\\kodex"
        onFilePathClick={onFilePathClick}
        changedFiles={["apps/desktop/ui/src/features/composer/Composer.tsx"]}
        candidatePaths={["crates/app-core/src/state.rs"]}
      />,
    );

    const el = screen.getByText("SomeUnrelated.tsx:12");
    expect(el).toHaveClass("md-file-path");
    fireEvent.click(el);
    // Fixed chip; the editor receives the span as written.
    expect(onFilePathClick).toHaveBeenCalledWith("SomeUnrelated.tsx", 12);
  });

  it("resolves pool matches at click time even when the file no longer exists", () => {
    // Existence is the editor's problem — rendering never probes, so a
    // deleted file still renders and resolves like any other reference.
    const onFilePathClick = vi.fn();
    render(
      <MarkdownBody
        content={"提到 `transport.rs:16` 和 `docs/relay-service-requirements.md`。"}
        workspaceRoot="D:\\work\\kodex"
        onFilePathClick={onFilePathClick}
        candidatePaths={[
          "server/src/transport.rs",
          "docs/relay-service-requirements.md",
        ]}
      />,
    );
    fireEvent.click(screen.getByText("transport.rs:16"));
    expect(onFilePathClick).toHaveBeenCalledWith("server/src/transport.rs", 16);
    fireEvent.click(screen.getByText("docs/relay-service-requirements.md"));
    expect(onFilePathClick).toHaveBeenCalledWith(
      "docs/relay-service-requirements.md",
      undefined,
    );
  });

  it("strips trailing line references from candidate pool matches", () => {
    const onFilePathClick = vi.fn();
    const root = "D:\\work\\kodex";
    render(
      <MarkdownBody
        content={"输出里提到的 `commands/fs.rs:144` 可以直接跳转。"}
        workspaceRoot={root}
        onFilePathClick={onFilePathClick}
        // Shell output candidate carries a trailing :1 line reference.
        candidatePaths={["apps/desktop/src-tauri/src/commands/fs.rs:1"]}
      />,
    );
    fireEvent.click(screen.getByText("commands/fs.rs:144"));
    expect(onFilePathClick).toHaveBeenCalledWith(
      "apps/desktop/src-tauri/src/commands/fs.rs",
      144,
    );
  });

  it("resolves against the CURRENT workspace at click time across workspace switches", () => {
    // Stateless resolution: no module caches survive a workspace switch, so
    // the click target always follows the current props.
    const rootA = "D:\\work\\repoA";
    const rootB = "D:\\work\\repoB";
    const onFilePathClick = vi.fn();

    const { rerender } = render(
      <MarkdownBody
        content={"改在 `Composer.tsx:548` 里。"}
        workspaceRoot={rootA}
        onFilePathClick={onFilePathClick}
        changedFiles={["apps/Composer.tsx"]}
      />,
    );
    fireEvent.click(screen.getByText("Composer.tsx:548"));
    expect(onFilePathClick).toHaveBeenCalledWith("apps/Composer.tsx", 548);
    onFilePathClick.mockClear();

    rerender(
      <MarkdownBody
        content={"改在 `Composer.tsx:548` 里。"}
        workspaceRoot={rootB}
        onFilePathClick={onFilePathClick}
        changedFiles={["src/Composer.tsx"]}
      />,
    );
    fireEvent.click(screen.getByText("Composer.tsx:548"));
    expect(onFilePathClick).toHaveBeenCalledWith("src/Composer.tsx", 548);
  });
});

describe("MarkdownBody math", () => {
  /** KaTeX echoes the TeX source it was handed into the MathML annotation —
   *  the only place the original formula text survives rendering, which makes
   *  it the right probe for "did a repair pass corrupt the LaTeX?". */
  function texAnnotation(container: HTMLElement, index = 0): string {
    const annotations = container.querySelectorAll(
      'annotation[encoding="application/x-tex"]',
    );
    return annotations[index]?.textContent ?? "";
  }

  it("typesets a whole-line $$…$$ as a display formula", () => {
    const { container } = render(
      <MarkdownBody
        content={
          "数学上就是：\n\n$$p = \\sum_{i=k}^{n} \\binom{n}{i} \\left(\\frac{1}{2}\\right)^n$$\n\n零假设："
        }
      />,
    );

    const display = container.querySelector(".katex-display");
    expect(display).not.toBeNull();
    // The regression this guards: `language-math` used to fall through to the
    // Prism code-block renderer and print the LaTeX source verbatim.
    expect(container.querySelector(".md-code-block")).toBeNull();
    // KaTeX emits the source back in the MathML annotation; that is where a
    // corrupted (repaired) formula would show up.
    expect(texAnnotation(container)).toContain(
      "p = \\sum_{i=k}^{n} \\binom{n}{i} \\left(\\frac{1}{2}\\right)^n",
    );
  });

  it("typesets inline math without turning it into a block", () => {
    const { container } = render(
      <MarkdownBody content={"其中 $p=0.0009$ 表示公平抛硬币的概率。"} />,
    );

    expect(container.querySelector(".katex")).not.toBeNull();
    expect(container.querySelector(".katex-display")).toBeNull();
    expect(container.textContent).toContain("表示公平抛硬币的概率");
  });

  it("leaves money and shell variables as text", () => {
    const { container } = render(
      <MarkdownBody content={"价格 $5 和 $6 元，环境变量 $HOME 与 $TEMP。"} />,
    );

    expect(container.querySelector(".katex")).toBeNull();
    expect(container.textContent).toContain("价格 $5 和 $6 元");
    expect(container.textContent).toContain("$HOME 与 $TEMP");
  });

  it("renders raw LaTeX delimiters and survives the repair passes", () => {
    const { container } = render(
      <MarkdownBody content={"\\[\\nabla f \\neq 0\\]\n\n\\(x \\notin S\\)"} />,
    );

    expect(container.querySelector(".katex-display")).not.toBeNull();
    expect(container.querySelector(".katex-error")).toBeNull();
    // `\nabla` / `\neq` / `\notin` all start with `\n`; the stringified
    // line-break repair used to split them across lines.
    expect(texAnnotation(container)).toContain("\\nabla f \\neq 0");
    expect(texAnnotation(container, 1)).toContain("x \\notin S");
  });

  it("keeps dollar signs inside code spans literal", () => {
    const { container } = render(
      <MarkdownBody content={"跑 `echo $HOME`，不是公式。"} />,
    );

    expect(container.querySelector(".katex")).toBeNull();
    expect(container.textContent).toContain("echo $HOME");
  });
});

describe("resolveClickableFilePath", () => {
  const root = "D:\\work\\kodex";

  it("resolves relative paths with line and column", () => {
    expect(resolveClickableFilePath("crates/acp-core/src/mapping.rs:391", root)).toMatchObject({
      path: "crates/acp-core/src/mapping.rs",
      lineNumber: 391,
    });
    expect(resolveClickableFilePath("src/lib.rs:10:5", root)).toMatchObject({
      path: "src/lib.rs",
      lineNumber: 10,
    });
  });

  it("resolves diff-prefixed and absolute paths", () => {
    expect(resolveClickableFilePath("a/crates/x.rs:3", root)).toMatchObject({
      path: "crates/x.rs",
      lineNumber: 3,
    });
    expect(resolveClickableFilePath("D:\\work\\kodex\\src\\main.rs:8", root)).toMatchObject({
      path: "src/main.rs",
      lineNumber: 8,
    });
    expect(resolveClickableFilePath("/home/user/repo/src/main.rs", "/home/user/repo")).toMatchObject({
      path: "src/main.rs",
      lineNumber: undefined,
    });
    expect(resolveClickableFilePath("/home/user/other/src/main.rs", root)).toBeNull();
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
      path: "app-core/state.rs",
      lineNumber: undefined,
      matchTail: "app-core/state.rs",
    });
    expect(resolveClickableFilePath("crates / app-core / src / state.rs:12", root)).toEqual({
      path: "crates/app-core/src/state.rs",
      lineNumber: 12,
      matchTail: "crates/app-core/src/state.rs",
    });
    expect(resolveClickableFilePath("foo bar.rs", root)).toBeNull();
  });

  it("requires a workspace root for relative paths", () => {
    expect(resolveClickableFilePath("crates/x.rs:1")).toBeNull();
  });

  it("accepts compound filename extensions", () => {
    expect(
      resolveClickableFilePath(
        "apps/desktop/ui/src/features/conversation/MarkdownBody.test.tsx",
        root,
      ),
    ).toMatchObject({
      path: "apps/desktop/ui/src/features/conversation/MarkdownBody.test.tsx",
    });
    expect(resolveClickableFilePath("MarkdownBody.test.tsx", root)).toMatchObject({
      path: "MarkdownBody.test.tsx",
    });
    expect(resolveClickableFilePath("foo..tsx", root)).toBeNull();
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
  it("matches fragments that are a contiguous trailing run of the candidate", () => {
    // Leading directories may be dropped (abbreviated relative paths).
    expect(pathMatchesFragment("apps/desktop/src-tauri/src/commands/fs.rs", "commands/fs.rs")).toBe(true);
    expect(pathMatchesFragment("crates/app-core/src/state.rs", "app-core/src/state.rs")).toBe(true);
    expect(pathMatchesFragment("crates/app-core/src/state.rs", "state.rs")).toBe(true);
    expect(pathMatchesFragment("apps/desktop/ui/src/features/composer/Composer.tsx", "Composer.tsx")).toBe(true);
    expect(pathMatchesFragment("apps/desktop/src-tauri/src/commands/fs.rs", "commands/fs.rs:12")).toBe(true);
  });

  it("rejects fragments that skip intermediate segments", () => {
    // `app-core/state.rs` is not a contiguous suffix of
    // `crates/app-core/src/state.rs` — `src/` sits between `app-core` and
    // `state.rs`, so it denotes a different file.
    expect(pathMatchesFragment("crates/app-core/src/state.rs", "app-core/state.rs")).toBe(false);
    expect(pathMatchesFragment("crates/app-core/src/state.rs", "app-core/state.rs:12")).toBe(false);
  });

  it("rejects out-of-order or foreign fragments", () => {
    expect(pathMatchesFragment("crates/app-core/src/state.rs", "state.rs/app-core")).toBe(false);
    expect(pathMatchesFragment("crates/app-core/src/state.rs", "other/state.rs")).toBe(false);
    expect(pathMatchesFragment("crates/app-core/src/state.rs", "app-core/lib.rs")).toBe(false);
  });

  it("does not match a deeper sibling that merely shares the fragment's segments", () => {
    // Regression for `runtime/tests.rs` linking to
    // `.../runtime/permissions/tests.rs`.
    expect(pathMatchesFragment("crates/acp-core/src/runtime/permissions/tests.rs", "runtime/tests.rs")).toBe(false);
    expect(pathMatchesFragment("crates/acp-core/src/runtime/tests.rs", "runtime/tests.rs")).toBe(true);
  });
});
