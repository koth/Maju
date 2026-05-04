import { lazy, Suspense, useCallback, useMemo } from "react";
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

  const badgeConfig: Record<FileChangeType, { label: string; className: string }> = {
    Created: { label: "ADDED", className: "dt-badge-created" },
    Modified: { label: "MODIFIED", className: "dt-badge-modified" },
    Deleted: { label: "DELETED", className: "dt-badge-deleted" },
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
        <Suspense fallback={<div className="dt-loading">Loading diff editor...</div>}>
          <MonacoDiffEditor
            height="100%"
            language={language}
            original={change.old_text ?? ""}
            modified={change.new_text}
            theme="kodex-dark"
            beforeMount={handleBeforeMount}
            options={{
              readOnly: true,
              renderSideBySide: true,
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
