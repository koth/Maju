import { useCallback, useEffect, useState } from "react";
import type { MutableRefObject } from "react";
import type { UiSnapshot } from "../../types";

const TERMINAL_DOCK_HEIGHT_STORAGE_PREFIX = "kodex.terminalDock.height:";
const TERMINAL_DOCK_VISIBLE_STORAGE_PREFIX = "kodex.terminalDock.visible:";
const TERMINAL_DOCK_DEFAULT_HEIGHT = 220;

function terminalDockHeightKey(workspaceRoot: string) {
  return `${TERMINAL_DOCK_HEIGHT_STORAGE_PREFIX}${workspaceRoot}`;
}

function terminalDockVisibleKey(workspaceRoot: string) {
  return `${TERMINAL_DOCK_VISIBLE_STORAGE_PREFIX}${workspaceRoot}`;
}

function readTerminalDockHeight(workspaceRoot: string) {
  const stored = Number(window.localStorage.getItem(terminalDockHeightKey(workspaceRoot)));
  return Number.isFinite(stored) && stored >= 140 ? stored : TERMINAL_DOCK_DEFAULT_HEIGHT;
}

function readTerminalDockVisible(workspaceRoot: string) {
  return window.localStorage.getItem(terminalDockVisibleKey(workspaceRoot)) === "1";
}

export function useTerminalDockState(
  snapshot: UiSnapshot | null,
  snapshotRef: MutableRefObject<UiSnapshot | null>,
) {
  const [terminalDockVisible, setTerminalDockVisible] = useState(false);
  const [terminalDockMounted, setTerminalDockMounted] = useState(false);
  const [terminalDockHeight, setTerminalDockHeight] = useState(TERMINAL_DOCK_DEFAULT_HEIGHT);

  useEffect(() => {
    const workspaceRoot = snapshot?.workspace.root;
    if (!workspaceRoot) return;
    const visible = readTerminalDockVisible(workspaceRoot);
    setTerminalDockVisible(visible);
    setTerminalDockMounted(visible);
    setTerminalDockHeight(readTerminalDockHeight(workspaceRoot));
  }, [snapshot?.workspace.root]);

  const handleToggleTerminalDock = useCallback(() => {
    const workspaceRoot = snapshotRef.current?.workspace.root;
    setTerminalDockVisible((current) => {
      const next = !current;
      if (next) {
        setTerminalDockMounted(true);
      }
      if (workspaceRoot) {
        window.localStorage.setItem(terminalDockVisibleKey(workspaceRoot), next ? "1" : "0");
      }
      return next;
    });
  }, [snapshotRef]);

  const handleHideTerminalDock = useCallback(() => {
    const workspaceRoot = snapshotRef.current?.workspace.root;
    if (workspaceRoot) {
      window.localStorage.setItem(terminalDockVisibleKey(workspaceRoot), "0");
    }
    setTerminalDockVisible(false);
  }, [snapshotRef]);

  const handleTerminalDockHeightChange = useCallback((height: number) => {
    const workspaceRoot = snapshotRef.current?.workspace.root;
    setTerminalDockHeight(height);
    if (workspaceRoot) {
      window.localStorage.setItem(terminalDockHeightKey(workspaceRoot), String(height));
    }
  }, [snapshotRef]);

  return {
    terminalDockVisible,
    terminalDockMounted,
    terminalDockHeight,
    handleToggleTerminalDock,
    handleHideTerminalDock,
    handleTerminalDockHeightChange,
  };
}
