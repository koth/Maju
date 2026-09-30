import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { openExternalUrl } from "../../lib/tauri";
import { registerPanelOpener } from "../browser/browserTabs";
import type { SearchResult } from "../../types";
import { SearchResults } from "./SearchResults";

vi.mock("../../lib/tauri", async () => {
  const actual = await vi.importActual<typeof import("../../lib/tauri")>("../../lib/tauri");
  return {
    ...actual,
    openExternalUrl: vi.fn(),
  };
});

vi.mock("../filetree/file-icons", () => ({
  getFileIcon: () => "icon.svg",
}));

function renderResults(result: SearchResult, onFileOpen = vi.fn()) {
  const onClose = vi.fn();
  render(
    <SearchResults
      result={result}
      loading={false}
      error={null}
      onFileOpen={onFileOpen}
      onClose={onClose}
      placement="inline"
    />,
  );
  return { onClose, onFileOpen };
}

describe("SearchResults", () => {
  afterEach(() => {
    cleanup();
    registerPanelOpener(null);
    vi.clearAllMocks();
  });

  it("shows file name suggestions before content matches and opens the selected file", () => {
    const onFileOpen = vi.fn();
    const { onClose } = renderResults(
      {
        query: "search",
        file_suggestions: [
          { path: "src/features/search/SearchResults.tsx", name: "SearchResults.tsx" },
        ],
        files: [
          {
            path: "src/features/workbench/GlobalChrome.tsx",
            matches: [{ line_number: 12, line_text: "const searchTitle = '搜索工作区';" }],
          },
        ],
        total_matches: 1,
        truncated: false,
      },
      onFileOpen,
    );

    const suggestionTitle = screen.getByText("文件");
    const contentHeader = screen.getByText("内容匹配");
    expect(
      suggestionTitle.compareDocumentPosition(contentHeader) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /SearchResults\.tsx/ }));

    expect(onFileOpen).toHaveBeenCalledWith("src/features/search/SearchResults.tsx");
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("highlights the query text inside file names", () => {
    renderResults({
      query: "Search",
      file_suggestions: [
        { path: "src/features/search/SearchResults.tsx", name: "SearchResults.tsx" },
      ],
      files: [],
      total_matches: 0,
      truncated: false,
    });

    const marks = screen.getAllByText("Search");
    expect(marks.length).toBeGreaterThan(0);
    expect(marks.every((node) => node.tagName === "MARK")).toBe(true);
  });

  it("routes notice urls to the right-panel browser", () => {
    const opener = vi.fn(async () => true);
    registerPanelOpener(opener);

    renderResults({
      query: "search",
      file_suggestions: [],
      files: [],
      total_matches: 0,
      truncated: false,
      notice: {
        message: "未检测到 ripgrep (rg)，内容搜索不可用。安装说明：",
        url: "https://github.com/BurntSushi/ripgrep#installation",
        url_label: "https://github.com/BurntSushi/ripgrep#installation",
      },
    });

    fireEvent.click(screen.getByRole("link", { name: /ripgrep#installation/ }));

    // Web links go to the right panel's browser; the system browser is never a
    // destination for them.
    expect(opener).toHaveBeenCalledWith("https://github.com/BurntSushi/ripgrep#installation");
    expect(openExternalUrl).not.toHaveBeenCalled();
  });
});
