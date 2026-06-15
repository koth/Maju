import { useCallback, useEffect, useMemo, useRef } from "react";
import type { KeyboardEvent as ReactKeyboardEvent } from "react";

const DEFAULT_SCROLL_STEP = 80;

interface HorizontalScrollControlsOptions<T extends HTMLElement> {
  enabled?: boolean;
  step?: number;
  onScrollBy?: (delta: number) => void;
  resolveScrollTarget?: (root: T) => HTMLElement | null;
}

export function useHorizontalScrollControls<T extends HTMLElement>({
  enabled = true,
  step = DEFAULT_SCROLL_STEP,
  onScrollBy,
  resolveScrollTarget,
}: HorizontalScrollControlsOptions<T> = {}) {
  const ref = useRef<T | null>(null);
  const hoveredRef = useRef(false);

  const scrollByDelta = useCallback(
    (delta: number) => {
      if (!enabled) return;
      if (onScrollBy) {
        onScrollBy(delta);
        return;
      }
      const element = ref.current;
      if (!element) return;
      const scrollTarget = resolveScrollTarget?.(element) ?? element;
      scrollTarget.scrollLeft = Math.max(0, scrollTarget.scrollLeft + delta);
    },
    [enabled, onScrollBy, resolveScrollTarget],
  );

  const handleArrowKey = useCallback(
    (event: KeyboardEvent | ReactKeyboardEvent<T>) => {
      if (
        !enabled ||
        event.defaultPrevented ||
        event.altKey ||
        event.ctrlKey ||
        event.metaKey ||
        event.shiftKey
      ) {
        return;
      }

      if (event.key === "ArrowLeft") {
        scrollByDelta(-step);
      } else if (event.key === "ArrowRight") {
        scrollByDelta(step);
      } else {
        return;
      }

      event.preventDefault();
      event.stopPropagation();
    },
    [enabled, scrollByDelta, step],
  );

  const handleMouseEnter = useCallback(() => {
    hoveredRef.current = true;
  }, []);

  const handleMouseLeave = useCallback(() => {
    hoveredRef.current = false;
  }, []);

  useEffect(() => {
    if (!enabled || typeof window === "undefined" || typeof document === "undefined") {
      return;
    }

    const handleDocumentKeyDown = (event: KeyboardEvent) => {
      const element = ref.current;
      if (!element) return;
      const activeElement = document.activeElement;
      const hasDiffFocus = activeElement instanceof Node && element.contains(activeElement);
      if (!hoveredRef.current && !hasDiffFocus) return;
      if (isEditableTarget(activeElement)) return;
      handleArrowKey(event);
    };

    window.addEventListener("keydown", handleDocumentKeyDown, { capture: true });
    return () => {
      window.removeEventListener("keydown", handleDocumentKeyDown, { capture: true });
    };
  }, [enabled, handleArrowKey]);

  const scrollControlProps = useMemo(
    () => ({
      ref,
      tabIndex: enabled ? 0 : -1,
      onKeyDownCapture: handleArrowKey,
      onMouseEnter: handleMouseEnter,
      onMouseLeave: handleMouseLeave,
    }),
    [enabled, handleArrowKey, handleMouseEnter, handleMouseLeave],
  );

  return {
    ref,
    scrollByDelta,
    scrollControlProps,
  };
}

function isEditableTarget(target: EventTarget | null) {
  if (typeof HTMLElement === "undefined" || !(target instanceof HTMLElement)) {
    return false;
  }
  return Boolean(
    target.closest(
      "button,a,input,textarea,select,summary,[contenteditable='true'],[role='button'],[data-horizontal-scroll-ignore='true']",
    ),
  );
}
