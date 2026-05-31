import { useEffect } from "react";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { EditorView } from "./EditorView";
import { disposeAllModels, getModelBaseVersion } from "./monaco-model-registry";
import {
  editorLspOpenDocument,
  editorOpenFile,
  editorSaveFile,
} from "../../lib/tauri";

const modelStore = new Map<string, FakeModel>();

class FakeRange {
  constructor(
    public startLineNumber: number,
    public startColumn: number,
    public endLineNumber: number,
    public endColumn: number,
  ) {}
}

class FakeModel {
  private value: string;
  private language: string;
  private version = 1;
  private disposed = false;

  constructor(value: string, language = "plaintext") {
    this.value = value;
    this.language = language;
  }

  getValue() {
    return this.value;
  }

  setValue(value: string) {
    this.value = value;
    this.version += 1;
  }

  getVersionId() {
    return this.version;
  }

  getLanguageId() {
    return this.language;
  }

  setLanguage(language: string) {
    this.language = language;
  }

  isDisposed() {
    return this.disposed;
  }

  dispose() {
    this.disposed = true;
  }

  findMatches() {
    return [];
  }

  getValueInRange() {
    return this.value;
  }
}

const fakeMonaco = {
  Uri: {
    parse(uri: string) {
      return { toString: () => uri, path: uri.replace("file:///", "/") };
    },
  },
  Range: FakeRange,
  MarkerSeverity: { Error: 8, Warning: 4, Info: 2, Hint: 1 },
  languages: {
    CompletionItemKind: { Text: 1 },
    SymbolKind: { Variable: 13 },
    registerHoverProvider: vi.fn(),
    registerCompletionItemProvider: vi.fn(),
    registerDefinitionProvider: vi.fn(),
    registerReferenceProvider: vi.fn(),
    registerDocumentSymbolProvider: vi.fn(),
    registerDocumentFormattingEditProvider: vi.fn(),
    registerDocumentSemanticTokensProvider: vi.fn(),
  },
  editor: {
    defineTheme: vi.fn(),
    setModelMarkers: vi.fn(),
    setModelLanguage: vi.fn((model: FakeModel, language: string) => {
      model.setLanguage(language);
    }),
    getModel(uri: { toString: () => string }) {
      return modelStore.get(uri.toString()) ?? null;
    },
    createModel(content: string, language: string, uri: { toString: () => string }) {
      const model = new FakeModel(content, language);
      modelStore.set(uri.toString(), model);
      return model;
    },
  },
};

let currentModel: FakeModel | null = null;
let currentSelection = {
  isEmpty: () => true,
  startLineNumber: 1,
  startColumn: 1,
  endLineNumber: 1,
  endColumn: 1,
};
let selectionListener: (() => void) | null = null;

interface FakeEditorAction {
  id: string;
  label: string;
  precondition?: string;
  contextMenuGroupId?: string;
  run: () => void;
}

function createFakeEditor() {
  return {
    setModel: vi.fn((model: FakeModel) => {
      currentModel = model;
    }),
    getModel: () => currentModel,
    getSelection: () => currentSelection,
    hasTextFocus: () => true,
    onDidDispose: vi.fn(),
    onDidChangeCursorSelection: vi.fn((listener: () => void) => {
      selectionListener = listener;
      return {
        dispose: vi.fn(() => {
          if (selectionListener === listener) {
            selectionListener = null;
          }
        }),
      };
    }),
    addAction: vi.fn((_action: FakeEditorAction) => ({ dispose: vi.fn() })),
    saveViewState: vi.fn(() => null),
    restoreViewState: vi.fn(),
    revealLineNearTop: vi.fn(),
    setPosition: vi.fn(),
    focus: vi.fn(),
    layout: vi.fn(),
    createDecorationsCollection: () => ({ clear: vi.fn() }),
  };
}

let fakeEditor = createFakeEditor();

function setFakeSelection(selection: typeof currentSelection) {
  currentSelection = selection;
  selectionListener?.();
}

vi.mock("@monaco-editor/react", () => ({
  default: function MockMonacoEditor(props: {
    value: string;
    keepCurrentModel?: boolean;
    beforeMount: (monaco: typeof fakeMonaco) => void;
    onMount: (editor: typeof fakeEditor, monaco: typeof fakeMonaco) => void;
    onChange: (value?: string) => void;
  }) {
    useEffect(() => {
      props.beforeMount(fakeMonaco);
      props.onMount(fakeEditor, fakeMonaco);
      return () => {
        if (!props.keepCurrentModel) {
          fakeEditor.getModel()?.dispose();
        }
        currentModel = null;
      };
    }, []);
    return (
      <textarea
        aria-label="mock editor"
        value={props.value}
        onChange={(event) => {
          currentModel?.setValue(event.currentTarget.value);
          props.onChange(event.currentTarget.value);
        }}
      />
    );
  },
}));

