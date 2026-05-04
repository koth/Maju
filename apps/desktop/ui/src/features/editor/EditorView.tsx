import { useState, useEffect, useRef, useCallback, useMemo, lazy, Suspense } from "react";
import type { editor as monacoEditor } from "monaco-editor";
import { editorOpenFile } from "../../lib/tauri";
import { saveViewState, restoreViewState } from "./monaco-view-state";
import { KODEX_THEME_NAME, kodexDarkTheme } from "./monaco-theme";
import { initTextMate, registerTextMateLanguage } from "./textmate-engine";
import "./EditorView.css";

const MonacoEditor = lazy(() =>
  import("@monaco-editor/react").then((mod) => ({ default: mod.default })),
);

let themeRegistered = false;
let textmateInitStarted = false;

const LANG_MAP: Record<string, string> = {
  ts: "typescript",
  tsx: "typescriptreact",
  js: "javascript",
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
}

export function EditorView({ path, lineNumber, searchQuery, navToken }: Props) {
  const [content, setContent] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const editorRef = useRef<monacoEditor.IStandaloneCodeEditor | null>(null);
  const prevPathRef = useRef<string | null>(null);
  const decorationsRef = useRef<monacoEditor.IEditorDecorationsCollection | null>(null);

  const language = useMemo(() => {
    const ext = path.split(".").pop()?.toLowerCase() ?? "";
    return LANG_MAP[ext] ?? "plaintext";
  }, [path]);

  useEffect(() => {
    if (prevPathRef.current && editorRef.current) {
      saveViewState(prevPathRef.current, editorRef.current);
    }
    prevPathRef.current = path;

    setContent(null);
    setError(null);
    editorOpenFile(path)
      .then(setContent)
      .catch((e) => setError(String(e)));
  }, [path]);

  // When lineNumber or searchQuery change for the same file (e.g. clicking another result)
  useEffect(() => {
    const editor = editorRef.current;
    if (!editor) return;

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
  }, [navToken]);

  const handleEditorMount = useCallback(
    (editor: monacoEditor.IStandaloneCodeEditor) => {
      editorRef.current = editor;

      // Only restore previous view state if we don't have a specific line to jump to
      if (!lineNumber) {
        restoreViewState(path, editor);
      }

      // Delay navigation to ensure Monaco has fully laid out content
      if (lineNumber || searchQuery) {
        requestAnimationFrame(() => {
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
    [path, lineNumber, searchQuery],
  );

  const handleBeforeMount = useCallback(
    (monaco: typeof import("monaco-editor")) => {
      if (!themeRegistered) {
        monaco.editor.defineTheme(KODEX_THEME_NAME, kodexDarkTheme);
        themeRegistered = true;
      }
      if (!textmateInitStarted) {
        textmateInitStarted = true;
        initTextMate().catch(() => {});
      }
      registerTextMateLanguage(monaco, language).catch(() => {});
    },
    [language],
  );

  if (error) {
    return <div className="editor-error">Failed to load file: {error}</div>;
  }

  if (content === null) {
    return <div className="editor-loading">Loading file...</div>;
  }

  return (
    <div className="editor-view">
      <Suspense fallback={<div className="editor-loading">Loading editor...</div>}>
        <MonacoEditor
          height="100%"
          language={language}
          value={content}
          theme="kodex-dark"
          beforeMount={handleBeforeMount}
          onMount={handleEditorMount}
          options={{
            readOnly: true,
            minimap: { enabled: true },
            scrollBeyondLastLine: false,
            fontSize: 13,
            fontFamily: "'Consolas', 'Courier New', monospace",
            lineHeight: 20,
            renderLineHighlight: "line",
            smoothScrolling: true,
            cursorBlinking: "smooth",
            padding: { top: 12, bottom: 12 },
            automaticLayout: true,
          }}
        />
      </Suspense>
    </div>
  );
}
