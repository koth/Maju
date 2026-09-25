import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import rehypeKatex from "rehype-katex";
// KaTeX ships its own metrics and embedded fonts; the theme-neutral
// reconciliations with the chat surface live in MarkdownBody.css.
import "katex/dist/katex.min.css";
import "./MarkdownBody.css";
import { Check, Copy, FileCode } from "lucide-react";
import {
  Children,
  isValidElement,
  memo,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ComponentProps,
  type CSSProperties,
  type ReactNode,
} from "react";
import { Prism as SyntaxHighlighter } from "react-syntax-highlighter";
import { oneLight, vscDarkPlus } from "react-syntax-highlighter/dist/esm/styles/prism";

/** Prism theme objects are module-level singletons, so their identity is
 *  stable across renders — required for the memoized code block below. */
type MarkdownCodeTheme = Record<string, CSSProperties>;

/** Memoized fenced-code renderer. All props are primitives plus a stable
 *  module-level theme object, so React bails out for every UNCHANGED code
 *  block when react-markdown rebuilds the AST. This is the streaming hot
 *  path: each throttled commit of a growing reply re-parses the whole body,
 *  and without this memo Prism re-highlighted EVERY code block in it
 *  (hundreds of ms of main-thread work per commit once a long dsh reply
 *  accumulates dozens of large code blocks). With the memo, only the tail
 *  block that actually grew re-highlights. */
const MarkdownCodeBlock = memo(function MarkdownCodeBlock({
  language,
  code,
  theme,
}: {
  language: string;
  code: string;
  theme: MarkdownCodeTheme;
}) {
  return (
    <div className="md-code-block">
      <div className="md-code-header">
        <span className="md-code-lang">{language}</span>
        <CopyCodeButton text={code} />
      </div>
      <SyntaxHighlighter
        style={theme}
        language={language}
        PreTag="div"
        customStyle={{
          margin: 0,
          padding: "12px 12px 12px 0",
          borderRadius: "0 0 10px 10px",
          fontSize: "13px",
          lineHeight: "1.5",
          color: "var(--md-code-pre-text, inherit)",
          background: "var(--md-code-block-bg, var(--app-bg))",
          backgroundColor: "var(--md-code-block-bg, var(--app-bg))",
        }}
      >
        {code}
      </SyntaxHighlighter>
    </div>
  );
});
import { useCurrentAppTheme } from "../../lib/use-app-theme";
import { stripWorkspaceRootPrefix } from "../filetree/FileTree";
import { maskMarkdownMath } from "./markdown-math";

interface Props {
  content: string;
  /** Absolute workspace root used to resolve relative file paths in messages. */
  workspaceRoot?: string;
  /** Called when the user clicks an inline-code file path (`crates/foo.rs:75`). */
  onFilePathClick?: (filePath: string, lineNumber?: number) => void;
  /** Paths of files in the current git changeset — the strongest signal for
   *  resolving bare file names, since the assistant usually discusses files
   *  it just changed. */
  changedFiles?: string[];
  /** Paths collected from the current turn (shell commands, tool outputs,
   *  turn file changes). Used as the second-priority match source after the
   *  git changeset; candidates are matched as whole trailing segments, never
   *  by basename alone. Readonly: the timeline passes pool-owned arrays, and
   *  identity stability is what keeps memoized rows from re-rendering on
   *  every streaming delta. */
  candidatePaths?: readonly string[];
  /** Called when a markdown image is clicked; omitted in non-chat surfaces. */
  onImagePreview?: (src: string, alt?: string) => void;
}

/** Value-keyed per-block compact-repair cache. Blocks are stable strings
 *  across streaming commits, so every finished block repairs once and all
 *  later commits hit the cache. */
const repairBlockCache = new Map<string, string>();
const REPAIR_BLOCK_CACHE_MAX = 4096;

function repairBlockCached(block: string): string {
  const cached = repairBlockCache.get(block);
  if (cached !== undefined) return cached;
  const repaired = repairCompactMarkdownBlockLines(block);
  if (repairBlockCache.size >= REPAIR_BLOCK_CACHE_MAX) {
    repairBlockCache.clear();
  }
  repairBlockCache.set(block, repaired);
  return repaired;
}

type MarkdownComponents = ComponentProps<typeof ReactMarkdown>["components"];

/** Split markdown into top-level blocks at blank lines so each block can be
 *  parsed and memoized independently (see MarkdownSection). Fence-aware:
 *  blank lines inside ``` / ~~~ fences never split. Continuation-aware: a
 *  block whose first line continues the previous construct (another list
 *  item, a blockquote line, an indented line) is JOINED into the previous
 *  block, so loose lists keep their numbering and blockquotes stay one
 *  element. The joined ranges concatenate back to the exact original text. */
