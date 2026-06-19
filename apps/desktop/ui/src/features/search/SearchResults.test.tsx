import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { openExternalUrl } from "../../lib/tauri";
import type { SearchResult } from "../../types";
import { SearchResults } from "./SearchResults";

vi.mock("../../lib/tauri", async () => {
  const actual = await vi.importActual<typeof import("../../lib/tauri")>("../../lib/tauri");
  return {
    ...actual,
    openExternalUrl: vi.fn(),
  };
});

function renderResults(result: SearchResult, onFileOpen = vi.fn()) {
  const onClose = vi.fn();
  render(
    <SearchResults
      result={result}
      loading={false}
      error={null}
      onFileOpen={onFileOpen}
      onClose={onClose}
    />,
  );
  return { onClose, onFileOpen };
}

describe("SearchResults", () => {
  afterEach(() => {
    cleanup();
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

    const suggestionTitle = screen.getByText("文件名匹配");
    const contentHeader = screen.getByText("在 1 个文件中找到 1 个匹配");
    expect(
      suggestionTitle.compareDocumentPosition(contentHeader) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /SearchResults\.tsx/ }));

    expect(onFileOpen).toHaveBeenCalledWith("src/features/search/SearchResults.tsx");
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("opens notice urls externally", () => {
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

    expect(openExternalUrl).toHaveBeenCalledWith(
      "https://github.com/BurntSushi/ripgrep#installation",
    );
  });
});
