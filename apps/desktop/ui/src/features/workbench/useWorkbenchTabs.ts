import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { FileChangeRecord, SessionFileChange, TabDescriptor } from "../../types";
import { editorSaveFile, sessionGetChangeSetFileDiff } from "../../lib/tauri";
import {
  disposeModel,
  getModelBaseVersion,
  getModelValue,
  isModelDirty,
  updateModelBase,
  updateModelBaseVersion,
} from "../editor/monaco-model-registry";

const CONVERSATION_TAB: TabDescriptor = {
  id: "conversation",
  type: "conversation",
  label: "Chat",
};

export interface PendingCloseTab {
  id: string;
  label: string;
  filePath: string;
}

interface UseWorkbenchTabsArgs {
  onAfterEditorSave: () => Promise<void>;
}

export function useWorkbenchTabs({ onAfterEditorSave }: UseWorkbenchTabsArgs) {
  const [tabs, setTabs] = useState<TabDescriptor[]>([CONVERSATION_TAB]);
  const [activeTabId, setActiveTabId] = useState("conversation");
  const [pendingCloseTab, setPendingCloseTab] = useState<PendingCloseTab | null>(null);
  const [resolvedDiffChange, setResolvedDiffChange] = useState<
    SessionFileChange | FileChangeRecord | null
  >(null);
  const navTokenRef = useRef(0);

  const resetTabs = useCallback(() => {
    setTabs([CONVERSATION_TAB]);
    setActiveTabId("conversation");
    setPendingCloseTab(null);
  }, []);

  const handleOpenDiffTab = useCallback(
    (
      path: string,
      source: "session" | "git" | "change-set" = "session",
      change?: SessionFileChange,
      changeSetId?: string,
      record?: FileChangeRecord,
    ) => {
      const tabId = changeSetId
        ? `diff:${changeSetId}:${path}`
        : change
          ? `diff:turn:${path}:${change.timestamp}:${change.added_lines}:${change.removed_lines}`
          : `diff:${source}:${path}`;
      setTabs((prev) => {
        if (prev.some((t) => t.id === tabId)) return prev;
        const fileName = path.replace(/\\/g, "/").split("/").pop() || path;
        return [
          ...prev,
          {
            id: tabId,
            type: "diff" as const,
            label: fileName,
            filePath: path,
            diffSource: source,
            changeSetId,
            diffChange: change,
            diffRecord: record,
          },
        ];
      });
      setActiveTabId(tabId);
    },
    [],
  );

  const handleOpenEditorTab = useCallback((filePath: string) => {
    const tabId = `editor:${filePath}`;
    setTabs((prev) => {
      if (prev.some((t) => t.id === tabId)) return prev;
      const fileName = filePath.replace(/\\/g, "/").split("/").pop() || filePath;
      return [
        ...prev,
        { id: tabId, type: "editor" as const, label: fileName, filePath },
      ];
    });
    setActiveTabId(tabId);
  }, []);

  const handleSearchResultOpen = useCallback(
    (filePath: string, lineNumber?: number, searchQuery?: string) => {
      const tabId = `editor:${filePath}`;
      const token = ++navTokenRef.current;
      setTabs((prev) => {
        const existing = prev.find((t) => t.id === tabId);
        if (existing) {
          return prev.map((t) =>
            t.id === tabId ? { ...t, lineNumber, searchQuery, navToken: token } : t,
          );
        }
        const fileName = filePath.replace(/\\/g, "/").split("/").pop() || filePath;
        return [
          ...prev,
          {
            id: tabId,
            type: "editor" as const,
            label: fileName,
            filePath,
            lineNumber,
            searchQuery,
            navToken: token,
          },
        ];
      });
      setActiveTabId(tabId);
    },
    [],
  );

  const closeTabById = useCallback(
    (id: string) => {
      if (id === "conversation") return;
      setTabs((prev) => {
        const filtered = prev.filter((t) => t.id !== id);
        if (activeTabId === id) {
          const idx = prev.findIndex((t) => t.id === id);
          const newActive = filtered[Math.min(idx, filtered.length - 1)]?.id ?? "conversation";
          setActiveTabId(newActive);
        }
        return filtered;
      });
    },
    [activeTabId],
  );

  const handleCloseTab = useCallback(
    async (id: string) => {
      if (id === "conversation") return;

      const closing = tabs.find((tab) => tab.id === id);
      if (closing?.type !== "editor" || !closing.filePath) {
        closeTabById(id);
        return;
      }

      const hasUnsavedChanges = Boolean(closing.dirty) || isModelDirty(closing.filePath);
      if (!hasUnsavedChanges) {
        closeTabById(id);
        return;
      }

      setPendingCloseTab({
        id,
        label: closing.label,
        filePath: closing.filePath,
      });
    },
    [closeTabById, tabs],
  );

  const handleConfirmSaveClose = useCallback(async () => {
    if (!pendingCloseTab) return;

    const content = getModelValue(pendingCloseTab.filePath);
    const baseVersion = getModelBaseVersion(pendingCloseTab.filePath);
    if (content == null || !baseVersion) {
      window.alert("这个文件的编辑状态还没有准备好，请切回文件后再保存或关闭。");
      return;
    }

    try {
      const saved = await editorSaveFile(pendingCloseTab.filePath, content, baseVersion);
      updateModelBase(pendingCloseTab.filePath, saved.content);
      updateModelBaseVersion(pendingCloseTab.filePath, saved.version);
      disposeModel(pendingCloseTab.filePath);
      closeTabById(pendingCloseTab.id);
      setPendingCloseTab(null);
      await onAfterEditorSave();
    } catch (error) {
      window.alert(`保存失败，文件未关闭：${String(error)}`);
    }
  }, [closeTabById, onAfterEditorSave, pendingCloseTab]);

  const handleConfirmDiscardClose = useCallback(() => {
    if (!pendingCloseTab) return;
    disposeModel(pendingCloseTab.filePath);
    closeTabById(pendingCloseTab.id);
    setPendingCloseTab(null);
  }, [closeTabById, pendingCloseTab]);

  const handleCancelClose = useCallback(() => {
    setPendingCloseTab(null);
  }, []);

  const handleEditorDirtyChange = useCallback((filePath: string, dirty: boolean) => {
    setTabs((prev) =>
      prev.map((tab) =>
        tab.type === "editor" && tab.filePath === filePath ? { ...tab, dirty } : tab,
      ),
    );
  }, []);

  const handleEditorSaved = useCallback(async () => {
    await onAfterEditorSave();
  }, [onAfterEditorSave]);

  const handleTabSelect = useCallback((id: string) => {
    setActiveTabId(id);
  }, []);

  const activeTab = tabs.find((t) => t.id === activeTabId) ?? tabs[0];
  const isDiffTab = activeTab.type === "diff" && activeTab.filePath != null;

  useEffect(() => {
    const filePath = activeTab.filePath;
    if (!isDiffTab || !filePath) {
      setResolvedDiffChange(null);
      return;
    }

    let cancelled = false;
    setResolvedDiffChange(null);
    if (activeTab.diffRecord) {
      setResolvedDiffChange(activeTab.diffRecord);
      return () => {
        cancelled = true;
      };
    }
    if (activeTab.diffChange && !activeTab.changeSetId) {
      setResolvedDiffChange(activeTab.diffChange);
      return () => {
        cancelled = true;
      };
    }
    if (!activeTab.changeSetId) {
      return () => {
        cancelled = true;
      };
    }

    sessionGetChangeSetFileDiff({
      change_set_id: activeTab.changeSetId,
      path: filePath,
    })
      .then((change) => {
        if (!cancelled) setResolvedDiffChange(change);
      })
      .catch(() => {
        if (!cancelled) setResolvedDiffChange(null);
      });

    return () => {
      cancelled = true;
    };
  }, [isDiffTab, activeTab.filePath, activeTab.diffChange, activeTab.diffRecord, activeTab.changeSetId]);

  const displayTabs = useMemo(
    () => tabs.map((tab) => (tab.type === "conversation" ? { ...tab, label: "Chat" } : tab)),
    [tabs],
  );

  return {
    tabs,
    activeTab,
    activeTabId,
    displayTabs,
    resolvedDiffChange,
    pendingCloseTab,
    resetTabs,
    handleOpenDiffTab,
    handleOpenEditorTab,
    handleSearchResultOpen,
    handleCloseTab,
    handleConfirmSaveClose,
    handleConfirmDiscardClose,
    handleCancelClose,
    handleEditorDirtyChange,
    handleEditorSaved,
    handleTabSelect,
  };
}
