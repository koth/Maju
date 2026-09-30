import { createPortal } from "react-dom";
import {
  useEffect,
  useLayoutEffect,
  useMemo,
  useState,
  type CSSProperties,
  type ReactNode,
  type RefObject,
} from "react";
import type { SearchResult } from "../../types";
import { openLinkInPanelOrExternal } from "../browser/browserTabs";
import { getFileIcon } from "../filetree/file-icons";
import "./SearchResults.css";

const MAX_VISIBLE_MATCHES = 3;
const DROPDOWN_WIDTH = 560;
const DROPDOWN_GAP = 8;
const VIEWPORT_MARGIN = 12;

interface Props {
  result: SearchResult | null;
  loading: boolean;
  error: string | null;
  query?: string;
  onFileOpen: (filePath: string, lineNumber?: number, searchQuery?: string) => void;
  onClose: () => void;
  anchorRef?: RefObject<HTMLElement | null>;
  placement?: "anchored" | "inline";
  activeIndex?: number;
  onActiveIndexChange?: (index: number) => void;
  onActiveCountChange?: (count: number) => void;
}

interface DropdownPosition {
  top: number;
  left: number;
  width: number;
}

type SelectableItem =
  | { kind: "file"; path: string; name: string }
  | {
      kind: "match";
      path: string;
      lineNumber: number;
      lineText: string;
      remaining?: number;
    };

