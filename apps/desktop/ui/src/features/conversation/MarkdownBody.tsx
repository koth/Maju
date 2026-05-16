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

  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm]}
      urlTransform={safeMarkdownUrl}
      components={{
        code({ className, children, ...props }) {
          const match = /language-(\w+)/.exec(className || "");
          const codeString = String(children).replace(/\n$/, "");

          if (match) {
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
      {content}
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
