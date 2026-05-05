import { lazy, Suspense, useCallback, useMemo, useState, useRef } from "react";
import type { SessionFileChange, FileChangeType } from "../../types";
import { KODEX_THEME_NAME, kodexDarkTheme } from "./monaco-theme";
import { initTextMate, registerTextMateLanguage } from "./textmate-engine";
import "./DiffTab.css";

const MonacoDiffEditor = lazy(() =>
  import("@monaco-editor/react").then((mod) => ({ default: mod.DiffEditor })),
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
  change: SessionFileChange;
}

export function DiffTab({ change }: Props) {
  const [sideBySide, setSideBySide] = useState(true);
  const sideBySideRef = useRef(true);
  const editorRef = useRef<import("monaco-editor").editor.IStandaloneDiffEditor | null>(null);

  const language = useMemo(() => {
    const ext = change.path.split(".").pop()?.toLowerCase() ?? "";
    return LANG_MAP[ext] ?? "plaintext";
  }, [change.path]);

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

  const handleMount = useCallback(
    (editor: import("monaco-editor").editor.IStandaloneDiffEditor) => {
      editorRef.current = editor;
      editor.updateOptions({ renderSideBySide: true });
    },
    [],
  );

  const toggleSideBySide = useCallback(() => {
    setSideBySide((prev) => {
      const next = !prev;
      sideBySideRef.current = next;
      editorRef.current?.updateOptions({ renderSideBySide: next });
      return next;
    });
  }, []);

  const badgeConfig: Record<FileChangeType, { label: string; className: string }> = {
    Created: { label: "已添加", className: "dt-badge-created" },
    Modified: { label: "已修改", className: "dt-badge-modified" },
    Deleted: { label: "已删除", className: "dt-badge-deleted" },
  };
  const badge = badgeConfig[change.change_type];

  const fileName = change.path.replace(/\\/g, "/").split("/").pop() || change.path;

  return (
    <div className="diff-tab">
      <div className="dt-header">
        <div className="dt-header-left">
          <span className="dt-file-name">{fileName}</span>
          <span className={`dt-badge ${badge.className}`}>{badge.label}</span>
        </div>
        <div className="dt-header-right">
          <button
            type="button"
            className="dt-mode-btn"
            title={sideBySide ? "切换到内联差异" : "切换到并排差异"}
            onClick={toggleSideBySide}
          >
            {sideBySide ? "并排" : "内联"}
          </button>
          <span className="dt-path">{change.path}</span>
          <div className="dt-stats">
            {change.added_lines > 0 && (
              <span className="dt-stat-added">+{change.added_lines}</span>
            )}
            {change.removed_lines > 0 && (
              <span className="dt-stat-removed">-{change.removed_lines}</span>
            )}
          </div>
        </div>
      </div>
      <div className="dt-editor">
        <Suspense fallback={<div className="dt-loading">正在加载差异编辑器...</div>}>
          <MonacoDiffEditor
            height="100%"
            language={language}
            original={change.old_text ?? ""}
            modified={change.new_text}
            theme="kodex-dark"
            beforeMount={handleBeforeMount}
            onMount={handleMount}
            options={{
              readOnly: true,
              renderSideBySide: sideBySide,
              minimap: { enabled: false },
              scrollBeyondLastLine: false,
              fontSize: 13,
              fontFamily: "'Consolas', 'JetBrains Mono', 'Courier New', monospace",
              lineHeight: 20,
              smoothScrolling: true,
              padding: { top: 12, bottom: 12 },
              automaticLayout: true,
            }}
          />
        </Suspense>
      </div>
    </div>
  );
}
