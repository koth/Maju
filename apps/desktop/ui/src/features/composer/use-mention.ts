import { useCallback, useEffect, useRef, useState } from "react";
import type { FileEntry } from "../../types";
import { fsMentionSuggest } from "../../lib/tauri";
import { getCaretCoordinates, type CaretCoordinates } from "./caret-coordinates";

export type MentionKind = "File" | "Directory";

export interface MentionAnchor {
  /** Distance from the viewport's bottom edge where the popover's bottom sits. */
  bottom: number;
  /** Distance from the viewport's left edge where the popover's left sits. */
  left: number;
}

export interface UseMentionOptions {
  textareaRef: React.RefObject<HTMLTextAreaElement | null>;
  enabled: boolean;
  /** Receives the selected path; the hook already removed the `@query` text. */
  onSelect: (path: string, kind: MentionKind) => void;
  /** Used to splice out the `@query` range when a mention is confirmed. */
  setInput: (next: string | ((prev: string) => string)) => void;
}

export interface UseMentionResult {
  open: boolean;
  items: FileEntry[];
  activeIndex: number;
  loading: boolean;
  query: string;
  dirPart: string;
  prefix: string;
  anchor: MentionAnchor | null;
  setActiveIndex: (index: number) => void;
  syncFromValue: (nextValue: string) => void;
  handleKeyDown: (e: React.KeyboardEvent) => boolean;
  confirm: (index?: number) => void;
  close: () => void;
}

const MAX_QUERY_LENGTH = 64;
const FETCH_DEBOUNCE_MS = 150;

// Scan backwards from the caret for an `@` that opens a mention. The `@`
// must be preceded by start-of-text or whitespace, and the run between `@`
// and the caret must contain no whitespace (a space ends the mention).
function findMentionAnchor(value: string, caret: number): number {
  for (let i = caret - 1; i >= 0; i--) {
    const ch = value[i];
    if (ch === "@") {
      return i === 0 || /\s/.test(value[i - 1]) ? i : -1;
    }
    if (/\s/.test(ch)) return -1;
  }
  return -1;
}

function splitQuery(query: string): { dirPart: string; prefix: string } {
  const slash = query.lastIndexOf("/");
  if (slash < 0) return { dirPart: "", prefix: query };
  return { dirPart: query.slice(0, slash), prefix: query.slice(slash + 1) };
}

function computeAnchor(coords: CaretCoordinates, textarea: HTMLTextAreaElement): MentionAnchor {
  const rect = textarea.getBoundingClientRect();
  const MENU_WIDTH = 460;
  const VIEWPORT_MARGIN = 8;
  const desiredLeft = rect.left + coords.left;
  const maxLeft = Math.max(VIEWPORT_MARGIN, window.innerWidth - MENU_WIDTH - VIEWPORT_MARGIN);
  return {
    left: Math.max(VIEWPORT_MARGIN, Math.min(desiredLeft, maxLeft)),
    bottom: window.innerHeight - rect.top + 8,
  };
}

