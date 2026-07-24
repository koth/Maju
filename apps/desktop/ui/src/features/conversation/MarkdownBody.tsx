import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Check, Copy, FileCode } from "lucide-react";
import {
  Children,
  isValidElement,
  memo,
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { Prism as SyntaxHighlighter } from "react-syntax-highlighter";
import { oneLight, vscDarkPlus } from "react-syntax-highlighter/dist/esm/styles/prism";
import { getAppliedAppTheme } from "../../theme";
import { fsPathExists } from "../../lib/tauri";

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
   *  by basename alone. */
  candidatePaths?: string[];
}

/** Cross-message cache of `fs_path_exists` results so the same file link is
 *  not re-probed for every assistant message that mentions it. */
const filePathExistenceCache = new Map<string, boolean>();

/** Workspace-relative (or absolute) path that a bare file name was located
 *  at, keyed by the same `path#line` cache key so the click handler opens
 *  the real file instead of the placeholder name. */
const barePathOverrides = new Map<string, string>();

/** Test hook: clear the module-level existence cache between cases. */
export function clearFilePathLinkCacheForTests() {
  filePathExistenceCache.clear();
  barePathOverrides.clear();
}

function MarkdownBody({ content, workspaceRoot, onFilePathClick, changedFiles, candidatePaths }: Props) {
  const appTheme = useCurrentAppTheme();
  const codeTheme = appTheme === "light" ? oneLight : vscDarkPlus;
  const displayContent = repairCompactMarkdown(content);
  // Inline-code spans that look like file paths are only rendered as links
  // once the backend confirmed they exist (incomplete paths like
  // `codex_api_proxy/mod.rs:3880` stay plain code).
  const [verifiedPaths, setVerifiedPaths] = useState<ReadonlySet<string>>(
    () => new Set(),
  );
  const pendingCandidates = new Map<string, ResolvedFilePath>();
  // Bump on every probe resolution so the effect re-runs against the freshly
  // populated cache even when nothing was newly verified.
  const [probeRound, setProbeRound] = useState(0);

  useEffect(() => {
    if (!onFilePathClick) return;
    const candidates = [...pendingCandidates.entries()].filter(
      ([key]) =>
        !verifiedPaths.has(key) && filePathExistenceCache.get(key) === undefined,
    );
    if (candidates.length === 0) return;
    let cancelled = false;

    const root = workspaceRoot
      ? normalizeFilePathSeparators(workspaceRoot).replace(/[\\/]+$/, "")
      : "";
    const separator = root.includes("\\") ? "\\" : "/";

    // Pass 0: changeset + turn candidate pool. A candidate's matchTail
    //  (`Composer.tsx`, `commands/fs.rs` or `app-core / state.rs`) is matched
    //  as an ordered segment subsequence against each candidate path — the
    //  fragment is never split into a basename prefix. Hits resolve directly
    //  to the real workspace path without any filesystem IO.
    const poolMatched = new Map<string, string>();
    const matchSources = [
      ...(changedFiles ?? []),
      ...(candidatePaths ?? []),
    ];
    if (matchSources.length > 0) {
      for (const [key, resolved] of candidates) {
        if (poolMatched.has(key) || !resolved.matchTail) continue;
        const tail = resolved.matchTail.replace(/\\/g, "/");
        for (const sourcePath of matchSources) {
          const normalized = sourcePath.replace(/\\/g, "/");
          if (!pathMatchesFragment(normalized, tail)) continue;
          const isAbsolute =
            /^[A-Za-z]:[\\/]/.test(sourcePath) || sourcePath.startsWith("/");
          poolMatched.set(
            key,
            isAbsolute
              ? sourcePath
              : root
                ? `${root}${separator}${sourcePath.replace(/[\\/]+/g, separator)}`
                : "",
          );
          break;
        }
      }
    }

    // Candidates resolved by the pools are done immediately.
    const poolKeys = new Set(poolMatched.keys());
    const changesetVerified: string[] = [];
    for (const [key, resolvedPath] of poolMatched) {
      if (!resolvedPath) continue;
      filePathExistenceCache.set(key, true);
      barePathOverrides.set(key, resolvedPath);
      changesetVerified.push(key);
    }
    if (changesetVerified.length > 0) {
      setVerifiedPaths((prev) => {
        const next = new Set(prev);
        for (const key of changesetVerified) next.add(key);
        return next;
      });
    }

    // Remaining candidates still need filesystem probing.
    const remaining = candidates.filter(([key]) => !poolKeys.has(key));

    // Directories of already-resolved full paths in this message give bare
    // names (`Composer.tsx:12`) a same-directory first guess before falling
    // back to probing the raw span as-is.
    const contextDirs = [
      ...new Set(
        [...pendingCandidates.values()]
          .filter((resolved) => !resolved.matchTail)
          .map((resolved) => {
            const normalized = resolved.path.replace(/\\/g, "/");
            const lastSlash = normalized.lastIndexOf("/");
            return lastSlash > 0 ? normalized.slice(0, lastSlash) : null;
          })
          .filter((dir): dir is string => dir != null),
      ),
    ];

    // Pass 1: probe every remaining candidate's resolved path with fsPathExists.
    // This covers complete relative paths and absolute paths alike. Candidates
    // with a matchTail (bare names or partial paths) that fail here get one
    // context-directory retry below; there is deliberately no workspace-wide
    // name search — unresolved spans stay plain code.
    const allPaths = [...new Set(remaining.map(([, resolved]) => resolved.path))];
    fsPathExists(allPaths)
      .then(async (results) => {
        if (cancelled) return;
        const existsByPath = new Map(
          allPaths.map((path, index) => [path, results[index] === true]),
        );
        const newlyVerified: string[] = [];
        for (const [key, resolved] of remaining) {
          const exists = existsByPath.get(resolved.path) === true;
          filePathExistenceCache.set(key, exists);
          if (exists) newlyVerified.push(key);
        }

        // Pass 2: candidates that failed fsPathExists but have a matchTail
        // (bare names or partial paths) get a second chance via context dirs.
        const unresolved = remaining.filter(
          ([key, resolved]) =>
            !newlyVerified.includes(key) && resolved.matchTail,
        );
        const resolvedByKey = new Map<string, string | null>();

        const guesses: { key: string; guess: string }[] = [];
        for (const [key, resolved] of unresolved) {
          const tail = resolved.matchTail!.replace(/\\/g, "/");
          for (const dir of contextDirs) {
            guesses.push({ key, guess: `${dir}/${tail}` });
          }
        }
        if (guesses.length > 0) {
          const uniqueGuesses = [...new Set(guesses.map((guess) => guess.guess))];
          const guessResults = await fsPathExists(uniqueGuesses);
          const exists = new Map(
            uniqueGuesses.map((guess, index) => [guess, guessResults[index] === true]),
          );
          for (const { key, guess } of guesses) {
            if (!resolvedByKey.has(key) && exists.get(guess)) {
              resolvedByKey.set(key, guess);
            }
          }
        }

        for (const [key] of unresolved) {
          const resolvedPath = resolvedByKey.get(key) ?? null;
          filePathExistenceCache.set(key, resolvedPath != null);
          if (resolvedPath != null) {
            barePathOverrides.set(key, resolvedPath);
            newlyVerified.push(key);
          }
        }

        if (newlyVerified.length > 0) {
          setVerifiedPaths((prev) => {
            const next = new Set(prev);
            for (const key of newlyVerified) next.add(key);
            return next;
          });
        }
        setProbeRound((round) => round + 1);
      })
      .catch(() => {
        // Probing failed (e.g. workspace reconnecting): leave spans as plain
        // code; a later render can retry.
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [displayContent, workspaceRoot, changedFiles, candidatePaths, onFilePathClick, verifiedPaths, probeRound]);

  const handleInlineCodeClick = useCallback(
    (event: React.MouseEvent<HTMLElement>) => {
      if (!onFilePathClick) return;
      const codeEl = (event.target as HTMLElement).closest("code.md-file-path");
      const raw = codeEl?.getAttribute("data-file-path");
      if (!raw) return;
      const [path, line] = raw.split("#");
      const lineNumber = line && Number(line) > 0 ? Number(line) : undefined;
      const resolved = { path, lineNumber };
      if (resolved) {
        onFilePathClick(resolved.path, resolved.lineNumber);
      }
    },
    [onFilePathClick, workspaceRoot],
  );

  return (
    // Clickable inline-code file paths are delegated from this wrapper so a
    // streaming re-render does not need per-node handlers.
    // eslint-disable-next-line jsx-a11y/no-static-element-interactions, jsx-a11y/click-events-have-key-events
    <div className="md-body" onClick={handleInlineCodeClick}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkPreserveLineBreaks]}
        urlTransform={safeMarkdownUrl}
        components={{
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
              <div className="md-code-block">
                <div className="md-code-header">
                  <span className="md-code-lang">{match[1]}</span>
                  <CopyCodeButton text={codeString} />
                </div>
                <SyntaxHighlighter
                  style={codeTheme}
                  language={match[1]}
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
                  {codeString}
                </SyntaxHighlighter>
              </div>
            );
          }

          const resolved =
            onFilePathClick != null
              ? resolveClickableFilePath(codeString, workspaceRoot)
              : null;
          let clickable = false;
          if (resolved) {
            const cacheKey = `${resolved.path}#${resolved.lineNumber ?? 0}`;
            pendingCandidates.set(cacheKey, resolved);
            clickable =
              verifiedPaths.has(cacheKey) ||
              filePathExistenceCache.get(cacheKey) === true;
          }
          const openPath =
            clickable && resolved
              ? barePathOverrides.get(
                  `${resolved.path}#${resolved.lineNumber ?? 0}`,
                ) ?? resolved.path
              : undefined;
          return (
            <code
              className={clickable ? "md-inline-code md-file-path" : "md-inline-code"}
              data-file-path={
                openPath
                  ? `${openPath}#${resolved?.lineNumber ?? 0}`
                  : undefined
              }
              title={openPath ? `${openPath} — 点击打开` : undefined}
              {...props}
            >
              {clickable && (
                <FileCode size={12} strokeWidth={2} className="md-file-path-icon" aria-hidden="true" />
              )}
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

/**
 * Detect inline-code spans that look like a workspace file reference such as
 * `crates/codebuddy-proxy/src/usage.rs:75` or `apps/desktop/ui/src/main.tsx`
 * and resolve them to an absolute path. Returns null for anything that is
 * clearly not a file path (identifiers, commands, urls, prose).
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
    if (!/^[^./\\]+\.[^./\\]{1,10}$/.test(candidate)) return null;
    return { path: candidate, lineNumber, matchTail: candidate };
  }
  // Must carry a file extension so bare directories / URLs do not match.
  const lastSegment = candidate.replace(/\\/g, "/").split("/").pop() ?? "";
  if (!/^[^./]+\.[^./]{1,10}$/.test(lastSegment)) {
    return null;
  }
  if (/^https?:\/\//i.test(candidate)) {
    return null;
  }

  if (isWindowsAbs || isPosixAbs) {
    return { path: normalizeFilePathSeparators(candidate), lineNumber };
  }
  if (!workspaceRoot) {
    return null;
  }
  const root = normalizeFilePathSeparators(workspaceRoot).replace(/[\\/]+$/, "");
  const separator = root.includes("\\") ? "\\" : "/";
  const relative = candidate.replace(/[\\/]+/g, separator);
  const matchTail = candidate
    .replace(/\\/g, "/")
    .replace(/:\d+(?::\d+)?$/, "");
  return { path: `${root}${separator}${relative}`, lineNumber, matchTail };
}

/** Collapse mixed `\` / `/` separators to the platform-dominant one so the
 *  resolved path compares cleanly against the canonical workspace root in
 *  the backend's traversal check. */
function normalizeFilePathSeparators(value: string) {
  const backslashes = (value.match(/\\/g) ?? []).length;
  const slashes = (value.match(/\//g) ?? []).length;
  return backslashes > slashes ? value.replace(/\//g, "\\") : value.replace(/\\/g, "/");
}

/** A path fragment (`app-core/state.rs`) matches a candidate path when its
 *  segments appear in order inside the candidate's segments and the fragment
 *  ends on the candidate's file name. This lets `app-core / state.rs` match
 *  `crates/app-core/src/state.rs` without ever splitting the fragment into
 *  a bare basename. */
export function pathMatchesFragment(candidatePath: string, fragment: string) {
  const candidateSegments = candidatePath.split("/").filter(Boolean);
  const fragmentSegments = fragment
    .split("/")
    .filter(Boolean)
    .map((segment) => segment.replace(/:\d+(?::\d+)?$/, ""));
  if (fragmentSegments.length === 0) return false;
  if (fragmentSegments.length > candidateSegments.length) return false;
  const fileSegment = fragmentSegments[fragmentSegments.length - 1];
  if (candidateSegments[candidateSegments.length - 1] !== fileSegment) return false;
  let cursor = 0;
  for (const segment of candidateSegments) {
    if (segment === fragmentSegments[cursor]) {
      cursor += 1;
      if (cursor === fragmentSegments.length) return true;
    }
  }
  return false;
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

export function repairCompactMarkdown(content: string) {
  const lines = repairCompactCodeFences(normalizeMarkdownInput(content)).split(/\r?\n/);
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
