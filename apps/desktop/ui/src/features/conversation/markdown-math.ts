/**
 * Math-aware preprocessing for chat markdown.
 *
 * Typesetting itself is remark-math + rehype-katex (wired up in
 * `MarkdownBody.tsx`). This module exists because two layers run BEFORE the
 * parser and were written for prose, so they silently corrupt LaTeX:
 *
 *  1. the compact-markdown repair passes (`repairCompactMarkdownNormalized` /
 *     `repairCompactMarkdownBlockLines`) rewrite lines heuristically. The
 *     whole-message pass alone turns any `\n`-prefixed macro into a real line
 *     break — `\nabla`, `\neq`, `\nu`, `\notin` all break — and the block pass
 *     happily re-reads `|`-heavy formulas as tables;
 *  2. `splitMarkdownBlocks` cuts a message at blank lines, which would tear a
 *     multi-line `$$…$$` display block in half.
 *
 * Both are solved by replacing every math span with an opaque single-line
 * placeholder for the duration of the repair pipeline and restoring the
 * canonical markdown afterwards ({@link maskMarkdownMath}).
 *
 * The same pass normalises the delimiters models actually emit:
 *  - `\[ … \]` and `\( … \)` — raw LaTeX, which markdown would otherwise print
 *    with the backslashes eaten (`\[x^2\]` → `[x^2]`);
 *  - a whole-line `$$ … $$`: remark-math reads that as INLINE math and
 *    typesets it at text size, while the author clearly meant a display block
 *    (this is the single most common shape in real assistant output);
 *  - a `$` that cannot be a delimiter — currency, `$HOME`, `$ npm run build` —
 *    is escaped, otherwise "价格 $5 和 $6 元" renders as the formula "5 和 ".
 */

/** Placeholder brackets. Private-use code points: they cannot collide with
 *  message text, and they survive every repair pass untouched (no repair regex
 *  keys off a non-ASCII, non-whitespace, non-punctuation character). */
const PLACEHOLDER_OPEN = "\uE000";
const PLACEHOLDER_CLOSE = "\uE001";

/** Bail out of a pathological span instead of scanning forever. */
const MAX_INLINE_MATH_CHARS = 2_000;
const MAX_DISPLAY_MATH_LINES = 400;

/** Raw-LaTeX display delimiters, longest first so `\[` never eats `\(`. */
const RAW_DISPLAY_DELIMITERS: ReadonlyArray<{ open: string; close: string }> = [
  { open: "\\[", close: "\\]" },
];

export interface MarkdownMathMask {
  /** Repair-pipeline input: every math span replaced by a single-line token. */
  text: string;
  /** Put the canonical math markdown back into a (repaired) string. */
  restore(text: string): string;
}

/** Cheap identity used when the message contains no math delimiter at all —
 *  the overwhelmingly common case, and the streaming hot path. */
function identityMask(content: string): MarkdownMathMask {
  return { text: content, restore: (text: string) => text };
}

/**
 * Replace math spans with placeholders and keep the canonical markdown they
 * must be restored to. The returned `restore` is a pure function of its
 * argument, so it is safe to call on any block string (including cached
 * repaired blocks).
 */
export function maskMarkdownMath(content: string): MarkdownMathMask {
  if (
    !content.includes("$") &&
    !content.includes("\\(") &&
    !content.includes("\\[")
  ) {
    return identityMask(content);
  }

  const canonical: string[] = [];
  const placeholder = (markdown: string): string => {
    const token = `${PLACEHOLDER_OPEN}${canonical.length}${PLACEHOLDER_CLOSE}`;
    canonical.push(markdown);
    return token;
  };

  const lines = content.split("\n");
  const out: string[] = [];
  let fence: string | null = null;
  let index = 0;

  while (index < lines.length) {
    const raw = lines[index];
    const line = stripCarriageReturn(raw);

    // Fenced code is literal: never look for math inside it.
    if (fence !== null) {
      out.push(raw);
      if (isFenceClose(line, fence)) fence = null;
      index += 1;
      continue;
    }
    const openingFence = fenceMarkerOf(line);
    if (openingFence !== null) {
      fence = openingFence;
      out.push(raw);
      index += 1;
      continue;
    }

    const display = matchDisplayMath(lines, index);
    if (display !== null) {
      out.push(placeholder(`$$\n${display.value.trim()}\n$$`));
      index = display.lastLine + 1;
      continue;
    }

    out.push(maskInlineMathInLine(line, placeholder));
    index += 1;
  }

  const text = out.join("\n");
  if (canonical.length === 0) return { text, restore: (value: string) => value };
  return {
    text,
    restore: (value: string) => restorePlaceholders(value, canonical),
  };
}

