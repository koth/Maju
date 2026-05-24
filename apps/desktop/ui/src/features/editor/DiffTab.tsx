import { lazy, Suspense, useCallback, useMemo, useState, useRef } from "react";
import type { SessionFileChange, FileChangeRecord, FileChangeType, AppTheme, DiffQuality } from "../../types";
import { monacoThemeForAppTheme, registerKodexThemes } from "./monaco-theme";
import { initTextMate, registerTextMateLanguage } from "./textmate-engine";
import { languageForPath } from "./languages";
import "./DiffTab.css";

const MonacoDiffEditor = lazy(() =>
  import("@monaco-editor/react").then((mod) => ({ default: mod.DiffEditor })),
);

let textmateInitStarted = false;

interface Props {
  change: SessionFileChange | FileChangeRecord;
  appTheme: AppTheme;
}

export function DiffTab({ change, appTheme }: Props) {
  const [sideBySide, setSideBySide] = useState(true);
  const sideBySideRef = useRef(true);
  const editorRef = useRef<import("monaco-editor").editor.IStandaloneDiffEditor | null>(null);

  const language = useMemo(() => {
    return languageForPath(change.path);
  }, [change.path]);

  const handleBeforeMount = useCallback(
    (monaco: typeof import("monaco-editor")) => {
      registerKodexThemes(monaco);
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
  const quality = "quality" in change ? change.quality : "Exact";
  const unavailableReason = diffUnavailableReason(quality);
  const modifiedText = change.new_text ?? "";

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
            <span className="dt-stat-added">+{change.added_lines}</span>
            <span className="dt-stat-removed">-{change.removed_lines}</span>
          </div>
        </div>
      </div>
      <div className="dt-editor">
        {unavailableReason ? (
          <div className="dt-unavailable">{unavailableReason}</div>
        ) : (
          <Suspense fallback={<div className="dt-loading">正在加载差异编辑器...</div>}>
            <MonacoDiffEditor
              height="100%"
              language={language}
              original={change.old_text ?? ""}
              modified={modifiedText}
              theme={monacoThemeForAppTheme(appTheme)}
              beforeMount={handleBeforeMount}
              onMount={handleMount}
              options={{
                readOnly: true,
                renderSideBySide: sideBySide,
                ignoreTrimWhitespace: false,
                renderWhitespace: "all",
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
        )}
      </div>
    </div>
  );
}

function diffUnavailableReason(quality: DiffQuality) {
  const labels: Record<DiffQuality, string | null> = {
    Exact: null,
    LargeFileSkipped: "文件太大，已跳过内联差异预览。",
    BinarySkipped: "二进制或不可读取文件，无法展示文本差异。",
    MissingBaseline: "缺少可比较的基线内容，无法展示可靠差异。",
    FragmentRejected: "只捕获到了片段级改动，已拒绝渲染为完整文件差异。",
    LegacyIncomplete: "旧历史记录缺少完整快照，无法展示可靠差异。",
  };
  return labels[quality] ?? null;
}
