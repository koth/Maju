import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Children, isValidElement, memo, useEffect, useState, type ReactNode } from "react";
import { Prism as SyntaxHighlighter } from "react-syntax-highlighter";
import { oneLight, vscDarkPlus } from "react-syntax-highlighter/dist/esm/styles/prism";
import { getAppliedAppTheme } from "../../theme";

interface Props {
  content: string;
}

function MarkdownBody({ content }: Props) {
  const appTheme = useCurrentAppTheme();
  const codeTheme = appTheme === "light" ? oneLight : vscDarkPlus;
  const displayContent = repairCompactMarkdown(content);

  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm]}
      urlTransform={safeMarkdownUrl}
      components={{
        code({ className, children, ...props }) {
          const match = /language-(\w+)/.exec(className || "");
          const codeString = (children == null ? "" : String(children)).replace(/\n$/, "");

          if (match) {
            if (codeString.trim().length === 0) {
              return null;
            }
            return (
              <div className="md-code-block">
                <div className="md-code-header">
                  <span className="md-code-lang">{match[1]}</span>
                </div>
                <SyntaxHighlighter
                  style={codeTheme}
                  language={match[1]}
                  PreTag="div"
                  customStyle={{
                    margin: 0,
                    padding: "12px",
                    borderRadius: "0 0 10px 10px",
                    fontSize: "13px",
                    lineHeight: "1.5",
                    color: "var(--md-code-pre-text, inherit)",
                    background: "var(--md-code-block-bg, var(--app-bg))",
                    backgroundColor: "var(--md-code-block-bg, var(--app-bg))",
                  }}
                >
                  {codeString}
                </SyntaxHighlighter>
              </div>
            );
          }

          return (
            <code className="md-inline-code" {...props}>
              {children}
            </code>
          );
        },
        p({ children }) {
          const imageOnly = isImageOnlyParagraph(children);
          return (
            <p className={imageOnly ? "md-paragraph md-image-paragraph" : "md-paragraph"}>
              {children}
            </p>
          );
        },
        ul({ children }) {
          return <ul className="md-list">{children}</ul>;
        },
        ol({ children }) {
          return <ol className="md-list md-list-ordered">{children}</ol>;
        },
        li({ children }) {
          return <li className="md-list-item">{children}</li>;
        },
        h1({ children }) {
          return <h1 className="md-heading md-h1">{children}</h1>;
        },
        h2({ children }) {
          return <h2 className="md-heading md-h2">{children}</h2>;
        },
        h3({ children }) {
          return <h3 className="md-heading md-h3">{children}</h3>;
        },
        blockquote({ children }) {
          return <blockquote className="md-blockquote">{children}</blockquote>;
        },
        hr() {
          return <hr className="md-hr" />;
        },
        a({ href, children }) {
          return (
            <a className="md-link" href={href} target="_blank" rel="noopener noreferrer">
              {children}
            </a>
          );
        },
        img({ src, alt }) {
          return <img className="md-image" src={src} alt={alt ?? "附加的图片"} />;
        },
        strong({ children }) {
          return <strong className="md-bold">{children}</strong>;
        },
        table({ children }) {
          return (
            <div className="md-table-wrap">
              <table className="md-table">{children}</table>
            </div>
          );
        },
        thead({ children }) {
          return <thead className="md-thead">{children}</thead>;
        },
        tbody({ children }) {
          return <tbody className="md-tbody">{children}</tbody>;
        },
        tr({ children }) {
          return <tr className="md-tr">{children}</tr>;
        },
        th({ children }) {
          return <th className="md-th">{children}</th>;
        },
        td({ children }) {
          return <td className="md-td">{children}</td>;
        },
      }}
    >
      {displayContent}
    </ReactMarkdown>
  );
}

export default memo(MarkdownBody);

function useCurrentAppTheme() {
  const [theme, setTheme] = useState(() => getAppliedAppTheme());

  useEffect(() => {
    const root = document.documentElement;
    const observer = new MutationObserver(() => setTheme(getAppliedAppTheme()));
    observer.observe(root, { attributes: true, attributeFilter: ["data-theme"] });
    return () => observer.disconnect();
  }, []);

  return theme;
}

