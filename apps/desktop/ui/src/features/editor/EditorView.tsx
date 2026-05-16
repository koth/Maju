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
  onAddComposerReference?: (request: {
    path: string;
    text: string;
    startLine: number;
    endLine: number;
  }) => void;
}

export function EditorView({
  path,
  lineNumber,
  searchQuery,
  navToken,
  appTheme,
  onDirtyChange,
  onSaved,
  onAddComposerReference,
}: Props) {
  const [snapshot, setSnapshot] = useState<EditorFileSnapshot | null>(null);
  const [content, setContent] = useState<string>("");
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [conflict, setConflict] = useState<string | null>(null);
  const [lspStatus, setLspStatus] = useState<LspServerStatus | null>(null);
  const [sourceMode, setSourceMode] = useState(false);
  const [fullscreen, setFullscreen] = useState(false);
  const [selectionReference, setSelectionReference] = useState<{
    text: string;
    startLine: number;
    endLine: number;
  } | null>(null);
  const editorRef = useRef<monacoEditor.IStandaloneCodeEditor | null>(null);
  const monacoRef = useRef<typeof import("monaco-editor") | null>(null);
  const prevPathRef = useRef<string | null>(null);
  const decorationsRef = useRef<monacoEditor.IEditorDecorationsCollection | null>(null);
  const selectionDisposableRef = useRef<{ dispose: () => void } | null>(null);
  const lspChangeTimerRef = useRef<number | null>(null);
  const editorDisposedRef = useRef(false);
  const openRequestSeqRef = useRef(0);
  const onDirtyChangeRef = useRef(onDirtyChange);
  const onSavedRef = useRef(onSaved);

  useEffect(() => {
    onDirtyChangeRef.current = onDirtyChange;
  }, [onDirtyChange]);

  useEffect(() => {
    onSavedRef.current = onSaved;
  }, [onSaved]);

  const language = useMemo(() => {
    const ext = path.split(".").pop()?.toLowerCase() ?? "";
    return LANG_MAP[ext] ?? "plaintext";
  }, [path]);
  const activeSnapshot = snapshot?.path === path ? snapshot : null;
  const fileKind = activeSnapshot?.kind ?? "text";
  const isTextFile = activeSnapshot != null && fileKind === "text";
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
        onDirtyChangeRef.current?.(path, hasDirtyModel);
      })
      .catch((e) => {
        if (!cancelled && requestSeq === openRequestSeqRef.current) {
          setError(String(e));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [path, safeSetEditorModel]);

  useEffect(() => {
    if (!activeSnapshot || !isSourceMode) return;
    let disposed = false;
    const modelContent = getModelValue(path) ?? activeSnapshot.content;
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
  }, [activeSnapshot, isSourceMode, language, path]);

  useEffect(() => {
    setSelectionReference(null);
  }, [path, isSourceMode]);

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
        selectionDisposableRef.current?.dispose();
        selectionDisposableRef.current = null;
        editorDisposedRef.current = true;
      });
      selectionDisposableRef.current?.dispose();
      const updateSelectionReference = () => {
        const model = editor.getModel();
        const selection = editor.getSelection();
        if (!model || !selection || selection.isEmpty()) {
          setSelectionReference(null);
          return;
        }
        const text = model.getValueInRange(selection);
        if (!text.trim()) {
          setSelectionReference(null);
          return;
        }
        const startLine = Math.min(selection.startLineNumber, selection.endLineNumber);
        const endLine = Math.max(selection.startLineNumber, selection.endLineNumber);
        setSelectionReference({ text, startLine, endLine });
      };
      selectionDisposableRef.current = editor.onDidChangeCursorSelection(updateSelectionReference);
      const model = getOrCreateModel(monacoInstance, path, content);
      updateModelBaseVersion(path, activeSnapshot?.version);
      safeSetEditorModel(model);
      updateSelectionReference();

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
    [path, content, lineNumber, searchQuery, safeSetEditorModel, activeSnapshot?.version],
  );

  useEffect(() => {
    const monaco = monacoRef.current;
    const editor = editorRef.current;
    if (!activeSnapshot || !isSourceMode || !monaco || !editor || editorDisposedRef.current) return;

    const model = getOrCreateModel(monaco, path, content);
    safeSetEditorModel(model);
    if (!dirty && model.getValue() !== content) {
      setModelContent(path, content);
    }
  }, [activeSnapshot, content, dirty, isSourceMode, path, safeSetEditorModel]);

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
    (nextContent: string, baseSnapshot = activeSnapshot) => {
      const nextDirty = baseSnapshot ? nextContent !== baseSnapshot.content : false;
      setDirty(nextDirty);
      onDirtyChangeRef.current?.(path, nextDirty);
    },
    [activeSnapshot, path],
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
      if (!activeSnapshot || saving) return;
      if (!dirty && !overwrite) return;

      setSaving(true);
      setError(null);
      setConflict(null);
      try {
        const saved = await editorSaveFile(path, content, activeSnapshot.version, overwrite);
        setSnapshot(saved);
        setContent(saved.content);
        setModelContent(path, saved.content);
        updateModelBase(path, saved.content);
        updateModelBaseVersion(path, saved.version);
        setDirty(false);
        onDirtyChangeRef.current?.(path, false);
        if (lspStatus?.running) {
          editorLspSaveDocument(path, language, saved.content).catch(() => {});
        }
        onSavedRef.current?.();
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
    [activeSnapshot, content, dirty, language, lspStatus?.running, path, saving],
  );

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (fullscreen && event.key === "Escape") {
        event.preventDefault();
        setFullscreen(false);
        return;
      }
      if (!(event.ctrlKey || event.metaKey) || event.key.toLowerCase() !== "s") return;
      if (!isSourceMode || !editorRef.current?.hasTextFocus()) return;
      event.preventDefault();
      handleSave();
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [fullscreen, handleSave, isSourceMode]);

  useEffect(() => {
    requestAnimationFrame(() => {
      const editor = editorRef.current;
      if (!editorDisposedRef.current && typeof editor?.layout === "function") {
        editor.layout();
      }
    });
  }, [fullscreen]);

  useEffect(() => {
    return () => {
      selectionDisposableRef.current?.dispose();
      selectionDisposableRef.current = null;
    };
  }, []);

  if (error) {
    return <div className="editor-error">加载文件失败：{error}</div>;
  }

  if (activeSnapshot === null) {
    return <div className="editor-loading">正在加载文件...</div>;
  }

  return (
    <div className={`editor-view ${fullscreen ? "is-fullscreen" : ""}`}>
      <div className="editor-toolbar">
        <div className="editor-toolbar-main">
          {fullscreen && <span className="editor-fullscreen-mode">全屏编辑</span>}
          <span className="editor-toolbar-path" title={activeSnapshot.path}>{activeSnapshot.path}</span>
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
          {isSourceMode && onAddComposerReference && (
            <button
              type="button"
              className="editor-action-btn editor-reference-btn"
              disabled={!selectionReference}
              title={
                selectionReference
                  ? `引用第 ${selectionReference.startLine}-${selectionReference.endLine} 行`
                  : "选中代码行后添加到 Composer 引用"
              }
              onClick={() => {
                if (!selectionReference) return;
                onAddComposerReference({
                  path,
                  text: selectionReference.text,
                  startLine: selectionReference.startLine,
                  endLine: selectionReference.endLine,
                });
              }}
            >
              引用选区
            </button>
          )}
          {isSourceMode && (
            <>
              {conflict && <span className="editor-conflict-text">磁盘内容已变化</span>}
            </>
          )}
          <button
            type="button"
            className={
              fullscreen
                ? "editor-action-btn editor-exit-fullscreen-btn"
                : "editor-action-btn editor-icon-btn editor-fullscreen-btn"
            }
            title={fullscreen ? "退出全屏 (Esc)" : "全屏编辑"}
            aria-label={fullscreen ? "退出全屏" : "全屏编辑"}
            aria-keyshortcuts={fullscreen ? "Escape" : undefined}
            onClick={() => setFullscreen((value) => !value)}
          >
            {fullscreen ? (
              <>
                <span aria-hidden="true" className="editor-fullscreen-symbol">⤡</span>
                <span>退出全屏</span>
                <span className="editor-fullscreen-shortcut">Esc</span>
              </>
            ) : (
              <span aria-hidden="true">⛶</span>
            )}
          </button>
        </div>
      </div>
      {conflict && <div className="editor-conflict-banner">{conflict}</div>}
      {isImageFile ? (
        <div className="editor-preview editor-image-preview">
          <img className="editor-image" src={content} alt={activeSnapshot.path} />
        </div>
      ) : !isSourceMode && language === "markdown" ? (
        <div className="editor-preview editor-document-preview">
          <Suspense fallback={<div className="editor-loading">正在加载预览...</div>}>
            <MarkdownBody content={content} />
          </Suspense>
        </div>
      ) : !isSourceMode && language === "html" ? (
        <div className="editor-preview editor-html-preview">
          <iframe className="editor-html-frame" title={activeSnapshot.path} sandbox="" srcDoc={content} />
        </div>
      ) : (
        <Suspense fallback={<div className="editor-loading">正在加载编辑器...</div>}>
          <MonacoEditor
            height="100%"
            language={language}
            path={path}
            value={content}
            keepCurrentModel
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
