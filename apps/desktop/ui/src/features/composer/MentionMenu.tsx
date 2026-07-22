import { createPortal } from "react-dom";
import type { FileEntry } from "../../types";
import { getFileIcon, getFolderIcon } from "../filetree/file-icons";
import type { MentionAnchor, MentionKind } from "./use-mention";
import "./MentionMenu.css";

interface Props {
  items: FileEntry[];
  activeIndex: number;
  loading: boolean;
  query: string;
  dirPart: string;
  prefix: string;
  anchor: MentionAnchor | null;
  onConfirm: (index: number) => void;
  onHover: (index: number) => void;
}

const FALLBACK_ANCHOR: MentionAnchor = { left: 16, bottom: 240 };

function Highlighted({ text, needle }: { text: string; needle: string }) {
  const needleTrim = needle.trim();
  if (!needleTrim) return <>{text}</>;
  const lower = text.toLowerCase();
  const target = needleTrim.toLowerCase();
  const nodes: React.ReactNode[] = [];
  let i = 0;
  let key = 0;
  while (i < text.length) {
    const idx = lower.indexOf(target, i);
    if (idx < 0) {
      nodes.push(text.slice(i));
      break;
    }
    if (idx > i) nodes.push(text.slice(i, idx));
    nodes.push(<mark key={key++}>{text.slice(idx, idx + target.length)}</mark>);
    i = idx + target.length;
  }
  return <>{nodes}</>;
}

function parentDir(path: string, name: string): string {
  if (path === name) return "";
  const trimmed = path.endsWith(name) ? path.slice(0, path.length - name.length) : path;
  return trimmed.replace(/\/+$/, "");
}

export function MentionMenu({
  items,
  activeIndex,
  loading,
  query,
  dirPart,
  prefix,
  anchor,
  onConfirm,
  onHover,
}: Props) {
  const drill = query.includes("/");
  const highlight = drill ? prefix : query;
  const pos = anchor ?? FALLBACK_ANCHOR;

  const headerLabel = drill
    ? dirPart || "/"
    : "引用文件或目录";

  const isEmpty = !loading && items.length === 0;
  return createPortal(
    <div
      className="mention-menu"
      role="listbox"
      aria-label="引用文件或目录"
      style={{ left: `${pos.left}px`, bottom: `${pos.bottom}px` }}
    >
      <div className="mention-menu-header">
        <span className="mention-menu-label" title={headerLabel}>
          {headerLabel}
        </span>
        <span className="mention-menu-count">
          {loading ? "搜索中…" : items.length > 0 ? `${items.length} 项` : ""}
        </span>
      </div>

      {isEmpty ? (
        <div className="mention-menu-empty">
          <div className="mention-menu-empty-title">未找到匹配</div>
          <div className="mention-menu-empty-hint">
            {query ? "试试输入路径，如 src/" : "输入文件名或路径开始引用"}
          </div>
        </div>
      ) : (
        <div className="mention-menu-list">
          {items.map((item, index) => {
            const isDir = item.kind === "Directory";
            const icon = isDir ? getFolderIcon(item.name, false) : getFileIcon(item.path);
            const breadcrumb = parentDir(item.path, item.name);
            return (
              <button
                key={`${item.kind}:${item.path}`}
                type="button"
                role="option"
                aria-selected={index === activeIndex}
                className={`mention-menu-item${index === activeIndex ? " is-active" : ""}`}
                onMouseDown={(e) => {
                  e.preventDefault();
                  onConfirm(index);
                }}
                onMouseEnter={() => onHover(index)}
              >
                <img className="mention-menu-icon" src={icon} alt="" />
                <span className="mention-menu-name">
                  <Highlighted text={item.name} needle={highlight} />
                </span>
                {breadcrumb ? (
                  <span className="mention-menu-path" title={breadcrumb}>
                    {breadcrumb}
                  </span>
                ) : null}
                {isDir ? <span className="mention-menu-chevron" aria-hidden>›</span> : null}
              </button>
            );
          })}
        </div>
      )}

      <div className="mention-menu-footer">
        <span className="mention-menu-hint">
          <kbd>↑</kbd>
          <kbd>↓</kbd>
          选择
        </span>
        <span className="mention-menu-hint">
          <kbd>↵</kbd>
          确认
        </span>
        <span className="mention-menu-hint">
          <kbd>esc</kbd>
          关闭
        </span>
      </div>
    </div>,
    document.body,
  );
}

export type { MentionKind };
