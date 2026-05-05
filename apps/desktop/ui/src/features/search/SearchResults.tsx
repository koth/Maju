import { createPortal } from "react-dom";
import type { SearchResult } from "../../types";
import "./SearchResults.css";

const MAX_VISIBLE_MATCHES = 3;

interface Props {
  result: SearchResult | null;
  loading: boolean;
  error: string | null;
  onFileOpen: (filePath: string, lineNumber?: number, searchQuery?: string) => void;
  onClose: () => void;
}

export function SearchResults({ result, loading, error, onFileOpen, onClose }: Props) {
  const dropdown = (() => {
    if (loading) {
      return (
        <div className="search-results-dropdown">
          <div className="search-results-status">搜索中...</div>
        </div>
      );
    }

    if (error) {
      return (
        <div className="search-results-dropdown">
          <div className="search-results-error">{error}</div>
        </div>
      );
    }

    if (!result) return null;

    if (result.files.length === 0) {
      return (
        <div className="search-results-dropdown">
          <div className="search-results-status">未找到结果</div>
        </div>
      );
    }

    return (
      <div className="search-results-dropdown">
        <div className="search-results-header">
          <span className="search-results-count">
            在 {result.files.length} 个文件中找到 {result.total_matches} 个匹配
          </span>
          {result.truncated && (
            <span className="search-results-truncated">结果已截断</span>
          )}
        </div>
        <div className="search-results-list">
          {result.files.map((file) => {
            const visible = file.matches.slice(0, MAX_VISIBLE_MATCHES);
            const remaining = file.matches.length - MAX_VISIBLE_MATCHES;
            return (
              <div key={file.path} className="search-results-file">
                <div
                  className="search-results-file-header"
                  onClick={() => {
                    onFileOpen(file.path, file.matches[0]?.line_number, result.query);
                    onClose();
                  }}
                >
                  {file.path}
                  <span className="search-results-file-count">{file.matches.length}</span>
                </div>
                {visible.map((match, idx) => (
                  <div
                    key={idx}
                    className="search-results-match"
                    onClick={() => {
                      onFileOpen(file.path, match.line_number, result.query);
                      onClose();
                    }}
                  >
                    <span className="search-results-line-num">{match.line_number}</span>
                    <span className="search-results-line-text">{match.line_text}</span>
                  </div>
                ))}
                {remaining > 0 && (
                  <div
                    className="search-results-more"
                    onClick={() => {
                      onFileOpen(file.path, file.matches[0]?.line_number, result.query);
                      onClose();
                    }}
                  >
                    ...剩余 {remaining} 个匹配
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </div>
    );
  })();

  if (!dropdown) return null;
  return createPortal(dropdown, document.body);
}
