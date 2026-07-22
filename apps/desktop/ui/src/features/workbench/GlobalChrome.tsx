import { useState, useRef, useEffect, useCallback } from "react";
import type { WorkspaceDescriptor, SearchResult } from "../../types";
import { fsSearch } from "../../lib/tauri";
import { isMacOS } from "../../lib/platform";
import { SearchResults } from "../search/SearchResults";
import { WindowControls } from "./WindowControls";

interface Props {
  workspace: WorkspaceDescriptor;
  remoteWorkspace: boolean;
  sidebarCollapsed: boolean;
  refreshing: boolean;
  rightPanelCollapsed: boolean;
  terminalDockVisible: boolean;
  onToggleSidebar: () => void;
  onToggleTerminal: () => void;
  onRefreshGit: () => void;
  onToggleRightPanel: () => void;
  onFileOpen: (filePath: string, lineNumber?: number, searchQuery?: string) => void;
}

export function GlobalChrome({
  workspace: _workspace,
  remoteWorkspace,
  sidebarCollapsed,
  refreshing,
  rightPanelCollapsed,
  terminalDockVisible,
  onToggleSidebar,
  onToggleTerminal,
  onRefreshGit,
  onToggleRightPanel,
  onFileOpen,
}: Props) {
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [searchResult, setSearchResult] = useState<SearchResult | null>(null);
  const [searchLoading, setSearchLoading] = useState(false);
  const [searchError, setSearchError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const searchContainerRef = useRef<HTMLDivElement>(null);
  const searchRequestRef = useRef(0);

  const closeSearch = useCallback(() => {
    searchRequestRef.current += 1;
    setSearchOpen(false);
    setSearchResult(null);
    setSearchError(null);
    setSearchLoading(false);
  }, []);

  const toggleSearch = useCallback(() => {
    if (searchOpen) {
      closeSearch();
    } else {
      setSearchOpen(true);
    }
  }, [searchOpen, closeSearch]);

  useEffect(() => {
    if (searchOpen && inputRef.current) {
      inputRef.current.focus();
    }
  }, [searchOpen]);

  useEffect(() => {
    if (!searchOpen) return;
    const handleClick = (e: MouseEvent) => {
      const target = e.target as Node;
      // Ignore clicks inside the search container or the portal dropdown
      if (searchContainerRef.current && searchContainerRef.current.contains(target)) return;
      const dropdown = document.querySelector(".search-results-dropdown");
      if (dropdown && dropdown.contains(target)) return;
      closeSearch();
    };
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [searchOpen, closeSearch]);

  const runSearch = useCallback(async (q: string) => {
    const requestId = ++searchRequestRef.current;
    setSearchLoading(true);
    setSearchError(null);
    setSearchResult(null);
    try {
      const result = await fsSearch(q);
      if (searchRequestRef.current === requestId) {
        setSearchResult(result);
      }
    } catch (err) {
      if (searchRequestRef.current === requestId) {
        setSearchError(String(err));
      }
    } finally {
      if (searchRequestRef.current === requestId) {
        setSearchLoading(false);
      }
    }
  }, []);

  useEffect(() => {
    if (!searchOpen) return;
    const q = searchQuery.trim();
    if (!q) {
      searchRequestRef.current += 1;
      setSearchResult(null);
      setSearchError(null);
      setSearchLoading(false);
      return;
    }
    const timer = window.setTimeout(() => {
      void runSearch(q);
    }, 220);
    return () => window.clearTimeout(timer);
  }, [runSearch, searchOpen, searchQuery]);

  const handleSearchSubmit = useCallback(async () => {
    const q = searchQuery.trim();
    if (!q) return;
    await runSearch(q);
  }, [runSearch, searchQuery]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Enter") {
        handleSearchSubmit();
      } else if (e.key === "Escape") {
        closeSearch();
      }
    },
    [handleSearchSubmit, closeSearch],
  );

  const showDropdown = searchOpen && (searchLoading || searchError !== null || searchResult !== null);
  const chromeClassName = `global-chrome ${isMacOS() ? "is-macos" : ""}`;
  const terminalTitle = terminalDockVisible
    ? "隐藏终端"
    : remoteWorkspace
      ? "打开远程终端"
      : "打开终端";
  const searchTitle = "搜索工作区";
  const gitRefreshTitle = "刷新 Git 状态";

  return (
    <header className={chromeClassName} data-tauri-drag-region>
      <div className="global-chrome-left">
        <button
          type="button"
          className={`chrome-icon-btn chrome-sidebar-toggle ${sidebarCollapsed ? "" : "is-active"}`}
          onClick={onToggleSidebar}
          title={sidebarCollapsed ? "显示项目栏" : "隐藏项目栏"}
          aria-label={sidebarCollapsed ? "显示项目栏" : "隐藏项目栏"}
          aria-pressed={!sidebarCollapsed}
        >
          <LeftSidebarIcon />
        </button>
      </div>
      <div className="global-chrome-actions">
        <button
          type="button"
          className={`chrome-icon-btn ${terminalDockVisible ? "is-active" : ""}`}
          onClick={onToggleTerminal}
          title={terminalTitle}
          aria-label={terminalTitle}
          aria-pressed={terminalDockVisible}
        >
          <TerminalIcon />
        </button>
        <div className="chrome-search-container" ref={searchContainerRef}>
          <button
            type="button"
            className={`chrome-icon-btn ${searchOpen ? "is-active" : ""}`}
            onClick={toggleSearch}
            title={searchTitle}
            aria-label={searchTitle}
          >
            <SearchIcon />
          </button>
          {searchOpen && (
            <input
              ref={inputRef}
              type="text"
              className="chrome-search-input"
              placeholder="搜索文件..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              onKeyDown={handleKeyDown}
            />
          )}
          {showDropdown && (
            <SearchResults
              result={searchResult}
              loading={searchLoading}
              error={searchError}
              onFileOpen={onFileOpen}
              onClose={closeSearch}
            />
          )}
        </div>
        <button
          type="button"
          className="chrome-icon-btn"
          onClick={onRefreshGit}
          disabled={refreshing}
          title={gitRefreshTitle}
          aria-label={gitRefreshTitle}
        >
          <GitBranchIcon />
        </button>
        <button
          type="button"
          className={`chrome-icon-btn ${rightPanelCollapsed ? "" : "is-active"}`}
          onClick={onToggleRightPanel}
          title={rightPanelCollapsed ? "显示右侧栏" : "隐藏右侧栏"}
          aria-label={rightPanelCollapsed ? "显示右侧栏" : "隐藏右侧栏"}
          aria-pressed={!rightPanelCollapsed}
        >
          <RightSidebarIcon />
        </button>
        <WindowControls />
      </div>
    </header>
  );
}

function SearchIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="11" cy="11" r="7" />
      <path d="m16 16 5 5" />
    </svg>
  );
}

function LeftSidebarIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <rect x="4" y="5" width="16" height="14" rx="2" />
      <path d="M9 5v14" />
      <path d="M6.5 9h1" />
      <path d="M6.5 12h1" />
      <path d="M6.5 15h1" />
    </svg>
  );
}

function GitBranchIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M7 5v10a4 4 0 0 0 4 4h2" />
      <path d="M17 7a4 4 0 0 1-4 4H7" />
      <circle cx="7" cy="5" r="2" />
      <circle cx="7" cy="19" r="2" />
      <circle cx="17" cy="7" r="2" />
    </svg>
  );
}

function TerminalIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <rect x="4" y="5" width="16" height="14" rx="2" />
      <path d="m8 10 3 2-3 2" />
      <path d="M13 15h3" />
    </svg>
  );
}

function RightSidebarIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <rect x="4" y="5" width="16" height="14" rx="2" />
      <path d="M15 5v14" />
      <path d="M17.5 9h-1" />
      <path d="M17.5 12h-1" />
      <path d="M17.5 15h-1" />
    </svg>
  );
}