export function splitMarkdownBlocks(content: string): string[] {
  if (content.length === 0) return [""];
  const lines = content.split("\n");
  const blocks: string[] = [];
  let start = 0;
  let fence: string | null = null;
  const fenceMarkerOf = (line: string): string | null => {
    const match = /^\s{0,3}(```+|~~~+)/.exec(line);
    return match ? match[1].slice(0, 3) : null;
  };
  const startsContinuation = (line: string): boolean =>
    /^\s{0,3}(?:[-*+]|\d{1,9}[.)])\s/.test(line) ||
    /^\s{0,3}>/.test(line) ||
    /^\s{2,}\S/.test(line);
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    if (fence) {
      const marker = fenceMarkerOf(line);
      if (marker && marker === fence) fence = null;
      continue;
    }
    const marker = fenceMarkerOf(line);
    if (marker) {
      fence = marker;
      continue;
    }
    if (line.trim().length === 0) {
      // A blank line is a boundary — unless the next non-blank line continues
      // the current construct (loose list / blockquote / indented block).
      if (index + 1 < lines.length) {
        let next = index + 1;
        while (next < lines.length && lines[next].trim().length === 0) next += 1;
        if (next < lines.length && startsContinuation(lines[next])) continue;
      }
      if (index > start) blocks.push(lines.slice(start, index).join("\n"));
      start = index + 1;
    }
  }
  if (start < lines.length) blocks.push(lines.slice(start).join("\n"));
  return blocks.length > 0 ? blocks : [content];
}

/** Remark plugins, in order. `remarkMath` must run BEFORE
 *  `remarkPreserveLineBreaks`: the latter splits text nodes on "\n" and would
 *  otherwise have already shredded a multi-line formula into `break` nodes.
 *  Once math is its own node it carries no children, so line-break handling
 *  leaves it untouched. */
const REMARK_PLUGINS = [remarkGfm, remarkMath, remarkPreserveLineBreaks];

/** KaTeX is strict by default: it warns on unicode-in-math and refuses a few
 *  constructs LLMs emit casually (bare CJK inside `$…$`, `\text` without a
 *  package mindset). `strict: "ignore"` typesets them instead of dropping the
 *  formula; genuine syntax errors still surface through rehype-katex's own
 *  `throwOnError: false` retry, which renders the source in red rather than
 *  failing the message. */
const KATEX_OPTIONS = { strict: "ignore", trust: false } as const;

const REHYPE_PLUGINS: NonNullable<
  ComponentProps<typeof ReactMarkdown>["rehypePlugins"]
> = [[rehypeKatex, KATEX_OPTIONS]];

/** One top-level markdown block. Memo compares ONLY the block text and the
 *  workspace root: react-markdown has no parse cache, so a re-render of a
 *  section re-parses its whole text — during streaming every commit must
 *  therefore re-parse just the growing tail block. File links are now
 *  deterministic (shape-only; no async verification state), so the block
 *  text and the root fully determine the rendered markup. The components
 *  map is rebuilt per MarkdownBody render and intentionally excluded. */
const MarkdownSection = memo(
  function MarkdownSection({
    content,
    components,
  }: {
    content: string;
    components: MarkdownComponents;
    workspaceRoot?: string;
  }) {
    return (
      <ReactMarkdown
        remarkPlugins={REMARK_PLUGINS}
        rehypePlugins={REHYPE_PLUGINS}
        urlTransform={safeMarkdownUrl}
        components={components}
      >
        {content}
      </ReactMarkdown>
    );
  },
  (prev, next) =>
    prev.content === next.content && prev.workspaceRoot === next.workspaceRoot,
);

function MarkdownBody({ content, workspaceRoot, onFilePathClick, changedFiles, candidatePaths, onImagePreview }: Props) {
  const appTheme = useCurrentAppTheme();
  const codeTheme = appTheme === "light" ? oneLight : vscDarkPlus;
  // Two-tier compact repair (see repairCompactMarkdownNormalized /
  // repairCompactMarkdownBlockLines): the stateful whole-message passes run
  // per commit (linear, cheap); the per-line repair passes are value-cached
  // per block so every finished block repairs exactly once and only the
  // streaming tail block repairs per commit.
  //
  // Math is masked out for the whole pipeline and restored at the very end
  // (see markdown-math.ts): both repair tiers rewrite lines heuristically and
  // would mangle LaTeX, and masked formulas are also single-line, which keeps
  // `splitMarkdownBlocks` from cutting a display block at an internal blank
  // line.
  const math = useMemo(() => maskMarkdownMath(content), [content]);
  const normalized = useMemo(
    () => repairCompactMarkdownNormalized(math.text),
    [math],
  );
  const blocks = useMemo(() => splitMarkdownBlocks(normalized), [normalized]);

  // Delegated from the wrapper so both inline-code chips and markdown-link
  // file links share one handler (no per-node closures). The target is
  // resolved at CLICK time — rendering stays a deterministic shape check.
  const handleFileLinkClick = useCallback(
    (event: React.MouseEvent<HTMLElement>) => {
      if (!onFilePathClick) return;
      const linkEl = (event.target as HTMLElement).closest(".md-file-path");
      const raw = linkEl?.getAttribute("data-file-path");
      if (!raw) return;
      if (linkEl instanceof HTMLAnchorElement) event.preventDefault();
      const [path, line] = raw.split("#");
      const lineNumber = line && Number(line) > 0 ? Number(line) : undefined;
      // Bare names / partial paths (`Composer.tsx`, `commands/fs.rs`) resolve
      // against the changeset + turn candidate pool when possible; a pool hit
      // opens the real file instead of the fragment. Falls back to the span
      // itself, which the editor opens relative to the workspace root.
      const target = resolveFileLinkTarget(
        path,
        changedFiles,
        candidatePaths,
        workspaceRoot,
      );
      onFilePathClick(target, lineNumber);
    },
    [onFilePathClick, workspaceRoot, changedFiles, candidatePaths],
  );

  const handleFileLinkKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLElement>) => {
      if (event.key !== "Enter" && event.key !== " ") return;
      event.preventDefault();
      event.currentTarget.click();
    },
    [],
  );

  const components: MarkdownComponents = {
        br() {
          return <br className="md-line-break" />;
        },
        code({ className, children, ...props }) {
          const match = /language-(\w+)/.exec(className || "");
          const codeString = (children == null ? "" : String(children)).replace(/\n$/, "");

          if (match) {
            if (codeString.trim().length === 0) {
              return null;
            }
            return (
              <MarkdownCodeBlock language={match[1]} code={codeString} theme={codeTheme} />
            );
          }

          // File-shaped spans render as FIXED file links — a pure shape check,
          // no async verification and no per-render recomputation. Click-time
          // resolution opens the real file (see handleFileLinkClick).
          const resolved =
            onFilePathClick != null
              ? resolveClickableFilePath(codeString, workspaceRoot)
              : null;
          if (!resolved) {
            return (
              <code className="md-inline-code" {...props}>
                {children}
              </code>
            );
          }
          const label = `${resolved.path}${resolved.lineNumber ? `:${resolved.lineNumber}` : ""}`;
          return (
            <code
              className="md-inline-code md-file-path"
              data-file-path={`${resolved.path}#${resolved.lineNumber ?? 0}`}
              title={`${label} — 点击打开`}
              role="link"
              tabIndex={0}
              onKeyDown={handleFileLinkKeyDown}
              aria-label={`打开文件 ${label}`}
              {...props}
            >
              <FileCode size={14} strokeWidth={2} className="md-file-path-icon" aria-hidden="true" />
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
        h4({ children }) {
          return <h4 className="md-heading md-h4">{children}</h4>;
        },
        h5({ children }) {
          return <h5 className="md-heading md-h5">{children}</h5>;
        },
        h6({ children }) {
          return <h6 className="md-heading md-h6">{children}</h6>;
        },
        blockquote({ children }) {
          return <blockquote className="md-blockquote">{children}</blockquote>;
        },
        hr() {
          return <hr className="md-hr" />;
        },
        a({ href, children }) {
          // Markdown links whose target is a workspace file reference
          // (`[MarkdownBody.tsx](apps/…/MarkdownBody.tsx)`, `…#L3251`) render
          // as FIXED file links — same shape-only rule as inline-code paths,
          // opened by the wrapper's delegated click handler.
          if (href && onFilePathClick != null && !/^(?:https?:|mailto:|#)/i.test(href)) {
            const resolved = resolveClickableFilePath(
              markdownFileHrefToSpan(href.trim()),
              workspaceRoot,
            );
            if (resolved) {
              const label = `${resolved.path}${resolved.lineNumber ? `:${resolved.lineNumber}` : ""}`;
              return (
                <a
                  className="md-link md-file-path"
                  role="link"
                  tabIndex={0}
                  href={undefined}
                  data-file-path={`${resolved.path}#${resolved.lineNumber ?? 0}`}
                  title={`${label} — 点击打开`}
                  aria-label={`打开文件 ${label}`}
                  onKeyDown={handleFileLinkKeyDown}
                >
                  <FileCode size={14} strokeWidth={2} className="md-file-path-icon" aria-hidden="true" />
                  {children}
                </a>
              );
            }
          }
          return (
            <a className="md-link" href={href} target="_blank" rel="noopener noreferrer">
              {children}
            </a>
          );
        },
        img({ src, alt }) {
          const label = alt || "附加的图片";
          if (!onImagePreview || typeof src !== "string" || !src) {
            return <img className="md-image" src={src} alt={label} />;
          }
          return (
            <button
              type="button"
              className="md-image-button"
              onClick={() => onImagePreview(src, label)}
              aria-label={`预览 ${label}`}
              title="预览图片"
            >
              <img className="md-image" src={src} alt={label} />
            </button>
          );
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
  };

  // Block-split rendering: react-markdown has NO internal parse cache, so a
  // single <ReactMarkdown> re-parses the ENTIRE growing reply on every
  // streaming commit (the "卡成翔 while the LLM types" cost). Splitting into
  // top-level blocks lets every finished block bail out of parsing; only the
  // block under the cursor re-parses per commit.

  return (
    // File links (inline-code chips and markdown-link targets) are delegated
    // from this wrapper so a streaming re-render does not need per-node
    // handlers.
    // eslint-disable-next-line jsx-a11y/no-static-element-interactions, jsx-a11y/click-events-have-key-events
    <div className="md-body" onClick={handleFileLinkClick}>
      {blocks.map((block, index) => (
        <MarkdownSection
          key={index}
          content={math.restore(repairBlockCached(block))}
          components={components}
          workspaceRoot={workspaceRoot}
        />
      ))}
    </div>
  );
}

interface ResolvedFilePath {
  path: string;
  lineNumber?: number;
  /** Normalised path fragment used for changeset/context matching — the bare
   *  name (`Composer.tsx`) for bare spans, or the relative fragment
   *  (`commands/fs.rs`) for partial relative paths. Absent for absolute
   *  paths that do not need disambiguation. */
  matchTail?: string;
}

/** Convert a markdown-link href to the span form the path resolver
 *  understands: `path#L12` / `path#L10-L20` → `path:12` / `path:10`. Without
 *  this the `#L…` fragment stays glued to the file name and neither the
 *  shape check nor the click-time resolution would recognise the path. */
function markdownFileHrefToSpan(href: string): string {
  const lineMatch = href.match(/^(.*?)#L(\d+)(?:-L?\d+)?$/);
  return lineMatch ? `${lineMatch[1]}:${lineMatch[2]}` : href;
}

/** Resolve a clicked file span to the path handed to `onFilePathClick`. Pure
 *  and synchronous — no probes, no caches. Bare names and partial paths
 *  (`Composer.tsx:548`, `commands/fs.rs`) are matched against the changeset +
 *  turn candidate pool as a CONTIGUOUS trailing run of segments (never just
 *  the basename, so a deeper sibling like `.../runtime/permissions/tests.rs`
 *  cannot capture `runtime/tests.rs`); the strongest match (fewest leading
 *  segments dropped) wins, ties keep the earliest source. Anything without a
 *  pool hit opens as written — relative to the workspace root. */
function resolveFileLinkTarget(
  span: string,
  changedFiles?: readonly string[],
  candidatePaths?: readonly string[],
  workspaceRoot?: string,
): string {
  const root = workspaceRoot
    ? normalizeFilePathSeparators(workspaceRoot).replace(/[\\/]+$/, "")
    : "";
  const tail = span.replace(/\\/g, "/");
  let best: { rank: number; relative: string } | null = null;
  for (const source of [...(changedFiles ?? []), ...(candidatePaths ?? [])]) {
    const normalized = source.replace(/\\/g, "/").replace(/:\d+(?::\d+)?$/, "");
    const rank = rankFragmentMatch(normalized, tail);
    if (rank === null) continue;
    // Strict improvement only, so the earliest source wins ties.
    if (best !== null && rank >= best.rank) continue;
    const relative = toWorkspaceRelativePath(normalized, root || undefined);
    if (!relative) continue;
    best = { rank, relative };
  }
  return best?.relative ?? span;
}

/** Accept compound filenames such as `MarkdownBody.test.tsx`, `types.d.ts`,
 * and `bundle.min.js` while still rejecting bare directories and identifiers. */
function isFileNameWithExtension(value: string): boolean {
  const parts = value.split(".");
  return (
    parts.length > 1 &&
    parts[0].length > 0 &&
    parts.slice(1).every((part) => part.length > 0) &&
    parts[parts.length - 1].length <= 10
  );
}

/**
 * Detect inline-code spans that look like a workspace file reference such as
 * `crates/codebuddy-proxy/src/usage.rs:75` or `apps/desktop/ui/src/main.tsx`
 * and resolve them to a workspace-relative open path. Returns null for
 * anything that is clearly not a file path (identifiers, commands, urls,
 * prose).
 */
export function resolveClickableFilePath(
  raw: string,
  workspaceRoot?: string,
): ResolvedFilePath | null {
  let candidate = raw.trim();
  if (candidate.length < 4 || candidate.length > 512) return null;
  // Whitespace is only allowed around `/` separators — agents often write
  // `app-core / state.rs`. The whole fragment is still matched as one tail.
  if (/\s/.test(candidate)) {
    if (!/^[^\s]+(?:\s*\/\s*[^\s]+)+$/.test(candidate)) return null;
    candidate = candidate.replace(/\s*\/\s*/g, "/");
  }
  // Strip diff prefixes so `a/src/foo.rs:10` / `b/src/foo.rs` also resolve.
  candidate = candidate.replace(/^[ab]\//, "");

  // A Windows drive prefix (`D:\...`) makes the path absolute; the `:line`
  // split must not eat the drive colon.
  const isWindowsAbs = /^[A-Za-z]:[\\/]/.test(candidate);
  // Split an optional trailing :line[:column] reference.
  let lineNumber: number | undefined;
  const lineMatch = candidate.match(/^(.*?):(\d+)(?::\d+)?$/);
  if (lineMatch) {
    // Only treat the trailing segment as a line reference when what remains
    // is still a plausible path (never strip the drive letter colon).
    const remainder = lineMatch[1];
    if (!isWindowsAbs || remainder.length > 2) {
      candidate = remainder;
      lineNumber = Number.parseInt(lineMatch[2], 10);
    }
  }
  if (lineNumber !== undefined && (!Number.isFinite(lineNumber) || lineNumber <= 0)) {
    return null;
  }

  const isPosixAbs = candidate.startsWith("/");
  const isRelative = candidate.includes("/") || candidate.includes("\\");
  if (!isWindowsAbs && !isPosixAbs && !isRelative) {
    // Bare file name such as `Composer.tsx:548` or `MarkdownBody.tsx` —
    // resolvable when it carries an extension. The line reference is now
    // optional because the changeset match is reliable enough on its own;
    // the workspace-wide name search (fsFindByName) stays gated on a line
    // number to avoid misidentifying common names in prose.
    if (!workspaceRoot) return null;
    if (!isFileNameWithExtension(candidate)) return null;
    return { path: candidate, lineNumber, matchTail: candidate };
  }
  // Must carry a file extension so bare directories / URLs do not match.
  const lastSegment = candidate.replace(/\\/g, "/").split("/").pop() ?? "";
  if (!isFileNameWithExtension(lastSegment)) {
    return null;
  }
  if (/^https?:\/\//i.test(candidate)) {
    return null;
  }

  if (isWindowsAbs || isPosixAbs) {
    // Absolute spans only become openable when they sit under the current
    // workspace root. Keep the stored path relative so the editor/open path
    // never depends on a second strip pass at click time.
    if (!workspaceRoot) return null;
    const relative = toWorkspaceRelativePath(candidate, workspaceRoot);
    if (
      !relative ||
      relative === candidate ||
      /^[A-Za-z]:[\\/]/.test(relative) ||
      relative.startsWith("/")
    ) {
      return null;
    }
    return { path: relative, lineNumber };
  }
  if (!workspaceRoot) {
    return null;
  }
  const relative = candidate.replace(/\\/g, "/");
  const matchTail = candidate
    .replace(/\\/g, "/")
    .replace(/:\d+(?::\d+)?$/, "");
  return { path: relative, lineNumber, matchTail };
}

/** Collapse mixed `\` / `/` separators to the platform-dominant one so the
 *  resolved path compares cleanly against the canonical workspace root in
 *  the backend's traversal check. */
function normalizeFilePathSeparators(value: string) {
  const backslashes = (value.match(/\\/g) ?? []).length;
  const slashes = (value.match(/\//g) ?? []).length;
  return backslashes > slashes ? value.replace(/\//g, "\\") : value.replace(/\\/g, "/");
}

/** Normalize any absolute-in-workspace or mixed-separator path down to the
 *  workspace-relative form the editor and remote FS APIs expect. */
function toWorkspaceRelativePath(path: string, workspaceRoot?: string) {
  const normalized = path.replace(/\\/g, "/").replace(/^\.\/+/, "");
  return stripWorkspaceRootPrefix(normalized, workspaceRoot).replace(/\\/g, "/");
}

/** A path fragment matches a candidate path when it is a CONTIGUOUS trailing
 *  run of the candidate's segments — every fragment segment lines up, in
 *  order, against the candidate's final segments. The whole fragment is
 *  considered, not just the trailing file name, so `runtime/tests.rs` matches
 *  `.../runtime/tests.rs` but NOT `.../runtime/permissions/tests.rs` (a
 *  different file that merely happens to share `runtime` and `tests.rs`).
 *  Leading directories may still be dropped, which is the safe abbreviation:
 *  `commands/fs.rs` hits `apps/desktop/src-tauri/src/commands/fs.rs`. Returns
 *  a boolean for backward compatibility; use {@link rankFragmentMatch} when
 *  disambiguating between several matching candidates. */
export function pathMatchesFragment(candidatePath: string, fragment: string) {
  return rankFragmentMatch(candidatePath, fragment) !== null;
}

/** Rank how well `candidatePath` matches a relative `fragment`. Lower is
 *  better; `null` means the fragment is not a contiguous trailing run of the
 *  candidate's segments. The rank is the candidate's segment count, so when
 *  several candidates end in the same fragment the one that drops the FEWEST
 *  leading segments (the most specific, shallowest match) wins; ties keep the
 *  earliest source. */
export function rankFragmentMatch(candidatePath: string, fragment: string): number | null {
  const candidateSegments = candidatePath
    .split("/")
    .filter(Boolean)
    .map((segment) => segment.replace(/:\d+(?::\d+)?$/, ""));
  const fragmentSegments = fragment
    .split("/")
    .filter(Boolean)
    .map((segment) => segment.replace(/:\d+(?::\d+)?$/, ""));
  if (fragmentSegments.length === 0) return null;
  if (fragmentSegments.length > candidateSegments.length) return null;
  // The whole fragment must line up contiguously against the candidate's
  // trailing segments — no intermediate directories may be skipped.
  const offset = candidateSegments.length - fragmentSegments.length;
  for (let index = 0; index < fragmentSegments.length; index++) {
    if (candidateSegments[offset + index] !== fragmentSegments[index]) return null;
  }
  return candidateSegments.length;
}

export default memo(MarkdownBody);

function CopyCodeButton({ text }: { text: string }) {
  return (
    <CopyTextButton
      text={text}
      label="复制代码"
      copiedLabel="已复制代码"
      className="md-code-copy"
      copiedClassName="md-code-copy-copied"
    />
  );
}

export interface CopyTextButtonProps {
  text: string;
  label: string;
  copiedLabel: string;
  className: string;
  copiedClassName?: string;
}

export function CopyTextButton({
  text,
  label,
  copiedLabel,
  className,
  copiedClassName,
}: CopyTextButtonProps) {
  const [copied, setCopied] = useState(false);
  const resetTimerRef = useRef<number | null>(null);

  useEffect(() => {
    return () => {
      if (resetTimerRef.current !== null) {
        window.clearTimeout(resetTimerRef.current);
      }
    };
  }, []);

  const handleCopy = useCallback(async () => {
    await copyTextToClipboard(text);
    setCopied(true);
    if (resetTimerRef.current !== null) {
      window.clearTimeout(resetTimerRef.current);
    }
    resetTimerRef.current = window.setTimeout(() => {
      setCopied(false);
      resetTimerRef.current = null;
    }, 1600);
  }, [text]);

  const resolvedClassName = copied
    ? copiedClassName
      ? `${className} ${copiedClassName}`
      : className
    : className;

  return (
    <button
      type="button"
      className={resolvedClassName}
      aria-label={copied ? copiedLabel : label}
      title={copied ? "已复制" : label}
      onClick={handleCopy}
    >
      {copied ? (
        <Check size={14} strokeWidth={2.2} aria-hidden="true" />
      ) : (
        <Copy size={14} strokeWidth={2.1} aria-hidden="true" />
      )}
    </button>
  );
}

async function copyTextToClipboard(text: string) {
  if (navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(text);
      return;
    } catch {
      // Fall through to the legacy path for embedded webviews without clipboard permission.
    }
  }
  fallbackCopyText(text);
}

function fallbackCopyText(text: string) {
  const textarea = document.createElement("textarea");
  textarea.value = text;
  textarea.setAttribute("readonly", "");
  textarea.style.position = "fixed";
  textarea.style.left = "-9999px";
  textarea.style.top = "0";
  document.body.appendChild(textarea);
  textarea.select();
  document.execCommand("copy");
  textarea.remove();
}

function safeMarkdownUrl(url: string) {
  if (/^data:image\/(png|jpeg|jpg|gif|webp);base64,[a-z0-9+/=]+$/i.test(url)) {
    return url;
  }
  if (/^(https?:|mailto:)/i.test(url) || url.startsWith("/") || url.startsWith("#")) {
    return url;
  }
  // Relative/workspace paths (file references such as `crates/foo.rs`) pass
  // through so the anchor renderer can turn verified ones into clickable file
  // links. Anything WITH an untrusted scheme (javascript:, data:text/html,
  // …) is still stripped.
  if (!/^[a-z][a-z0-9+.-]*:/i.test(url)) {
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
  if (!isValidElement<{ className?: string; src?: string; children?: ReactNode }>(child)) {
    return false;
  }
  return (
    child.props.className === "md-image" ||
    child.type === "img" ||
    Boolean(child.props.src) ||
    Children.toArray(child.props.children).some(isMarkdownImageElement)
  );
}

/** Whole-message normalisation: the STATEFUL passes (leaked course-break
 *  noise runs, escaped line breaks, stringified unwrap, compact-fence
 *  repair) must see the full text — blank lines carry meaning in the noise
 *  run and fences span it. Linear line scans, cheap enough to run on every
 *  streaming commit. */
function repairCompactMarkdownNormalized(content: string) {
  return repairCompactCodeFences(normalizeMarkdownInput(content));
}

/** Block-level line repairs (compact headings/tables/numbered lists).
 *  Stateless within a block by splitter construction (fences and tables never
 *  span a blank line once the whole-message tier has normalised compact
 *  fences), so blocks can be repaired independently and value-cached. */
function repairCompactMarkdownBlockLines(block: string) {
  const lines = block.split(/\r?\n/);
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

export function repairCompactMarkdown(content: string) {
  const math = maskMarkdownMath(content);
  return math.restore(
    repairCompactMarkdownBlockLines(repairCompactMarkdownNormalized(math.text)),
  );
}

const COMPACT_FENCE_LANGUAGES = [
  "typescript",
  "javascript",
  "powershell",
  "markdown",
  "python",
  "tsx",
  "jsx",
  "bash",
  "shell",
  "rust",
  "json",
  "yaml",
  "toml",
  "diff",
  "text",
  "sql",
  "css",
  "html",
  "sh",
  "md",
].sort((left, right) => right.length - left.length);

function repairCompactCodeFences(content: string) {
  const repaired: string[] = [];
  let activeCompactFence: CompactFenceState | null = null;

  for (const line of content.split(/\r?\n/)) {
    if (activeCompactFence) {
      if (line.trim() === activeCompactFence.marker) {
        repaired.push(`${activeCompactFence.indent}${activeCompactFence.marker}`);
        activeCompactFence = null;
        continue;
      }

      const hasInlineClose =
        line.endsWith(activeCompactFence.marker) &&
        !line.trimStart().startsWith(activeCompactFence.marker);
      const contentLine = hasInlineClose
        ? line.slice(0, -activeCompactFence.marker.length)
        : line;
      const repairedContent = repairCompactFenceContent(
        activeCompactFence.language,
        contentLine,
      );
      if (repairedContent.length > 0) {
        repaired.push(...repairedContent.split("\n"));
      }
      if (hasInlineClose) {
        repaired.push(`${activeCompactFence.indent}${activeCompactFence.marker}`);
        activeCompactFence = null;
      }
      continue;
    }

    const result = repairCompactCodeFenceLine(line);
    repaired.push(...result.lines);
    activeCompactFence = result.openFence ?? null;
  }

  return repaired.join("\n");
}

interface CompactFenceState {
  marker: string;
  language: string;
  indent: string;
}

function repairCompactCodeFenceLine(line: string) {
  const match = line.match(/^(\s*)(`{3,}|~{3,})([A-Za-z][\w+-]*\S.*)$/u);
  if (!match) {
    return { lines: [line] };
  }

  const [, indent, marker, tail] = match;
  const split = splitCompactFenceTail(tail);
  if (!split) {
    return { lines: [line] };
  }

  const closingMarker = marker[0].repeat(marker.length);
  const hasInlineClose = split.content.endsWith(closingMarker);
  const content = hasInlineClose
    ? split.content.slice(0, -closingMarker.length)
    : split.content;
  const repairedContent = repairCompactFenceContent(split.language, content).split("\n");
  const opening = `${indent}${marker}${split.language}`;
  return hasInlineClose
    ? { lines: [opening, ...repairedContent, `${indent}${closingMarker}`] }
    : {
        lines: [opening, ...repairedContent],
        openFence: { marker: closingMarker, language: split.language, indent },
      };
}

function splitCompactFenceTail(tail: string) {
  const lower = tail.toLowerCase();
  for (const language of COMPACT_FENCE_LANGUAGES) {
    if (!lower.startsWith(language) || tail.length <= language.length) {
      continue;
    }
    const content = tail.slice(language.length);
    if (/^\s/u.test(content)) {
      continue;
    }
    return {
      language: tail.slice(0, language.length),
      content,
    };
  }
  return null;
}

function repairCompactFenceContent(language: string, content: string) {
  const trimmed = content.trim();
  if (!/^(text|markdown|md)$/iu.test(language)) {
    return trimmed;
  }

  return trimmed
    .replace(/([^\s\n])(?=asset_structured_tags\b)/gu, "$1\n")
    .replace(/([^\s\n])(?=asset_search_documents\b)/gu, "$1\n")
    .replace(/([^\s\n])(?=vision:[a-z_]+:)/giu, "$1\n")
    .replace(/([^\s\n])(-\s*)/gu, "$1\n$2")
    .replace(/(^|\n)-(?=\S)/gu, "$1- ")
    .replace(/=([^\s\n])/gu, "= $1");
}

function normalizeMarkdownInput(content: string) {
  return stripLeakedCourseBreakNoise(
    normalizeEscapedMarkdownLineBreaks(unwrapStringifiedMarkdown(content)),
  );
}

function stripLeakedCourseBreakNoise(content: string) {
  const lines = content.split(/\r?\n/);
  const repaired: string[] = [];
  let noiseRun: string[] = [];
  let courseLineCount = 0;
  let inFence = false;

  const flushNoiseRun = () => {
    if (courseLineCount < 3) {
      repaired.push(...noiseRun);
    }
    noiseRun = [];
    courseLineCount = 0;
  };

  for (const line of lines) {
    if (/^\s*(```|~~~)/u.test(line)) {
      flushNoiseRun();
      inFence = !inFence;
      repaired.push(line);
      continue;
    }

    if (inFence) {
      repaired.push(line);
      continue;
    }

    const trimmed = line.trim();
    const isCourseNoise = /^course$/iu.test(trimmed);
    const isBreakNoise = /^<br\s*\/?>$/iu.test(trimmed);
    if (trimmed === "" || isCourseNoise || isBreakNoise) {
      noiseRun.push(line);
      if (isCourseNoise) {
        courseLineCount += 1;
      }
      continue;
    }

    flushNoiseRun();
    repaired.push(line);
  }

  flushNoiseRun();
  return repaired.join("\n");
}

function repairCompactMarkdownLine(line: string) {
  return repairCompactHeadingLine(repairCompactMarkdownTable(line)).replace(
    /([^\s\n])(\d{1,2}\.\s+(?=(?:\*\*)?[\p{Script=Han}A-Za-z]))/gu,
    "$1\n$2",
  );
}

function repairCompactHeadingLine(line: string) {
  const match = line.match(/^([\u200B\u200C\u200D\uFEFF]*[ \t]{0,3})(.*)$/u);
  if (!match) {
    return line;
  }

  const prefix = match[1].replace(/[\u200B\u200C\u200D\uFEFF]/gu, "");
  const rest = match[2];
  const plainHeading = rest.match(/^(#{1,6})(?!#)([^\S\r\n]*)(\S.*)$/u);
  if (plainHeading) {
    return `${prefix}${plainHeading[1]} ${plainHeading[3]}`;
  }

  const escapedEachHeading = rest.match(/^((?:\\#){1,6})(?!\\#|#)([^\S\r\n]*)(\S.*)$/u);
  if (escapedEachHeading) {
    return `${prefix}${escapedEachHeading[1].replace(/\\/gu, "")} ${escapedEachHeading[3]}`;
  }

  const escapedFirstHeading = rest.match(/^\\(#{1,6})(?!#)([^\S\r\n]*)(\S.*)$/u);
  if (escapedFirstHeading) {
    return `${prefix}${escapedFirstHeading[1]} ${escapedFirstHeading[3]}`;
  }

  return line;
}

function normalizeEscapedMarkdownLineBreaks(content: string) {
  if (!content.includes("\\n")) {
    return content;
  }
  if (!looksLikeMarkdownBlock(content)) {
    return content;
  }
  return escapedMarkdownLineBreaksAsNewlines(content);
}

function escapedMarkdownLineBreaksAsNewlines(content: string) {
  return content.replace(/\\r\\n/g, "\n").replace(/\\n/g, "\n");
}

function unwrapStringifiedMarkdown(content: string) {
  const trimmed = content.trim();
  if (trimmed.length < 2 || !isWrappedInMatchingQuotes(trimmed)) {
    return content;
  }

  if (trimmed.startsWith("\"")) {
    try {
      const parsed: unknown = JSON.parse(trimmed);
      if (typeof parsed === "string" && looksLikeMarkdownBlock(parsed)) {
        return parsed;
      }
    } catch {
      // Some proxied outputs include literal newlines inside surrounding quotes.
    }
  }

  const inner = trimmed.slice(1, -1);
  if (looksLikeMarkdownBlock(inner)) {
    return inner;
  }
  return content;
}

function isWrappedInMatchingQuotes(value: string) {
  return (
    (value.startsWith("\"") && value.endsWith("\"")) ||
    (value.startsWith("'") && value.endsWith("'"))
  );
}

function looksLikeMarkdownBlock(content: string) {
  const normalized = escapedMarkdownLineBreaksAsNewlines(content);
  return /(?:^|\n)\s{0,3}(?:#{1,6}(?!#)\s*\S|[-*+]\s|\d{1,2}\.\s|>|```|~~~|\|)/u.test(
    normalized,
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

type MarkdownAstNode = {
  type?: string;
  value?: string;
  children?: MarkdownAstNode[];
};

function remarkPreserveLineBreaks() {
  return (tree: MarkdownAstNode) => {
    preserveLineBreaksInChildren(tree);
  };
}

function preserveLineBreaksInChildren(node: MarkdownAstNode) {
  if (!Array.isArray(node.children)) {
    return;
  }

  const children: MarkdownAstNode[] = [];
  for (const child of node.children) {
    if (child.type === "text" && typeof child.value === "string" && child.value.includes("\n")) {
      children.push(...splitMarkdownTextOnLineBreaks(child));
      continue;
    }

    preserveLineBreaksInChildren(child);
    children.push(child);
  }
  node.children = children;
}

function splitMarkdownTextOnLineBreaks(node: MarkdownAstNode) {
  const parts = (node.value ?? "").split("\n");
  const nodes: MarkdownAstNode[] = [];
  parts.forEach((part, index) => {
    if (index > 0) {
      nodes.push({ type: "break" });
    }
    if (part.length > 0) {
      nodes.push({ ...node, value: part });
    }
  });
  return nodes;
}
