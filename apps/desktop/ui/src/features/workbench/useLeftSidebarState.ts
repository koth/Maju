import { useCallback, useState } from "react";
import type { CSSProperties, PointerEvent as ReactPointerEvent } from "react";

const LEFT_SIDEBAR_WIDTH_STORAGE_KEY = "kodex.leftSidebarWidth";
const LEFT_SIDEBAR_DEFAULT_WIDTH = 296;
const LEFT_SIDEBAR_MIN_WIDTH = 172;
const LEFT_SIDEBAR_MAX_WIDTH = 420;
const LEFT_SIDEBAR_MAX_VIEWPORT_RATIO = 0.46;
const LEFT_SIDEBAR_MIN_MAIN_WIDTH = 420;

function getWorkbenchContentRect() {
  return document.querySelector<HTMLElement>(".workbench-content")?.getBoundingClientRect();
}

function getLeftSidebarMaxWidth() {
  if (typeof window === "undefined") return LEFT_SIDEBAR_MAX_WIDTH;
  const contentWidth = getWorkbenchContentRect()?.width ?? window.innerWidth;
  const layoutMax = Math.floor(contentWidth - LEFT_SIDEBAR_MIN_MAIN_WIDTH);
  const viewportMax = Math.floor(window.innerWidth * LEFT_SIDEBAR_MAX_VIEWPORT_RATIO);
  return Math.min(
    LEFT_SIDEBAR_MAX_WIDTH,
    viewportMax,
    Math.max(LEFT_SIDEBAR_MIN_WIDTH, layoutMax),
  );
}

function clampLeftSidebarWidth(width: number) {
  return Math.min(getLeftSidebarMaxWidth(), Math.max(LEFT_SIDEBAR_MIN_WIDTH, width));
}

export function useLeftSidebarState() {
  const [leftSidebarWidth, setLeftSidebarWidth] = useState(() => {
    const stored = Number(window.localStorage.getItem(LEFT_SIDEBAR_WIDTH_STORAGE_KEY));
    return Number.isFinite(stored) ? clampLeftSidebarWidth(stored) : LEFT_SIDEBAR_DEFAULT_WIDTH;
  });

  const clampStoredLeftSidebarWidth = useCallback(() => {
    setLeftSidebarWidth((current) => {
      const next = clampLeftSidebarWidth(current);
      if (next !== current) {
        window.localStorage.setItem(LEFT_SIDEBAR_WIDTH_STORAGE_KEY, String(next));
      }
      return next;
    });
  }, []);

  const handleLeftSidebarResizeStart = useCallback((event: ReactPointerEvent<HTMLButtonElement>) => {
    event.preventDefault();
    const pointerId = event.pointerId;
    event.currentTarget.setPointerCapture(pointerId);
    document.body.classList.add("is-resizing-left-sidebar");

    const updateWidth = (clientX: number) => {
      const contentLeft = getWorkbenchContentRect()?.left ?? 0;
      const nextWidth = clampLeftSidebarWidth(clientX - contentLeft);
      setLeftSidebarWidth(nextWidth);
      window.localStorage.setItem(LEFT_SIDEBAR_WIDTH_STORAGE_KEY, String(nextWidth));
    };

    const handlePointerMove = (moveEvent: PointerEvent) => {
      updateWidth(moveEvent.clientX);
    };

    const handlePointerUp = () => {
      document.body.classList.remove("is-resizing-left-sidebar");
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("pointerup", handlePointerUp);
      window.removeEventListener("pointercancel", handlePointerUp);
    };

    window.addEventListener("pointermove", handlePointerMove);
    window.addEventListener("pointerup", handlePointerUp);
    window.addEventListener("pointercancel", handlePointerUp);
  }, []);

  const leftSidebarStyle = {
    "--left-sidebar-width": `${leftSidebarWidth}px`,
  } as CSSProperties;

  return {
    leftSidebarWidth,
    leftSidebarStyle,
    clampStoredLeftSidebarWidth,
    handleLeftSidebarResizeStart,
  };
}
