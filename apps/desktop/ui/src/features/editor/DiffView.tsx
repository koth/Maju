import { useState, useEffect, useCallback, lazy, Suspense } from "react";
import type { AppTheme } from "../../types";
import { editorOpenFile, reviewGetDiff } from "../../lib/tauri";
import { getAppliedAppTheme } from "../../theme";
import { monacoThemeForAppTheme, registerKodexThemes } from "./monaco-theme";
import { languageForPath } from "./languages";
import "./EditorView.css";

const MonacoDiffEditor = lazy(() =>
  import("@monaco-editor/react").then((mod) => ({ default: mod.DiffEditor })),
);

interface Props {
  path: string;
  appTheme?: AppTheme;
}

export function DiffView({ path, appTheme }: Props) {
  const [original, setOriginal] = useState<string | null>(null);
  const [modified, setModified] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setOriginal(null);
    setModified(null);
    setError(null);

    Promise.all([
      editorOpenFile(path).then((snapshot) => snapshot.content).catch(() => ""),
      reviewGetDiff(path),
    ])
      .then(([fileContent, diffData]) => {
        if (diffData && diffData.hunks.length > 0) {
          // Reconstruct original from diff data
          const lines = fileContent.split("\n");
          setModified(fileContent);
          // Use file content as modified, reconstruct original by reversing hunks
          const originalLines: string[] = [];
          for (const hunk of diffData.hunks) {
            for (const line of hunk.lines) {
              if (line.kind === "Context" || line.kind === "Removed") {
                originalLines.push(line.content);
              }
            }
          }
          setOriginal(
            originalLines.length > 0 ? originalLines.join("\n") : lines.join("\n"),
          );
        } else {
          setOriginal(fileContent);
          setModified(fileContent);
        }
      })
      .catch((e) => setError(String(e)));
  }, [path]);

  const handleBeforeMount = useCallback(
    (monaco: typeof import("monaco-editor")) => {
      registerKodexThemes(monaco);
    },
    [],
  );

  if (error) {
    return <div className="editor-error">加载差异失败：{error}</div>;
  }

  if (original === null || modified === null) {
    return <div className="editor-loading">正在加载差异...</div>;
  }

  const language = languageForPath(path);

  return (
    <div className="editor-view">
      <Suspense fallback={<div className="editor-loading">正在加载差异编辑器...</div>}>
        <MonacoDiffEditor
          height="100%"
          language={language}
          original={original}
          modified={modified}
          theme={monacoThemeForAppTheme(appTheme ?? getAppliedAppTheme())}
          beforeMount={handleBeforeMount}
          options={{
            readOnly: true,
            renderSideBySide: true,
            ignoreTrimWhitespace: false,
            renderWhitespace: "all",
            minimap: { enabled: false },
            scrollBeyondLastLine: false,
            fontSize: 13,
            fontFamily: "'Consolas', 'Courier New', monospace",
            lineHeight: 20,
            smoothScrolling: true,
            padding: { top: 12, bottom: 12 },
            automaticLayout: true,
          }}
        />
      </Suspense>
    </div>
  );
}
