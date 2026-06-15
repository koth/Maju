import { lazy, Suspense, useCallback, useMemo, useState, useRef } from "react";
import type { SessionFileChange, FileChangeRecord, FileChangeType, AppTheme, DiffQuality } from "../../types";
import { monacoThemeForAppTheme, registerKodexThemes } from "./monaco-theme";
import { initTextMate, registerTextMateLanguage } from "./textmate-engine";
import { languageForPath } from "./languages";
import { useHorizontalScrollControls } from "../../lib/use-horizontal-scroll-controls";
import "./DiffTab.css";

const MonacoDiffEditor = lazy(() =>
  import("@monaco-editor/react").then((mod) => ({ default: mod.DiffEditor })),
);

const DIFF_HORIZONTAL_SCROLL_STEP = 80;

let textmateInitStarted = false;

interface Props {
  change: SessionFileChange | FileChangeRecord;
  appTheme: AppTheme;
  toolbarMode?: "default" | "breadcrumbs";
  workspaceName?: string;
  fileTreeVisible?: boolean;
  onToggleFileTree?: () => void;
}

export function DiffTab({
  change,
  appTheme,
  toolbarMode = "default",
  workspaceName,
  fileTreeVisible = false,
  onToggleFileTree,
}: Props) {
  const [sideBySide, setSideBySide] = useState(true);
  const sideBySideRef = useRef(true);
  const editorRef = useRef<import("monaco-editor").editor.IStandaloneDiffEditor | null>(null);
  const useBreadcrumbToolbar = toolbarMode === "breadcrumbs";

  const language = useMemo(() => {
    return languageForPath(change.path);
  }, [change.path]);
  const fileName = change.path.replace(/\\/g, "/").split("/").pop() || change.path;
  const quality = "quality" in change ? change.quality : "Exact";
  const unavailableReason = diffUnavailableReason(quality);
  const modifiedText = change.new_text ?? "";
  const scrollDiffHorizontally = useCallback((delta: number) => {
    const editor = editorRef.current;
    if (!editor) return;
    for (const pane of [editor.getOriginalEditor(), editor.getModifiedEditor()]) {
      pane.setScrollLeft(Math.max(0, pane.getScrollLeft() + delta));
    }
  }, []);
  const horizontalScroll = useHorizontalScrollControls<HTMLDivElement>({
    onScrollBy: scrollDiffHorizontally,
  });

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
    (
      editor: import("monaco-editor").editor.IStandaloneDiffEditor,
      monaco: typeof import("monaco-editor"),
    ) => {
      editorRef.current = editor;
      editor.updateOptions({ renderSideBySide: true });
      for (const pane of [editor.getOriginalEditor(), editor.getModifiedEditor()]) {
        pane.addCommand(monaco.KeyCode.LeftArrow, () => {
          scrollDiffHorizontally(-DIFF_HORIZONTAL_SCROLL_STEP);
        });
        pane.addCommand(monaco.KeyCode.RightArrow, () => {
          scrollDiffHorizontally(DIFF_HORIZONTAL_SCROLL_STEP);
        });
      }
    },
    [scrollDiffHorizontally],
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

  return (
    <div className={`diff-tab ${useBreadcrumbToolbar ? "is-breadcrumb-toolbar" : ""}`}>
      <div className="dt-header">
        <div className="dt-header-left">
          {useBreadcrumbToolbar ? (
            <DiffBreadcrumbs path={change.path} workspaceName={workspaceName} />
          ) : (
            <>
              <span className="dt-file-name">{fileName}</span>
              <span className={`dt-badge ${badge.className}`}>{badge.label}</span>
            </>
          )}
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
          {!useBreadcrumbToolbar && <span className="dt-path">{change.path}</span>}
          <div className="dt-stats">
            <span className="dt-stat-added">+{change.added_lines}</span>
            <span className="dt-stat-removed">-{change.removed_lines}</span>
          </div>
          {onToggleFileTree && (
            <button
              type="button"
              className={`dt-icon-btn dt-filetree-toggle ${fileTreeVisible ? "is-active" : ""}`}
              title={fileTreeVisible ? "隐藏 Git 文件树" : "显示 Git 文件树"}
              aria-label={fileTreeVisible ? "隐藏 Git 文件树" : "显示 Git 文件树"}
              aria-pressed={fileTreeVisible}
              onClick={onToggleFileTree}
            >
              <FolderPanelIcon />
            </button>
          )}
        </div>
      </div>
      <div
        className="dt-editor"
        {...horizontalScroll.scrollControlProps}
      >
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
                overviewRulerBorder: false,
                overviewRulerLanes: 0,
                hideCursorInOverviewRuler: true,
                fontSize: 13,
                fontFamily: "'Consolas', 'JetBrains Mono', 'Courier New', monospace",
                lineHeight: 20,
                smoothScrolling: true,
                scrollbar: {
                  vertical: "auto",
                  verticalScrollbarSize: 10,
                  horizontal: "auto",
                  horizontalScrollbarSize: 10,
                  useShadows: false,
                },
                padding: { top: useBreadcrumbToolbar ? 18 : 12, bottom: 12 },
                automaticLayout: true,
              }}
            />
          </Suspense>
        )}
      </div>
    </div>
  );
}

function DiffBreadcrumbs({
  path,
  workspaceName,
}: {
  path: string;
  workspaceName?: string;
}) {
  const segments = path.replace(/\\/g, "/").split("/").filter(Boolean);
  const rootLabel = workspaceName?.trim() || "workspace";
  const items = [rootLabel, ...segments];

  return (
    <nav className="dt-breadcrumbs" aria-label="差异文件路径" title={path}>
      {items.map((item, index) => {
        const isLast = index === items.length - 1;
        return (
          <span
            key={`${item}-${index}`}
            className={`dt-breadcrumb-item ${isLast ? "is-current" : ""}`}
          >
            <span className="dt-breadcrumb-label">{item}</span>
            {!isLast && <span className="dt-breadcrumb-separator" aria-hidden="true">›</span>}
          </span>
        );
      })}
    </nav>
  );
}

function FolderPanelIcon() {
  return (
    <svg viewBox="0 0 20 20" aria-hidden="true">
      <path d="M2.8 5.8c0-.9.7-1.6 1.6-1.6h3.4l1.4 1.5h6.4c.9 0 1.6.7 1.6 1.6v7c0 .9-.7 1.6-1.6 1.6H4.4c-.9 0-1.6-.7-1.6-1.6V5.8Z" />
      <path d="M2.8 7.6h14.4" />
      <path d="M13.2 9.4v4.7" />
    </svg>
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