function safeMarkdownUrl(url: string) {
  if (/^data:image\/(png|jpeg|jpg|gif|webp);base64,[a-z0-9+/=]+$/i.test(url)) {
    return url;
  }
  if (/^(https?:|mailto:)/i.test(url) || url.startsWith("/") || url.startsWith("#")) {
    return url;
  }
  return "";
}

function isImageOnlyParagraph(children: ReactNode) {
  const meaningfulChildren = Children.toArray(children).filter(
    (child) => !(typeof child === "string" && child.trim() === ""),
  );
  return (
    meaningfulChildren.length > 0 &&
    meaningfulChildren.every(isMarkdownImageElement)
  );
}

function isMarkdownImageElement(child: ReactNode) {
  if (!isValidElement<{ className?: string; src?: string }>(child)) {
    return false;
  }
  return child.props.className === "md-image" || child.type === "img" || Boolean(child.props.src);
}

function repairCompactMarkdown(content: string) {
  const lines = content.split(/\r?\n/);
  let inFence = false;
  const repaired: string[] = [];

  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    if (/^\s*(```|~~~)/.test(line)) {
      inFence = !inFence;
      repaired.push(line);
      continue;
    }
    if (inFence) {
      repaired.push(line);
      continue;
    }

    const nextLine = lines[index + 1];
    if (nextLine !== undefined) {
      const compactTable = repairSplitCompactMarkdownTable(line, nextLine);
      if (compactTable !== null) {
        repaired.push(compactTable);
        index += 1;
        continue;
      }
    }

    repaired.push(repairCompactMarkdownLine(line));
  }

  return repaired.join("\n");
}

function repairCompactMarkdownLine(line: string) {
  return repairCompactMarkdownTable(line)
    .replace(/^(\s{0,3}#{1,6})(?=\S)/u, "$1 ")
    .replace(
      /([^\s\n])(\d{1,2}\.\s+(?=(?:\*\*)?[\p{Script=Han}A-Za-z]))/gu,
      "$1\n$2",
    );
}

function repairCompactMarkdownTable(line: string) {
  if ((!line.includes("||") && !/\|\s+\|/u.test(line)) || countChars(line, "|") < 6) {
    return line;
  }

  const headingMatch = line.match(/^(\s{0,3}#{1,6}[^|]+)(\|.+)$/u);
  const prefix = headingMatch ? `${headingMatch[1]}\n\n` : "";
  const tableText = headingMatch ? headingMatch[2] : line;
  const rows = compactMarkdownTableRows(tableText);

  if (rows.length < 2 || !/^\|?\s*:?-{3,}:?\s*(\|\s*:?-{3,}:?\s*)+\|?$/u.test(rows[1])) {
    return line;
  }

  return `${prefix}${rows.join("\n")}`;
}

function repairSplitCompactMarkdownTable(headerLine: string, bodyLine: string) {
  if (!bodyLine.includes("|") || !/^\s*\|?\s*:?-{3,}:?\s*\|/u.test(bodyLine)) {
    return null;
  }

  const headerMatch = headerLine.match(/^(.+?)(\|[^|]+(?:\|[^|]+)+\|?)\s*$/u);
  if (!headerMatch) {
    return null;
  }

  const prefix = headerMatch[1].trimEnd();
  const headerRow = normalizeMarkdownTableRow(headerMatch[2]);
  const rows = [headerRow, ...compactMarkdownTableRows(bodyLine)];
  if (rows.length < 3 || !/^\|?\s*:?-{3,}:?\s*(\|\s*:?-{3,}:?\s*)+\|?$/u.test(rows[1])) {
    return null;
  }

  const repairedPrefix = prefix
    ? `${prefix.replace(/^(\s{0,3}#{1,6})(?=\S)/u, "$1 ")}\n\n`
    : "";
  return `${repairedPrefix}${rows.join("\n")}`;
}

function compactMarkdownTableRows(tableText: string) {
  return tableText
    .replace(/\|\s+\|(?=\s*[^|\s])/gu, "||")
    .split("||")
    .map((row) => row.trim())
    .filter(Boolean)
    .map(normalizeMarkdownTableRow);
}

function normalizeMarkdownTableRow(row: string) {
  const normalized = row.startsWith("|") ? row : `|${row}`;
  return normalized.endsWith("|") ? normalized : `${normalized}|`;
}

function countChars(value: string, char: string) {
  return [...value].filter((current) => current === char).length;
}