function stripCarriageReturn(line: string): string {
  return line.endsWith("\r") ? line.slice(0, -1) : line;
}

/** Same fence grammar as `splitMarkdownBlocks`, so masking and splitting agree
 *  on what a code block is. */
function fenceMarkerOf(line: string): string | null {
  const match = /^\s{0,3}(```+|~~~+)/.exec(line);
  return match ? match[1].slice(0, 3) : null;
}

function isFenceClose(line: string, fence: string): boolean {
  return new RegExp(`^\\s{0,3}${fence}\\s*$`).test(line);
}

/**
 * A whole-line `$$…$$` (or `\[…\]`) is display math, not the inline math
 * remark-math would otherwise parse it as. `$$` may also open a block that
 * closes several lines later — that is the shape models use for `aligned`
 * environments, and collapsing it here is what keeps `splitMarkdownBlocks`
 * from cutting the formula in half at an internal blank line.
 *
 * Only column 0 counts: an indented or list-prefixed `$$` belongs to its block
 * and is left to the inline scan (promoting it would escape the list).
 */
function matchDisplayMath(
  lines: string[],
  index: number,
): { value: string; lastLine: number } | null {
  const line = stripCarriageReturn(lines[index]);
  if (line.trim() !== line) return null;

  const delimiter = line.startsWith("$$")
    ? { open: "$$", close: "$$" }
    : RAW_DISPLAY_DELIMITERS.find((candidate) => line.startsWith(candidate.open));
  if (!delimiter) return null;

  const rest = line.slice(delimiter.open.length);
  const closeOnSameLine = rest.indexOf(delimiter.close);
  if (closeOnSameLine >= 0) {
    const value = rest.slice(0, closeOnSameLine);
    const trailing = rest.slice(closeOnSameLine + delimiter.close.length);
    // `$$x$$ and more` is an inline span, not a display block.
    if (trailing.trim() !== "" || value.trim() === "") return null;
    return { value, lastLine: index };
  }

  const limit = Math.min(lines.length, index + MAX_DISPLAY_MATH_LINES);
  for (let candidate = index + 1; candidate < limit; candidate += 1) {
    const candidateLine = stripCarriageReturn(lines[candidate]);
    const at = candidateLine.indexOf(delimiter.close);
    if (at < 0) continue;
    if (candidateLine.slice(at + delimiter.close.length).trim() !== "") return null;
    const middle = lines
      .slice(index + 1, candidate)
      .map(stripCarriageReturn);
    const value = [rest, ...middle, candidateLine.slice(0, at)].join("\n");
    if (value.trim() === "") return null;
    return { value, lastLine: candidate };
  }
  // Unterminated (usually: the reply is still streaming in) — leave it alone.
  return null;
}

/**
 * Mask inline math on one line, escaping the `$` characters that cannot be
 * delimiters. Inline code spans are copied verbatim, and an already escaped
 * `\$` stays escaped.
 */
function maskInlineMathInLine(
  line: string,
  placeholder: (markdown: string) => string,
): string {
  let out = "";
  let index = 0;

  while (index < line.length) {
    const char = line[index];

    if (char === "`") {
      const run = countRun(line, index, "`");
      const closeAt = findBacktickClose(line, index + run, run);
      if (closeAt < 0) {
        out += line.slice(index);
        break;
      }
      out += line.slice(index, closeAt + run);
      index = closeAt + run;
      continue;
    }

    if (char === "\\") {
      const next = line[index + 1];
      if (next === "(" || next === "[") {
        const close = next === "(" ? "\\)" : "\\]";
        const closeAt = line.indexOf(close, index + 2);
        const value = closeAt < 0 ? "" : line.slice(index + 2, closeAt);
        if (value.trim() !== "") {
          // Mid-line raw LaTeX renders inline; a whole-line `\[…\]` was already
          // promoted to a display block by matchDisplayMath.
          out += placeholder(`$${value.trim()}$`);
          index = closeAt + close.length;
          continue;
        }
      }
      // Any other escape pair (`\\`, `\$`, `\alpha` in prose) is literal.
      out += line.slice(index, index + 2);
      index += 2;
      continue;
    }

    if (char === "$") {
      const run = countRun(line, index, "$");
      const closeAt = findDollarClose(line, index, run);
      if (closeAt < 0) {
        // Not a delimiter pair: currency, a shell variable, or a formula that
        // is still streaming in. Escape a single `$` so prose stays prose.
        out += run === 1 ? "\\$" : line.slice(index, index + run);
        index += run;
        continue;
      }
      const value = line.slice(index + run, closeAt);
      out +=
        run === 1
          ? placeholder(`$${value}$`)
          : placeholder(`$$${value}$$`);
      index = closeAt + run;
      continue;
    }

    out += char;
    index += 1;
  }

  return out;
}

