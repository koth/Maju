import { useEffect } from "react";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { EditorView } from "./EditorView";
import { getModelBaseVersion } from "./monaco-model-registry";
import {
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
  private version = 1;

  constructor(value: string) {
    this.value = value;
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

  isDisposed() {
    return false;
  }

  findMatches() {
    return [];
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
    getModel(uri: { toString: () => string }) {
      return modelStore.get(uri.toString()) ?? null;
    },
    createModel(content: string, _language: string, uri: { toString: () => string }) {
      const model = new FakeModel(content);
      modelStore.set(uri.toString(), model);
      return model;
    },
  },
};

const fakeEditor = {
  setModel: vi.fn(),
  getModel: () => null,
  hasTextFocus: () => true,
  onDidDispose: vi.fn(),
  saveViewState: vi.fn(() => null),
  restoreViewState: vi.fn(),
  revealLineNearTop: vi.fn(),
  setPosition: vi.fn(),
  focus: vi.fn(),
  createDecorationsCollection: () => ({ clear: vi.fn() }),
};

vi.mock("@monaco-editor/react", () => ({
  default: function MockMonacoEditor(props: {
    value: string;
    beforeMount: (monaco: typeof fakeMonaco) => void;
    onMount: (editor: typeof fakeEditor, monaco: typeof fakeMonaco) => void;
    onChange: (value?: string) => void;
  }) {
    useEffect(() => {
      props.beforeMount(fakeMonaco);
      props.onMount(fakeEditor, fakeMonaco);
    }, []);
    return (
      <textarea
        aria-label="mock editor"
        value={props.value}
        onChange={(event) => props.onChange(event.currentTarget.value)}
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
  });

  beforeEach(() => {
    vi.clearAllMocks();
    modelStore.clear();
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
});