export function SearchResults({
  result,
  loading,
  error,
  query = "",
  onFileOpen,
  onClose,
  anchorRef,
  placement = "anchored",
  activeIndex = -1,
  onActiveIndexChange,
  onActiveCountChange,
}: Props) {
  const [position, setPosition] = useState<DropdownPosition | null>(null);
  const inline = placement === "inline";
  const highlightQuery = (result?.query || query).trim();

  const selectableItems = useMemo(() => {
    if (!result) return [] as SelectableItem[];
    const items: SelectableItem[] = [];
    for (const file of result.file_suggestions ?? []) {
      items.push({ kind: "file", path: file.path, name: file.name });
    }
    for (const file of result.files) {
      items.push({
        kind: "file",
        path: file.path,
        name: file.path.split(/[/\\]/).pop() || file.path,
      });
      for (const match of file.matches.slice(0, MAX_VISIBLE_MATCHES)) {
        items.push({
          kind: "match",
          path: file.path,
          lineNumber: match.line_number,
          lineText: match.line_text,
        });
      }
    }
    return items;
  }, [result]);

  useEffect(() => {
    onActiveCountChange?.(selectableItems.length);
  }, [onActiveCountChange, selectableItems.length]);

  useEffect(() => {
    if (activeIndex < 0) return;
    const node = document.querySelector<HTMLElement>(
      `[data-search-item-index="${activeIndex}"]`,
    );
    node?.scrollIntoView({ block: "nearest" });
  }, [activeIndex, selectableItems.length]);

  useLayoutEffect(() => {
    if (inline) return;
    const updatePosition = () => {
      const anchor = anchorRef?.current;
      if (!anchor) {
        setPosition(null);
        return;
      }
      const rect = anchor.getBoundingClientRect();
      const width = Math.min(DROPDOWN_WIDTH, window.innerWidth - VIEWPORT_MARGIN * 2);
      const preferredLeft = rect.left + rect.width / 2 - width / 2;
      const maxLeft = window.innerWidth - width - VIEWPORT_MARGIN;
      const left = Math.max(VIEWPORT_MARGIN, Math.min(preferredLeft, maxLeft));
      setPosition({
        top: rect.bottom + DROPDOWN_GAP,
        left,
        width,
      });
    };

    updatePosition();
    window.addEventListener("resize", updatePosition);
    window.addEventListener("scroll", updatePosition, true);
    return () => {
      window.removeEventListener("resize", updatePosition);
      window.removeEventListener("scroll", updatePosition, true);
    };
  }, [anchorRef, inline, loading, error, result]);

  const style: CSSProperties | undefined =
    !inline && position
      ? {
          top: `${position.top}px`,
          left: `${position.left}px`,
          width: `${position.width}px`,
          right: "auto",
          transform: "none",
        }
      : undefined;

  const openItem = (item: SelectableItem) => {
    if (item.kind === "file") {
      onFileOpen(item.path);
    } else {
      onFileOpen(item.path, item.lineNumber, result?.query || query);
    }
    onClose();
  };

  let itemCursor = -1;
  const nextIndex = () => {
    itemCursor += 1;
    return itemCursor;
  };

  const body = (() => {
    if (loading) {
      return (
        <div className="search-results-status">
          <span className="search-results-spinner" aria-hidden="true" />
          <span>正在搜索{query.trim() ? ` “${query.trim()}”` : ""}…</span>
        </div>
      );
    }

    if (error) {
      return (
        <div className="search-results-error">
          <LinkifiedText text={error} />
        </div>
      );
    }

    if (!result) {
      if (!query.trim()) {
        return (
          <div className="search-results-status is-hint">
            输入文件名或内容关键字，支持工作区全文检索
          </div>
        );
      }
      return (
        <div className="search-results-status is-hint">
          输入后自动搜索文件名与内容
        </div>
      );
    }

    const suggestions = result.file_suggestions ?? [];
    const hasSuggestions = suggestions.length > 0;
    const hasContentMatches = result.files.length > 0;

    if (!hasSuggestions && !hasContentMatches && !result.notice) {
      return (
        <div className="search-results-status">
          未找到与 “{result.query}” 匹配的结果
        </div>
      );
    }

    return (
      <>
        {result.notice && (
          <div className="search-results-notice">
            <span>{result.notice.message}</span>
            {result.notice.url && (
              <ExternalLink href={result.notice.url}>
                {result.notice.url_label ?? result.notice.url}
              </ExternalLink>
            )}
          </div>
        )}
        {hasSuggestions && (
          <section className="search-results-section">
            <div className="search-results-section-title">
              <span>文件</span>
              <span className="search-results-section-count">{suggestions.length}</span>
            </div>
            <div className="search-file-suggestions">
              {suggestions.map((file) => {
                const index = nextIndex();
                return (
                  <button
                    key={file.path}
                    type="button"
                    className={`search-file-suggestion ${
                      index === activeIndex ? "is-active" : ""
                    }`}
                    data-search-item-index={index}
                    onMouseEnter={() => onActiveIndexChange?.(index)}
                    onClick={() => openItem({ kind: "file", path: file.path, name: file.name })}
                  >
                    <img className="search-result-icon" src={getFileIcon(file.path)} alt="" />
                    <span className="search-file-suggestion-copy">
                      <span className="search-file-suggestion-name">
                        {highlightText(file.name, highlightQuery)}
                      </span>
                      <span className="search-file-suggestion-path">
                        {highlightText(file.path, highlightQuery)}
                      </span>
                    </span>
                  </button>
                );
              })}
            </div>
          </section>
        )}
        {hasContentMatches && (
          <section className="search-results-section">
            <div className="search-results-header">
              <span className="search-results-count">
                内容匹配
                <span className="search-results-section-count">
                  {result.total_matches}
                </span>
              </span>
              <span className="search-results-meta">
                {result.files.length} 个文件
                {result.truncated && <span className="search-results-truncated">已截断</span>}
              </span>
            </div>
            <div className="search-results-list">
              {result.files.map((file) => {
                const visible = file.matches.slice(0, MAX_VISIBLE_MATCHES);
                const remaining = file.matches.length - MAX_VISIBLE_MATCHES;
                const fileName = file.path.split(/[/\\]/).pop() || file.path;
                const fileIndex = nextIndex();
                return (
                  <div key={file.path} className="search-results-file">
                    <button
                      type="button"
                      className={`search-results-file-header ${
                        fileIndex === activeIndex ? "is-active" : ""
                      }`}
                      data-search-item-index={fileIndex}
                      onMouseEnter={() => onActiveIndexChange?.(fileIndex)}
                      onClick={() =>
                        openItem({
                          kind: "file",
                          path: file.path,
                          name: fileName,
                        })
                      }
                    >
                      <img className="search-result-icon" src={getFileIcon(file.path)} alt="" />
                      <span className="search-results-file-copy">
                        <span className="search-results-file-name">
                          {highlightText(fileName, highlightQuery)}
                        </span>
                        <span className="search-results-file-path">
                          {highlightText(file.path, highlightQuery)}
                        </span>
                      </span>
                      <span className="search-results-file-count">{file.matches.length}</span>
                    </button>
                    {visible.map((match, idx) => {
                      const matchIndex = nextIndex();
                      return (
                        <button
                          key={idx}
                          type="button"
                          className={`search-results-match ${
                            matchIndex === activeIndex ? "is-active" : ""
                          }`}
                          data-search-item-index={matchIndex}
                          onMouseEnter={() => onActiveIndexChange?.(matchIndex)}
                          onClick={() =>
                            openItem({
                              kind: "match",
                              path: file.path,
                              lineNumber: match.line_number,
                              lineText: match.line_text,
                            })
                          }
                        >
                          <span className="search-results-line-num">{match.line_number}</span>
                          <span className="search-results-line-text">
                            {highlightText(match.line_text, highlightQuery)}
                          </span>
                        </button>
                      );
                    })}
                    {remaining > 0 && (
                      <button
                        type="button"
                        className="search-results-more"
                        onClick={() => {
                          onFileOpen(file.path, file.matches[0]?.line_number, result.query);
                          onClose();
                        }}
                      >
                        查看其余 {remaining} 个匹配
                      </button>
                    )}
                  </div>
                );
              })}
            </div>
          </section>
        )}
        {selectableItems.length > 0 && (
          <div className="search-results-footer">
            <span>
              <kbd>↑</kbd>
              <kbd>↓</kbd>
              选择
            </span>
            <span>
              <kbd>↵</kbd>
              打开
            </span>
            <span>
              <kbd>esc</kbd>
              关闭
            </span>
          </div>
        )}
      </>
    );
  })();

  if (!body) return null;

  const dropdown = (
    <div className={`search-results-dropdown ${inline ? "is-inline" : ""}`} style={style}>
      {body}
    </div>
  );

  if (inline) return dropdown;
  return createPortal(dropdown, document.body);
}