function countRun(value: string, start: number, char: string): number {
  let run = 0;
  while (value[start + run] === char) run += 1;
  return run;
}

/** Closing backtick run must be the same length as the opening one. */
function findBacktickClose(line: string, from: number, run: number): number {
  const marker = "`".repeat(run);
  for (let at = from; at < line.length; ) {
    const next = line.indexOf(marker, at);
    if (next < 0) return -1;
    if (line[next + run] !== "`" && line[next - 1] !== "`") return next;
    at = next + countRun(line, next, "`");
  }
  return -1;
}

/**
 * Position of the `$` run that closes a math span opened at `start`, or -1
 * when the span is not a delimiter pair.
 *
 * The single-`$` rules are the ones TeX auto-renderers settled on, and they are
 * what keeps money and shell variables out of the renderer:
 *  - the opening `$` must be followed by a non-space,
 *  - the closing `$` must be preceded by a non-space,
 *  - the closing `$` must not be followed by a digit (`$5 and $6`),
 *  - an escaped `\$` never closes.
 */
function findDollarClose(line: string, start: number, run: number): number {
  if (run === 1) {
    if (line[start + 1] === undefined || /\s/.test(line[start + 1])) return -1;
  }

  let at = start + run;
  while (at < line.length) {
    const next = line.indexOf("$", at);
    if (next < 0) return -1;
    if (line[next - 1] === "\\") {
      at = next + 1;
      continue;
    }
    const size = countRun(line, next, "$");
    if (size !== run) {
      at = next + size;
      continue;
    }
    const value = line.slice(start + run, next);
    if (value.length === 0 || value.length > MAX_INLINE_MATH_CHARS) return -1;
    if (run === 1) {
      if (/\s$/.test(value)) {
        at = next + size;
        continue;
      }
      if (/\d/.test(line[next + run] ?? "")) {
        at = next + size;
        continue;
      }
    }
    return next;
  }
  return -1;
}

/** Single left-to-right pass: placeholders are index-carrying, so nesting or
 *  ordering can never confuse the swap back. */
function restorePlaceholders(value: string, canonical: string[]): string {
  if (!value.includes(PLACEHOLDER_OPEN)) return value;
  let out = "";
  let index = 0;
  while (index < value.length) {
    const open = value.indexOf(PLACEHOLDER_OPEN, index);
    if (open < 0) {
      out += value.slice(index);
      break;
    }
    const close = value.indexOf(PLACEHOLDER_CLOSE, open + 1);
    if (close < 0) {
      out += value.slice(index);
      break;
    }
    const position = Number.parseInt(
      value.slice(open + PLACEHOLDER_OPEN.length, close),
      10,
    );
    const markdown = canonical[position];
    if (markdown === undefined) {
      out += value.slice(index, close + PLACEHOLDER_CLOSE.length);
    } else {
      out += value.slice(index, open) + markdown;
    }
    index = close + PLACEHOLDER_CLOSE.length;
  }
  return out;
}

/** Test hook: the raw token a masked span is replaced with. */
export function mathPlaceholderForTests(position: number): string {
  return `${PLACEHOLDER_OPEN}${position}${PLACEHOLDER_CLOSE}`;
}
