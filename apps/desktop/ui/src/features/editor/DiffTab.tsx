import { memo, useCallback, useMemo, useState } from "react";
import { MultiFileDiff } from "@pierre/diffs/react";
import type { SessionFileChange, FileChangeRecord, FileChangeType, AppTheme } from "../../types";
import { useHorizontalScrollControls } from "../../lib/use-horizontal-scroll-controls";
import {
  buildPierreDiff,
  pierreDiffOptions,
  resolvePierreDiffHorizontalScrollTarget,
} from "./pierre-diff";
import "./DiffTab.css";

interface Props {
  change: SessionFileChange | FileChangeRecord;
  appTheme: AppTheme;
  toolbarMode?: "default" | "breadcrumbs";
  workspaceName?: string;
  fileTreeVisible?: boolean;
  onToggleFileTree?: () => void;
}

export const DiffTab = memo(function DiffTab({
  change,
  appTheme,
  toolbarMode = "default",
  workspaceName,
  fileTreeVisible = false,
  onToggleFileTree,
}: Props) {
  const [sideBySide, setSideBySide] = useState(false);
  const useBreadcrumbToolbar = toolbarMode === "breadcrumbs";
  const fileName = change.path.replace(/\\/g, "/").split("/").pop() || change.path;
  const diffPreview = useMemo(() => buildPierreDiff(change), [change]);
  const diffOptions = useMemo(
    () => pierreDiffOptions(appTheme, sideBySide ? "split" : "unified"),
    [appTheme, sideBySide],
  );
  const horizontalScroll = useHorizontalScrollControls<HTMLDivElement>({
    resolveScrollTarget: resolvePierreDiffHorizontalScrollTarget,
  });

  const toggleSideBySide = useCallback(() => {
    setSideBySide((prev) => !prev);
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
        {...horizontalScroll.scrollControlProps}
        className="dt-editor"
      >
        {diffPreview.kind === "patch" ? (
          <div className="dt-pierre-scroll" aria-label={`${change.path} 差异预览`}>
            <MultiFileDiff
              oldFile={diffPreview.oldFile}
              newFile={diffPreview.newFile}
              className="dt-pierre-diff"
              options={diffOptions}
              disableWorkerPool
            />
          </div>
        ) : (
          <div className="dt-unavailable">{diffPreview.text}</div>
        )}
      </div>
    </div>
  );
});

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
