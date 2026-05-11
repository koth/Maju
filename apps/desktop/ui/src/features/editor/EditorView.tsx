import { useState, useEffect, useRef, useCallback, useMemo, lazy, Suspense } from "react";
import type { editor as monacoEditor } from "monaco-editor";
import type { AppTheme, EditorFileSnapshot, LspDiagnostic, LspServerStatus } from "../../types";
import {
  editorLspChangeDocument,
  editorLspCloseDocument,
  editorLspGetDiagnostics,
  editorLspOpenDocument,
  editorLspSaveDocument,
  editorOpenFile,
  editorSaveFile,
} from "../../lib/tauri";
import { saveViewState, restoreViewState } from "./monaco-view-state";
import { monacoThemeForAppTheme, registerKodexThemes } from "./monaco-theme";
import { registerLspProviders } from "./lsp-providers";
import { initTextMate, registerTextMateLanguage } from "./textmate-engine";
import {
  getModelValue,
  getOrCreateModel,
  isModelDirty,
  setModelContent,
  updateModelBase,
  updateModelBaseVersion,
} from "./monaco-model-registry";
import "./EditorView.css";

const MonacoEditor = lazy(() =>
  import("@monaco-editor/react").then((mod) => ({ default: mod.default })),
);
const MarkdownBody = lazy(() => import("../conversation/MarkdownBody"));

let textmateInitStarted = false;

const LANG_MAP: Record<string, string> = {
  ts: "typescript",
  tsx: "typescriptreact",
  js: "javascript",
  cjs: "javascript",
  mjs: "javascript",
  jsx: "javascriptreact",
  rs: "rust",
  json: "json",
  md: "markdown",
  css: "css",
  html: "html",
  toml: "toml",
  yaml: "yaml",
  yml: "yaml",
  py: "python",
};

interface Props {
  path: string;
  lineNumber?: number;
  searchQuery?: string;
  navToken?: number;
  appTheme: AppTheme;
  onDirtyChange?: (path: string, dirty: boolean) => void;
  onSaved?: () => void;
}