export function getSearchSelectableCount(result: SearchResult | null): number {
  if (!result) return 0;
  let count = (result.file_suggestions ?? []).length;
  for (const file of result.files) {
    count += 1 + Math.min(file.matches.length, MAX_VISIBLE_MATCHES);
  }
  return count;
}

export function openSearchSelectableItem(
  result: SearchResult,
  index: number,
  onFileOpen: (filePath: string, lineNumber?: number, searchQuery?: string) => void,
): boolean {
  const items: SelectableItem[] = [];
  for (const file of result.file_suggestions ?? []) {
    items.push({ kind: "file", path: file.path, name: file.name });
  }
  for (const file of result.files) {
    items.push({
      kind: "file",
      path: file.path,
      name: file.path.split(/[/\\]/).pop() || file.path,
    });
    for (const match of file.matches.slice(0, MAX_VISIBLE_MATCHES)) {
      items.push({
        kind: "match",
        path: file.path,
        lineNumber: match.line_number,
        lineText: match.line_text,
      });
    }
  }
  const item = items[index];
  if (!item) return false;
  if (item.kind === "file") {
    onFileOpen(item.path);
  } else {
    onFileOpen(item.path, item.lineNumber, result.query);
  }
  return true;
}

function highlightText(text: string, query: string): ReactNode {
  const q = query.trim();
  if (!q) return text;
  const lowerText = text.toLowerCase();
  const lowerQuery = q.toLowerCase();
  const parts: ReactNode[] = [];
  let cursor = 0;
  let matchIndex = lowerText.indexOf(lowerQuery);
  let key = 0;
  while (matchIndex >= 0) {
    if (matchIndex > cursor) {
      parts.push(text.slice(cursor, matchIndex));
    }
    parts.push(
      <mark key={`h-${key++}`} className="search-result-highlight">
        {text.slice(matchIndex, matchIndex + q.length)}
      </mark>,
    );
    cursor = matchIndex + q.length;
    matchIndex = lowerText.indexOf(lowerQuery, cursor);
  }
  if (cursor < text.length) {
    parts.push(text.slice(cursor));
  }
  return parts.length > 0 ? parts : text;
}

function ExternalLink({ href, children }: { href: string; children: string }) {
  const open = (event: { preventDefault: () => void }) => {
    event.preventDefault();
    // Right panel's built-in browser; a web link is never handed to the system
    // browser.
    void openLinkInPanelOrExternal(href);
  };
  return (
    <a
      className="search-results-link"
      href={href}
      onClick={open}
      onAuxClick={(event) => {
        // A middle click takes the same route as the left button; left to the
        // browser it is a new-window request the shell suppresses.
        if (event.button === 1) open(event);
      }}
      rel="noreferrer"
      target="_blank"
    >
      {children}
    </a>
  );
}

function LinkifiedText({ text }: { text: string }) {
  const parts = text.split(/(https?:\/\/[^\s]+)/g);
  return (
    <>
      {parts.map((part, index) =>
        /^https?:\/\//.test(part) ? (
          <ExternalLink key={`${part}-${index}`} href={part}>
            {part}
          </ExternalLink>
        ) : (
          <span key={`${part}-${index}`}>{part}</span>
        ),
      )}
    </>
  );
}