export function useMention({
  textareaRef,
  enabled,
  onSelect,
  setInput,
}: UseMentionOptions): UseMentionResult {
  const [open, setOpen] = useState(false);
  const [items, setItems] = useState<FileEntry[]>([]);
  const [activeIndex, setActiveIndex] = useState(0);
  const [loading, setLoading] = useState(false);
  const [query, setQuery] = useState("");
  const [dirPart, setDirPart] = useState("");
  const [prefix, setPrefix] = useState("");
  const [anchor, setAnchor] = useState<MentionAnchor | null>(null);

  const anchorRef = useRef(-1);
  const suppressedRef = useRef(-1);
  const fetchTokenRef = useRef(0);

  const close = useCallback(() => {
    if (anchorRef.current >= 0) suppressedRef.current = anchorRef.current;
    anchorRef.current = -1;
    setOpen(false);
    setItems([]);
    setActiveIndex(0);
    setLoading(false);
    setQuery("");
    setDirPart("");
    setPrefix("");
    setAnchor(null);
  }, []);

  // Disable (e.g. a turn goes active) closes any open mention.
  useEffect(() => {
    if (!enabled) close();
  }, [enabled, close]);

  const syncFromValue = useCallback(
    (nextValue: string) => {
      const textarea = textareaRef.current;
      const caret = textarea ? textarea.selectionStart : nextValue.length;

      if (!enabled) {
        close();
        return;
      }

      const found = findMentionAnchor(nextValue, caret);
      if (found < 0) {
        close();
        suppressedRef.current = -1;
        return;
      }
      if (suppressedRef.current === found) {
        // This mention was dismissed with Escape; keep it closed until the
        // `@` is gone or the caret leaves the token.
        if (open) close();
        return;
      }

      const q = nextValue.slice(found + 1, caret);
      if (q.length > MAX_QUERY_LENGTH) {
        close();
        return;
      }

      anchorRef.current = found;
      const coords = textarea ? getCaretCoordinates(textarea, caret) : null;
      setAnchor(coords && textarea ? computeAnchor(coords, textarea) : null);

      const { dirPart: dir, prefix: pre } = splitQuery(q);
      setOpen(true);
      setQuery(q);
      setDirPart(dir);
      setPrefix(pre);
    },
    [enabled, open, close, textareaRef],
  );

  // Debounced, race-cancelled fetch. Only fires when the menu is open.
  useEffect(() => {
    if (!open || !enabled) return;
    if (query === "") {
      setItems([]);
      setLoading(false);
      return;
    }

    const token = ++fetchTokenRef.current;
    setLoading(true);
    const handle = window.setTimeout(() => {
      void fsMentionSuggest(query)
        .then((result) => {
          if (fetchTokenRef.current !== token) return;
          setItems(result);
          setActiveIndex((prev) => Math.min(prev, Math.max(result.length - 1, 0)));
        })
        .catch(() => {
          if (fetchTokenRef.current !== token) return;
          setItems([]);
        })
        .finally(() => {
          if (fetchTokenRef.current === token) setLoading(false);
        });
    }, FETCH_DEBOUNCE_MS);

    return () => window.clearTimeout(handle);
  }, [open, enabled, query]);

  const confirm = useCallback(
    (index?: number) => {
      const i = index ?? activeIndex;
      const item = items[i];
      if (!item) {
        close();
        return;
      }
      const start = anchorRef.current;
      const textarea = textareaRef.current;
      const caret = textarea ? textarea.selectionStart : null;
      if (start < 0 || caret == null || caret < start) {
        close();
        return;
      }
      // Remove the `@query` text range from the draft before notifying the
      // composer, which then attaches the referenced file/folder as a chip.
      setInput((prev) => prev.slice(0, start) + prev.slice(caret));
      onSelect(item.path, item.kind);

      anchorRef.current = -1;
      suppressedRef.current = -1;
      setOpen(false);
      setItems([]);
      setActiveIndex(0);
      setQuery("");
      setDirPart("");
      setPrefix("");
      setAnchor(null);

      requestAnimationFrame(() => {
        const ta = textareaRef.current;
        ta?.focus();
        ta?.setSelectionRange(start, start);
      });
    },
    [activeIndex, items, onSelect, setInput, close, textareaRef],
  );

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent): boolean => {
      if (!open) return false;
      if (e.nativeEvent.isComposing || e.keyCode === 229) return false;

      if (items.length > 0) {
        if (e.key === "ArrowDown") {
          e.preventDefault();
          setActiveIndex((prev) => (prev + 1) % items.length);
          return true;
        }
        if (e.key === "ArrowUp") {
          e.preventDefault();
          setActiveIndex((prev) => (prev - 1 + items.length) % items.length);
          return true;
        }
        if ((e.key === "Enter" && !e.ctrlKey && !e.metaKey) || e.key === "Tab") {
          e.preventDefault();
          confirm();
          return true;
        }
      }
      if (e.key === "Escape") {
        e.preventDefault();
        close();
        return true;
      }
      return false;
    },
    [open, items.length, confirm, close],
  );

  return {
    open,
    items,
    activeIndex,
    loading,
    query,
    dirPart,
    prefix,
    anchor,
    setActiveIndex,
    syncFromValue,
    handleKeyDown,
    confirm,
    close,
  };
}