export function EditorView({ path, lineNumber, searchQuery, navToken, appTheme, onDirtyChange, onSaved }: Props) {
  const [snapshot, setSnapshot] = useState<EditorFileSnapshot | null>(null);
  const [content, setContent] = useState<string>("");
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [conflict, setConflict] = useState<string | null>(null);
  const [lspStatus, setLspStatus] = useState<LspServerStatus | null>(null);
  const [sourceMode, setSourceMode] = useState(false);
  const editorRef = useRef<monacoEditor.IStandaloneCodeEditor | null>(null);
  const monacoRef = useRef<typeof import("monaco-editor") | null>(null);
  const prevPathRef = useRef<string | null>(null);
  const decorationsRef = useRef<monacoEditor.IEditorDecorationsCollection | null>(null);
  const lspChangeTimerRef = useRef<number | null>(null);
  const editorDisposedRef = useRef(false);
  const openRequestSeqRef = useRef(0);

  const language = useMemo(() => {
    const ext = path.split(".").pop()?.toLowerCase() ?? "";
    return LANG_MAP[ext] ?? "plaintext";
  }, [path]);
  const fileKind = snapshot?.kind ?? "text";
  const isTextFile = fileKind === "text";
  const isImageFile = fileKind === "image";
  const isRenderableDocument = isTextFile && (language === "markdown" || language === "html");
  const isSourceMode = isTextFile && (!isRenderableDocument || sourceMode);

  const safeSetEditorModel = useCallback((model: monacoEditor.ITextModel) => {
    const editor = editorRef.current;
    if (!editor || editorDisposedRef.current) return;
    try {
      if (editor.getModel() !== model) {
        editor.setModel(model);
      }
    } catch (error) {
      if (String(error).includes("disposed")) {
        editorRef.current = null;
        editorDisposedRef.current = true;
        return;
      }
      throw error;
    }
  }, []);

  useEffect(() => {
    if (prevPathRef.current && editorRef.current && !editorDisposedRef.current) {
      saveViewState(prevPathRef.current, editorRef.current);
    }
    prevPathRef.current = path;
    const requestSeq = openRequestSeqRef.current + 1;
    openRequestSeqRef.current = requestSeq;
    let cancelled = false;

    setSnapshot(null);
    setContent("");
    setDirty(false);
    setError(null);
    setConflict(null);
    setLspStatus(null);
    setSourceMode(false);
    editorOpenFile(path)
      .then((nextSnapshot) => {
        if (cancelled || requestSeq !== openRequestSeqRef.current) return;
        const isNextTextFile = (nextSnapshot.kind ?? "text") === "text";
        const cachedContent = isNextTextFile ? getModelValue(path) : null;
        const hasDirtyModel = isNextTextFile && isModelDirty(path);
        const nextContent = hasDirtyModel && cachedContent !== null ? cachedContent : nextSnapshot.content;
        setSnapshot(nextSnapshot);
        setContent(nextContent);
        if (isNextTextFile && monacoRef.current) {
          const model = getOrCreateModel(monacoRef.current, path, nextContent);
          safeSetEditorModel(model);
        }
        if (isNextTextFile && !hasDirtyModel) {
          updateModelBase(path, nextSnapshot.content);
        }
        if (isNextTextFile) {
          updateModelBaseVersion(path, nextSnapshot.version);
        }
        setDirty(hasDirtyModel);
        onDirtyChange?.(path, hasDirtyModel);
      })
      .catch((e) => {
        if (!cancelled && requestSeq === openRequestSeqRef.current) {
          setError(String(e));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [path, onDirtyChange, safeSetEditorModel]);

  useEffect(() => {
    if (!snapshot || !isSourceMode) return;
    let disposed = false;
    const modelContent = getModelValue(path) ?? snapshot.content;
    editorLspOpenDocument(path, language, modelContent)
      .then((status) => {
        if (!disposed) setLspStatus(status);
      })
      .catch(() => {
        if (!disposed) {
          setLspStatus({
            languageId: language,
            configured: false,
            enabled: false,
            available: false,
            running: false,
            message: null,
          });
        }
      });

    return () => {
      disposed = true;
      if (lspChangeTimerRef.current !== null) {
        window.clearTimeout(lspChangeTimerRef.current);
        lspChangeTimerRef.current = null;
      }
      editorLspCloseDocument(path, language).catch(() => {});
    };
  }, [isSourceMode, language, path, snapshot?.path]);

  useEffect(() => {
    if (!lspStatus?.running) return;
    const updateMarkers = async () => {
      const diagnostics = await editorLspGetDiagnostics(path, language).catch(() => []);
      const monaco = monacoRef.current;
      const model = editorDisposedRef.current ? null : editorRef.current?.getModel();
      if (!monaco || !model) return;
      monaco.editor.setModelMarkers(
        model,
        "lsp",
        diagnostics.map((diagnostic) => diagnosticToMarker(monaco, diagnostic)),
      );
    };
    updateMarkers();
    const interval = window.setInterval(updateMarkers, 1200);
    return () => {
      window.clearInterval(interval);
      const monaco = monacoRef.current;
      const model = editorDisposedRef.current ? null : editorRef.current?.getModel();
      if (monaco && model) {
        monaco.editor.setModelMarkers(model, "lsp", []);
      }
    };
  }, [language, lspStatus?.running, path]);

  // When lineNumber or searchQuery change for the same file (e.g. clicking another result)
  useEffect(() => {
    const editor = editorRef.current;
    if (!isSourceMode || !editor || editorDisposedRef.current) return;

    if (lineNumber) {
      editor.revealLineNearTop(lineNumber, 0);
      editor.setPosition({ lineNumber, column: 1 });
      editor.focus();
    }

    if (searchQuery) {
      const model = editor.getModel();
      if (model) {
        const matches = model.findMatches(searchQuery, false, false, false, null, false);
        const decorations = matches.map((m) => ({
          range: m.range,
          options: {
            className: "search-highlight",
            overviewRuler: {
              color: "rgba(255, 80, 80, 0.6)",
              position: 1 as monacoEditor.OverviewRulerLane,
            },
          },
        }));
        if (decorationsRef.current) {
          decorationsRef.current.clear();
        }
        decorationsRef.current = editor.createDecorationsCollection(decorations);
      }
    } else if (decorationsRef.current) {
      decorationsRef.current.clear();
      decorationsRef.current = null;
    }
  }, [isSourceMode, navToken]);

  const handleEditorMount = useCallback(
    (editor: monacoEditor.IStandaloneCodeEditor, monacoInstance: typeof import("monaco-editor")) => {
      editorRef.current = editor;
      monacoRef.current = monacoInstance;
      editorDisposedRef.current = false;
      editor.onDidDispose?.(() => {
        if (editorRef.current === editor) {
          editorRef.current = null;
        }
        editorDisposedRef.current = true;
      });
      const model = getOrCreateModel(monacoInstance, path, content);
      updateModelBaseVersion(path, snapshot?.version);
      safeSetEditorModel(model);

      // Only restore previous view state if we don't have a specific line to jump to
      if (!lineNumber) {
        restoreViewState(path, editor);
      }

      // Delay navigation to ensure Monaco has fully laid out content
      if (lineNumber || searchQuery) {
        requestAnimationFrame(() => {
          if (editorDisposedRef.current || editorRef.current !== editor) return;
          if (lineNumber) {
            editor.revealLineNearTop(lineNumber, 0);
            editor.setPosition({ lineNumber, column: 1 });
            editor.focus();
          }

          if (searchQuery) {
            const model = editor.getModel();
            if (model) {
              const matches = model.findMatches(searchQuery, false, false, false, null, false);
              const decorations = matches.map((m) => ({
                range: m.range,
                options: {
                  className: "search-highlight",
                  overviewRuler: {
                    color: "rgba(255, 80, 80, 0.6)",
                    position: 1 as monacoEditor.OverviewRulerLane,
                  },
                },
              }));
              if (decorationsRef.current) {
                decorationsRef.current.clear();
              }
              decorationsRef.current = editor.createDecorationsCollection(decorations);
            }
          }
        });
      }
    },
    [path, content, lineNumber, searchQuery, safeSetEditorModel, snapshot?.version],
  );

  useEffect(() => {
    const monaco = monacoRef.current;
    const editor = editorRef.current;
    if (!snapshot || !isSourceMode || !monaco || !editor || editorDisposedRef.current) return;

    const model = getOrCreateModel(monaco, path, content);
    safeSetEditorModel(model);
    if (!dirty && model.getValue() !== content) {
      setModelContent(path, content);
    }
  }, [content, dirty, isSourceMode, path, snapshot]);

  const handleBeforeMount = useCallback(
    (monaco: typeof import("monaco-editor")) => {
      registerKodexThemes(monaco);
      if (!textmateInitStarted) {
        textmateInitStarted = true;
        initTextMate().catch(() => {});
      }
      registerTextMateLanguage(monaco, language).catch(() => {});
      registerLspProviders(monaco, language);
    },
    [language],
  );

  useEffect(() => {
    const monaco = monacoRef.current;
    if (!isSourceMode || !monaco) return;

    registerKodexThemes(monaco);
    if (!textmateInitStarted) {
      textmateInitStarted = true;
      initTextMate().catch(() => {});
    }

    let disposed = false;
    registerTextMateLanguage(monaco, language)
      .then(() => {
        if (disposed) return;
        const model = editorDisposedRef.current ? null : editorRef.current?.getModel();
        if (model && typeof model.getLanguageId === "function") {
          monaco.editor.setModelLanguage(model, language);
        }
      })
      .catch(() => {});
    registerLspProviders(monaco, language);

    return () => {
      disposed = true;
    };
  }, [isSourceMode, language]);

  const setEditorDirty = useCallback(
    (nextContent: string, baseSnapshot = snapshot) => {
      const nextDirty = baseSnapshot ? nextContent !== baseSnapshot.content : false;
      setDirty(nextDirty);
      onDirtyChange?.(path, nextDirty);
    },
    [onDirtyChange, path, snapshot],
  );

  const handleContentChange = useCallback(
    (value?: string) => {
      const nextContent = value ?? "";
      setContent(nextContent);
      setConflict(null);
      setEditorDirty(nextContent);
      if (lspStatus?.running) {
        if (lspChangeTimerRef.current !== null) {
          window.clearTimeout(lspChangeTimerRef.current);
        }
        lspChangeTimerRef.current = window.setTimeout(() => {
          editorLspChangeDocument(path, language, nextContent).catch(() => {});
          lspChangeTimerRef.current = null;
        }, 250);
      }
    },
    [language, lspStatus?.running, path, setEditorDirty],
  );

  const handleSave = useCallback(
    async (overwrite = false) => {
      if (!snapshot || saving) return;
      if (!dirty && !overwrite) return;

      setSaving(true);
      setError(null);
      setConflict(null);
      try {
        const saved = await editorSaveFile(path, content, snapshot.version, overwrite);
        setSnapshot(saved);
        setContent(saved.content);
        setModelContent(path, saved.content);
        updateModelBase(path, saved.content);
        updateModelBaseVersion(path, saved.version);
        setDirty(false);
        onDirtyChange?.(path, false);
        if (lspStatus?.running) {
          editorLspSaveDocument(path, language, saved.content).catch(() => {});
        }
        onSaved?.();
      } catch (e) {
        const message = String(e);
        if (message.includes("changed on disk") || message.includes("missing on disk")) {
          setConflict(message);
        } else {
          setError(message);
        }
      } finally {
        setSaving(false);
      }
    },
    [content, dirty, language, lspStatus?.running, onDirtyChange, onSaved, path, saving, snapshot],
  );

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (!(event.ctrlKey || event.metaKey) || event.key.toLowerCase() !== "s") return;
      if (!isSourceMode || !editorRef.current?.hasTextFocus()) return;
      event.preventDefault();
      handleSave();
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [handleSave, isSourceMode]);

  if (error) {
    return <div className="editor-error">加载文件失败：{error}</div>;
  }

  if (snapshot === null) {
    return <div className="editor-loading">正在加载文件...</div>;
  }

  return (
    <div className="editor-view">
      <div className="editor-toolbar">
        <div className="editor-toolbar-main">
          <span className="editor-toolbar-path">{snapshot.path}</span>
          {saving && <span className="editor-muted-pill">保存中...</span>}
          {isSourceMode && lspStatus?.running && <span className="editor-muted-pill">LSP 已连接</span>}
          {isSourceMode && !lspStatus?.running && lspStatus?.configured && lspStatus?.enabled && lspStatus?.message && (
            <button
              type="button"
              className="editor-muted-pill editor-lsp-settings-btn"
              title={`${lspStatus.message}。打开设置 > LSP`}
              onClick={() => window.dispatchEvent(new CustomEvent("kodex:open-settings"))}
            >
              LSP 需配置
            </button>
          )}
        </div>
        <div className="editor-toolbar-actions">
          {isRenderableDocument && !sourceMode && (
            <button type="button" className="editor-action-btn editor-source-toggle" onClick={() => setSourceMode(true)}>
              编辑原文
            </button>
          )}
          {isRenderableDocument && sourceMode && (
            <button type="button" className="editor-action-btn editor-source-toggle" onClick={() => setSourceMode(false)}>
              预览
            </button>
          )}
          {isSourceMode && (
            <>
              {conflict && <span className="editor-conflict-text">磁盘内容已变化</span>}
            </>
          )}
        </div>
      </div>
      {conflict && <div className="editor-conflict-banner">{conflict}</div>}
      {isImageFile ? (
        <div className="editor-preview editor-image-preview">
          <img className="editor-image" src={content} alt={snapshot.path} />
        </div>
      ) : !isSourceMode && language === "markdown" ? (
        <div className="editor-preview editor-document-preview">
          <Suspense fallback={<div className="editor-loading">正在加载预览...</div>}>
            <MarkdownBody content={content} />
          </Suspense>
        </div>
      ) : !isSourceMode && language === "html" ? (
        <div className="editor-preview editor-html-preview">
          <iframe className="editor-html-frame" title={snapshot.path} sandbox="" srcDoc={content} />
        </div>
      ) : (
        <Suspense fallback={<div className="editor-loading">正在加载编辑器...</div>}>
          <MonacoEditor
            height="100%"
            language={language}
            path={path}
            value={content}
            onChange={handleContentChange}
            theme={monacoThemeForAppTheme(appTheme)}
            beforeMount={handleBeforeMount}
            onMount={handleEditorMount}
            options={{
              readOnly: false,
              minimap: { enabled: true },
              scrollBeyondLastLine: false,
              fontSize: 13,
              fontFamily: "'Consolas', 'Courier New', monospace",
              lineHeight: 20,
              renderLineHighlight: "line",
              bracketPairColorization: { enabled: false },
              smoothScrolling: true,
              cursorBlinking: "smooth",
              padding: { top: 12, bottom: 12 },
              automaticLayout: true,
            }}
          />
        </Suspense>
      )}
    </div>
  );
}

function diagnosticToMarker(
  monaco: typeof import("monaco-editor"),
  diagnostic: LspDiagnostic,
): monacoEditor.IMarkerData {
  return {
    message: diagnostic.message,
    severity: diagnosticSeverity(monaco, diagnostic.severity),
    startLineNumber: diagnostic.startLine + 1,
    startColumn: diagnostic.startCharacter + 1,
    endLineNumber: diagnostic.endLine + 1,
    endColumn: diagnostic.endCharacter + 1,
  };
}

function diagnosticSeverity(monaco: typeof import("monaco-editor"), severity: number) {
  switch (severity) {
    case 1:
      return monaco.MarkerSeverity.Error;
    case 2:
      return monaco.MarkerSeverity.Warning;
    case 3:
      return monaco.MarkerSeverity.Info;
    case 4:
      return monaco.MarkerSeverity.Hint;
    default:
      return monaco.MarkerSeverity.Info;
  }
}