vi.mock("../conversation/MarkdownBody", () => ({
  default: function MockMarkdownBody({ content }: { content: string }) {
    const title = content.match(/^#\s+(.+)$/m)?.[1] ?? content;
    return <h1>{title}</h1>;
  },
}));

vi.mock("./textmate-engine", () => ({
  initTextMate: vi.fn().mockResolvedValue(undefined),
  registerTextMateLanguage: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("../../lib/tauri", async () => {
  const actual = await vi.importActual<typeof import("../../lib/tauri")>("../../lib/tauri");
  return {
    ...actual,
    editorOpenFile: vi.fn(),
    editorSaveFile: vi.fn(),
    editorLspOpenDocument: vi.fn().mockResolvedValue({
      languageId: "typescript",
      configured: true,
      enabled: true,
      available: false,
      running: false,
      message: "missing",
    }),
    editorLspChangeDocument: vi.fn().mockResolvedValue(2),
    editorLspSaveDocument: vi.fn().mockResolvedValue(undefined),
    editorLspCloseDocument: vi.fn().mockResolvedValue(undefined),
    editorLspGetDiagnostics: vi.fn().mockResolvedValue([]),
    editorLspRequest: vi.fn().mockResolvedValue(null),
  };
});

const version = { content_hash: "hash", modified_ms: 1, size: 4 };

describe("EditorView editable state", () => {
  afterEach(() => {
    cleanup();
    disposeAllModels();
  });

  beforeEach(() => {
    vi.clearAllMocks();
    modelStore.clear();
    currentModel = null;
    currentSelection = {
      isEmpty: () => true,
      startLineNumber: 1,
      startColumn: 1,
      endLineNumber: 1,
      endColumn: 1,
    };
    selectionListener = null;
    fakeEditor = createFakeEditor();
    vi.mocked(editorOpenFile).mockResolvedValue({
      path: "src/main.ts",
      content: "base",
      version,
    });
    vi.mocked(editorSaveFile).mockResolvedValue({
      path: "src/main.ts",
      content: "next",
      version: { ...version, content_hash: "next" },
    });
  });

  it("tracks dirty state and saves with Ctrl+S", async () => {
    const onDirtyChange = vi.fn();
    const onSaved = vi.fn();
    render(
      <EditorView
        path="src/main.ts"
        appTheme="kodex_dark"
        onDirtyChange={onDirtyChange}
        onSaved={onSaved}
      />,
    );

    const editor = await screen.findByLabelText("mock editor");
    fireEvent.change(editor, { target: { value: "next" } });

    await waitFor(() => expect(onDirtyChange).toHaveBeenLastCalledWith("src/main.ts", true));
    fireEvent.keyDown(window, { key: "s", ctrlKey: true });

    await waitFor(() => expect(editorSaveFile).toHaveBeenCalledWith("src/main.ts", "next", version, false));
    await waitFor(() => expect(onSaved).toHaveBeenCalled());
    expect(onDirtyChange).toHaveBeenLastCalledWith("src/main.ts", false);
  });

  it("keeps the model base version available for tab close saves", async () => {
    render(<EditorView path="src/main.ts" appTheme="kodex_dark" />);

    await screen.findByLabelText("mock editor");

    await waitFor(() => expect(getModelBaseVersion("src/main.ts")).toEqual(version));
  });

  it("adds selected editor lines to the composer from Monaco's context menu", async () => {
    const onAddComposerReference = vi.fn();
    render(
      <EditorView
        path="src/main.ts"
        appTheme="kodex_dark"
        onAddComposerReference={onAddComposerReference}
      />,
    );

    await screen.findByLabelText("mock editor");
    const action = fakeEditor.addAction.mock.calls.find(
      ([registeredAction]) => registeredAction.id === "kodex.send-selection-to-composer",
    )?.[0];

    expect(action).toMatchObject({
      label: "发送选区到 Composer",
      precondition: "editorHasSelection",
      contextMenuGroupId: "navigation",
    });

    setFakeSelection({
      isEmpty: () => false,
      startLineNumber: 4,
      startColumn: 1,
      endLineNumber: 9,
      endColumn: 1,
    });

    expect(screen.queryByRole("button", { name: "引用选区" })).toBeNull();
    action?.run();

    expect(onAddComposerReference).toHaveBeenCalledWith({
      path: "src/main.ts",
      startLine: 4,
      endLine: 8,
    });
  });

  it("does not reopen the same file when parent callbacks refresh", async () => {
    const firstDirtyChange = vi.fn();
    const { rerender } = render(
      <EditorView
        path="src/main.ts"
        appTheme="kodex_dark"
        onDirtyChange={firstDirtyChange}
      />,
    );

    await screen.findByLabelText("mock editor");
    expect(editorOpenFile).toHaveBeenCalledTimes(1);

    rerender(
      <EditorView
        path="src/main.ts"
        appTheme="kodex_dark"
        onDirtyChange={vi.fn()}
      />,
    );

    await new Promise((resolve) => window.setTimeout(resolve, 10));
    expect(editorOpenFile).toHaveBeenCalledTimes(1);
  });

  it("keeps unsaved edits when switching away from the editor and reopening the same tab", async () => {
    const { unmount } = render(<EditorView path="src/main.ts" appTheme="kodex_dark" />);

    const editor = await screen.findByLabelText("mock editor");
    fireEvent.change(editor, { target: { value: "dirty local edit" } });

    await waitFor(() => expect(editor).toHaveValue("dirty local edit"));
    unmount();

    render(<EditorView path="src/main.ts" appTheme="kodex_dark" />);

    await waitFor(() => {
      expect(screen.getByLabelText("mock editor")).toHaveValue("dirty local edit");
    });
  });

  it("keeps each file's dirty model when switching between file tabs", async () => {
    vi.mocked(editorOpenFile).mockImplementation(async (filePath: string) => ({
      path: filePath,
      content: filePath === "src/other.ts" ? "other base" : "base",
      version,
      kind: "text",
    }));
    const { rerender } = render(<EditorView path="src/main.ts" appTheme="kodex_dark" />);

    const editor = await screen.findByLabelText("mock editor");
    fireEvent.change(editor, { target: { value: "dirty main edit" } });
    await waitFor(() => expect(editor).toHaveValue("dirty main edit"));

    rerender(<EditorView path="src/other.ts" appTheme="kodex_dark" />);
    await waitFor(() => expect(screen.getByLabelText("mock editor")).toHaveValue("other base"));

    rerender(<EditorView path="src/main.ts" appTheme="kodex_dark" />);
    await waitFor(() => expect(screen.getByLabelText("mock editor")).toHaveValue("dirty main edit"));
  });

  it("does not show an unavailable badge for unsupported languages", async () => {
    const tauri = await import("../../lib/tauri");
    vi.mocked(tauri.editorLspOpenDocument).mockResolvedValueOnce({
      languageId: "plaintext",
      configured: false,
      enabled: false,
      available: false,
      running: false,
      message: null,
    });
    vi.mocked(editorOpenFile).mockResolvedValueOnce({
      path: "README.unknown",
      content: "base",
      version,
    });

    render(<EditorView path="README.unknown" appTheme="kodex_dark" />);

    await screen.findByLabelText("mock editor");
    await waitFor(() => expect(tauri.editorLspOpenDocument).toHaveBeenCalled());
    expect(screen.queryByText("LSP 需配置")).not.toBeInTheDocument();
  });

  it("shows a settings affordance only for enabled failed language servers", async () => {
    const tauri = await import("../../lib/tauri");
    render(<EditorView path="src/main.ts" appTheme="kodex_dark" />);

    expect(await screen.findByText("LSP 需配置")).toBeInTheDocument();
    fireEvent.click(screen.getByText("LSP 需配置"));

    vi.mocked(tauri.editorLspOpenDocument).mockResolvedValueOnce({
      languageId: "typescript",
      configured: true,
      enabled: false,
      available: false,
      running: false,
      message: "Language server disabled",
    });
    cleanup();
    render(<EditorView path="src/main.ts" appTheme="kodex_dark" />);

    await waitFor(() => expect(tauri.editorLspOpenDocument).toHaveBeenCalled());
    expect(screen.queryByText("LSP 需配置")).not.toBeInTheDocument();

    vi.mocked(tauri.editorLspOpenDocument).mockResolvedValueOnce({
      languageId: "typescript",
      configured: true,
      enabled: true,
      available: true,
      running: true,
      message: null,
    });
    cleanup();
    render(<EditorView path="src/main.ts" appTheme="kodex_dark" />);

    expect(await screen.findByText("LSP 已连接")).toBeInTheDocument();
  });

  it("registers syntax highlighting again when switching to TSX after mount", async () => {
    const textmate = await import("./textmate-engine");
    const { rerender } = render(<EditorView path="src/main.ts" appTheme="kodex_dark" />);

    await screen.findByLabelText("mock editor");
    vi.mocked(editorOpenFile).mockResolvedValueOnce({
      path: "src/App.tsx",
      content: "export function App() { return <div />; }",
      version,
    });

    rerender(<EditorView path="src/App.tsx" appTheme="kodex_dark" />);

    await waitFor(() => {
      expect(textmate.registerTextMateLanguage).toHaveBeenCalledWith(fakeMonaco, "typescriptreact");
    });
  });

  it.each([
    ["C++ headers", "include/channel.hpp", "cpp", "class Channel {};"],
    ["C# files", "src/Program.cs", "csharp", "public class Program {}"],
    ["Lean files", "Proof.lean", "lean", "theorem id (p : Prop) : p -> p := by intro h; exact h"],
  ])("maps %s to the expected grammar instead of plaintext", async (_label, path, language, content) => {
    const textmate = await import("./textmate-engine");
    vi.mocked(editorOpenFile).mockResolvedValueOnce({
      path,
      content,
      version,
    });

    render(<EditorView path={path} appTheme="kodex_dark" />);

    await screen.findByLabelText("mock editor");
    await waitFor(() => {
      expect(textmate.registerTextMateLanguage).toHaveBeenCalledWith(fakeMonaco, language);
    });
    expect(editorLspOpenDocument).toHaveBeenCalledWith(path, language, content);
    expect(currentModel?.getLanguageId()).toBe(language);
  });

  it("opens image files as a preview instead of a text editor", async () => {
    vi.mocked(editorOpenFile).mockResolvedValueOnce({
      path: "assets/logo.png",
      content: "data:image/png;base64,iVBORw0KGgo=",
      version,
      kind: "image",
      mime_type: "image/png",
    });

    render(<EditorView path="assets/logo.png" appTheme="kodex_dark" />);

    const image = await screen.findByAltText("assets/logo.png");
    expect(image).toHaveAttribute("src", "data:image/png;base64,iVBORw0KGgo=");
    expect(screen.queryByLabelText("mock editor")).not.toBeInTheDocument();
  });

  it("renders markdown by default and opens source editing on demand", async () => {
    vi.mocked(editorOpenFile).mockResolvedValueOnce({
      path: "README.md",
      content: "# Title\n\nBody",
      version,
      kind: "text",
    });

    render(<EditorView path="README.md" appTheme="kodex_dark" />);

    expect(await screen.findByRole("heading", { name: "Title" })).toBeInTheDocument();
    expect(screen.queryByLabelText("mock editor")).not.toBeInTheDocument();

    fireEvent.click(screen.getByText("编辑原文"));

    expect(await screen.findByLabelText("mock editor")).toBeInTheDocument();
  });

  it("shows file context and an explicit exit control in fullscreen", async () => {
    vi.mocked(editorOpenFile).mockResolvedValueOnce({
      path: "README.md",
      content: "# Title\n\nBody",
      version,
      kind: "text",
    });

    render(<EditorView path="README.md" appTheme="kodex_dark" />);

    expect(await screen.findByRole("heading", { name: "Title" })).toBeInTheDocument();
    fireEvent.click(screen.getByLabelText("全屏编辑"));

    expect(screen.getByText("全屏编辑")).toBeInTheDocument();
    expect(screen.getByTitle("README.md")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "退出全屏" })).toBeInTheDocument();
    expect(screen.getByText("Esc")).toBeInTheDocument();

    fireEvent.keyDown(window, { key: "Escape" });

    expect(screen.queryByRole("button", { name: "退出全屏" })).not.toBeInTheDocument();
  });

  it("omits editor fullscreen control in breadcrumb toolbar mode", async () => {
    render(
      <EditorView
        path="src/main.ts"
        appTheme="kodex_dark"
        toolbarMode="breadcrumbs"
        workspaceName="Kodex"
        onToggleFileTree={vi.fn()}
      />,
    );

    await screen.findByLabelText("mock editor");

    expect(screen.getByLabelText("文件路径")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "显示文件树" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "全屏编辑" })).not.toBeInTheDocument();
  });
});
