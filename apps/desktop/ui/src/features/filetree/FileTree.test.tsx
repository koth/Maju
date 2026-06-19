import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { confirm } from "@tauri-apps/plugin-dialog";
import { fsDeleteFile, fsListDir } from "../../lib/tauri";
import type { FileEntry } from "../../types";
import { FileTree } from "./FileTree";

vi.mock("@tauri-apps/plugin-dialog", () => ({
  confirm: vi.fn(),
}));

vi.mock("../../lib/tauri", async () => {
  const actual = await vi.importActual<typeof import("../../lib/tauri")>(
    "../../lib/tauri",
  );
  return {
    ...actual,
    fsDeleteFile: vi.fn(),
    fsListDir: vi.fn(),
    fsRename: vi.fn(),
    fsReveal: vi.fn(),
  };
});

const rootEntries: FileEntry[] = [
  { name: "src", kind: "Directory", path: "src" },
  { name: "notes.md", kind: "File", path: "notes.md" },
];

describe("FileTree", () => {
  beforeEach(() => {
    vi.mocked(fsListDir).mockImplementation(async (path: string) =>
      path === "" ? rootEntries : [],
    );
    vi.mocked(fsDeleteFile).mockResolvedValue(undefined);
    vi.mocked(confirm).mockResolvedValue(true);
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("deletes a file from the context menu after confirmation", async () => {
    render(<FileTree workspaceRoot="/repo" onFileOpen={vi.fn()} />);

    fireEvent.contextMenu(await screen.findByText("notes.md"), {
      clientX: 12,
      clientY: 12,
    });

    fireEvent.click(screen.getByRole("menuitem", { name: "删除文件" }));

    await waitFor(() => expect(fsDeleteFile).toHaveBeenCalledWith("notes.md"));
    expect(confirm).toHaveBeenCalledWith("确定删除文件 notes.md？");
    expect(fsListDir).toHaveBeenLastCalledWith("");
  });

  it("does not offer file deletion for directories", async () => {
    render(<FileTree workspaceRoot="/repo" onFileOpen={vi.fn()} />);

    fireEvent.contextMenu(await screen.findByText("src"), {
      clientX: 12,
      clientY: 12,
    });

    expect(screen.queryByRole("menuitem", { name: "删除文件" })).toBeNull();
  });

  it("omits context action when references are unavailable", async () => {
    render(
      <FileTree
        workspaceRoot="/repo"
        onFileOpen={vi.fn()}
        onAddComposerReference={vi.fn()}
        composerReferenceEnabled={false}
      />,
    );

    fireEvent.contextMenu(await screen.findByText("notes.md"), {
      clientX: 12,
      clientY: 12,
    });

    expect(screen.queryByRole("menuitem", { name: "发送到上下文" })).toBeNull();
    expect(screen.queryByRole("menuitem", { name: /Composer/ })).toBeNull();
  });

  it("sends a file to context from the context menu", async () => {
    const onAddReference = vi.fn();
    render(
      <FileTree
        workspaceRoot="/repo"
        onFileOpen={vi.fn()}
        onAddComposerReference={onAddReference}
        composerReferenceEnabled
      />,
    );

    fireEvent.contextMenu(await screen.findByText("notes.md"), {
      clientX: 12,
      clientY: 12,
    });
    fireEvent.click(screen.getByRole("menuitem", { name: "发送到上下文" }));

    expect(onAddReference).toHaveBeenCalledWith("notes.md");
  });

  it("uses inline search chrome without the panel header", async () => {
    render(<FileTree workspaceRoot="/repo" onFileOpen={vi.fn()} variant="inline" />);

    const filter = screen.getByPlaceholderText("筛选文件...");
    expect(screen.queryByText("所有文件")).toBeNull();
    expect(await screen.findByText("notes.md")).toBeTruthy();

    fireEvent.change(filter, { target: { value: "src" } });

    expect(screen.getByText("src")).toBeTruthy();
    expect(screen.queryByText("notes.md")).toBeNull();
  });
});
