// Math extraction for the phone's markdown pipeline — the mobile counterpart
// of the desktop `markdown-math.ts`, with the same rules so a session reads the
// same on both surfaces.
//
// Two jobs:
//
//  1. **Protect formulas from the repair passes.** `repairCompactMarkdown` is a
//     heuristic line rewriter (it turns a literal `\n` into a real line break,
//     so `\nabla` / `\neq` / `\notin` get split in half; it re-reads `|`-heavy
//     lines as tables). Math is masked out as an opaque placeholder for the
//     duration and restored afterwards.
//  2. **Cut display formulas out of the text.** `$$…$$` and `\[…\]` become their
//     own segment so the timeline can render each one in a WebView, while the
//     surrounding prose keeps going through the native markdown renderer.
//
// The same pass normalises the delimiters models actually emit:
//  - `\[ … \]` / `\( … \)` — raw LaTeX, which markdown would otherwise print
//    with the backslashes eaten;
//  - a whole-line `$$ … $$` — parsed as INLINE math by most renderers, when the
//    author meant a display block (this is the shape in the bug report);
//  - a `$` that cannot be a delimiter (currency, `$HOME`, `$ npm i`) is
//    escaped, otherwise "价格 $5 和 $6 元" renders as the formula "5 和 ".
//
// Inline `$…$` keeps its placeholder all the way into the renderer, where the
// custom `text` rule swaps it for a typographic approximation (see
// latex-inline.ts). Substituting text earlier would let markdown re-read the
// approximation as markup.

import { repairCompactMarkdown } from "./repair-compact-markdown";

/** Placeholder brackets. Private-use code points: they cannot collide with
 *  message text, and they survive the repair passes untouched. */
export const MATH_PLACEHOLDER_OPEN = "\uE000";
export const MATH_PLACEHOLDER_CLOSE = "\uE001";
const MAX_INLINE_MATH_CHARS = 2_000;
const MAX_DISPLAY_MATH_LINES = 400;

const RAW_DISPLAY_DELIMITERS: ReadonlyArray<{ open: string; close: string }> = [
  { open: "\\[", close: "\\]" },
];

export interface MathSpan {
  kind: "display" | "inline";
  tex: string;
}

export interface MaskedMarkdown {
  /** Repair-pipeline input: every math span replaced by a single-line token. */
  text: string;
  spans: MathSpan[];
}

export type MathPiece =
  | { kind: "text"; text: string }
  | { kind: "display"; tex: string };

/**
 * Replace math spans with placeholders, recording what each one must be
 * restored to.
 */
export function maskMarkdownMath(content: string): MaskedMarkdown {
  if (
    !content.includes("$") &&
    !content.includes("\\(") &&
    !content.includes("\\[")
  ) {
    return { text: content, spans: [] };
  }

  const spans: MathSpan[] = [];
  const placeholder = (kind: MathSpan["kind"], tex: string): string => {
    const token = `${MATH_PLACEHOLDER_OPEN}${spans.length}${MATH_PLACEHOLDER_CLOSE}`;
    spans.push({ kind, tex });
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
      out.push(placeholder("display", display.value.trim()));
      index = display.lastLine + 1;
      continue;
    }

    out.push(maskInlineMathInLine(line, placeholder));
    index += 1;
  }

  return { text: out.join("\n"), spans };
}

/// Whether a masked line is a display-math placeholder and nothing else.
function displayPlaceholderIndex(line: string, spans: MathSpan[]): number | null {
  const match = /^\uE000(\d+)\uE001$/.exec(line);
  if (!match) return null;
  const index = Number.parseInt(match[1], 10);
  return spans[index]?.kind === "display" ? index : null;
}

/**
 * Split repaired markdown into the pieces the renderer draws: prose segments
 * (which still carry inline-math placeholders) and display formulas, each of
 * which gets its own renderer.
 */
export function splitMathPieces(masked: string, spans: MathSpan[]): MathPiece[] {
  if (spans.length === 0) return [{ kind: "text", text: masked }];

  const pieces: MathPiece[] = [];
  let buffer: string[] = [];
  const flush = () => {
    if (buffer.length === 0) return;
    // A display formula owns its line; blank lines around it stay in the prose
    // so markdown still sees the paragraph break.
    const text = buffer.join("\n");
    if (text.trim().length > 0) pieces.push({ kind: "text", text });
    buffer = [];
  };

  for (const line of masked.split("\n")) {
    const displayIndex = displayPlaceholderIndex(line, spans);
    if (displayIndex === null) {
      buffer.push(line);
      continue;
    }
    flush();
    pieces.push({ kind: "display", tex: spans[displayIndex].tex });
  }
  flush();

  return pieces.length > 0 ? pieces : [{ kind: "text", text: masked }];
}

/// Mask math, repair, then split into prose segments and display formulas.
/// Pure — no React Native import — so the whole pipeline is covered by tests
/// instead of a device.
export function prepareMathBody(body: string): { pieces: MathPiece[]; spans: MathSpan[] } {
  const masked = maskMarkdownMath(body);
  const repaired = repairCompactMarkdown(masked.text);
  return { pieces: splitMathPieces(repaired, masked.spans), spans: masked.spans };
}

function stripCarriageReturn(line: string): string {
  return line.endsWith("\r") ? line.slice(0, -1) : line;
}

function fenceMarkerOf(line: string): string | null {
  const match = /^\s{0,3}(```+|~~~+)/.exec(line);
  return match ? match[1].slice(0, 3) : null;
}

function isFenceClose(line: string, fence: string): boolean {
  return new RegExp(`^\\s{0,3}${fence}\\s*$`).test(line);
}

/**
 * A whole-line `$$…$$` (or `\[…\]`) is display math. `$$` may also open a block
 * that closes several lines later — the shape models use for `aligned`
 * environments.
 *
 * Only column 0 counts: an indented or list-prefixed `$$` belongs to its block
 * and is left to the inline scan.
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
    const middle = lines.slice(index + 1, candidate).map(stripCarriageReturn);
    const value = [rest, ...middle, candidateLine.slice(0, at)].join("\n");
    if (value.trim() === "") return null;
    return { value, lastLine: candidate };
  }
  // Unterminated (usually: the reply is still streaming in) — leave it alone.
  return null;
}

/**
 * Mask inline math on one line, escaping the `$` characters that cannot be
 * delimiters. Inline code spans are copied verbatim, and `\$` stays escaped.
 */
function maskInlineMathInLine(
  line: string,
  placeholder: (kind: MathSpan["kind"], tex: string) => string,
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
          out += placeholder("inline", value.trim());
          index = closeAt + close.length;
          continue;
        }
      }
      out += line.slice(index, index + 2);
      index += 2;
      continue;
    }

    if (char === "$") {
      const run = countRun(line, index, "$");
      const closeAt = findDollarClose(line, index, run);
      if (closeAt < 0) {
        // Currency, a shell variable, or a formula still streaming in.
        out += run === 1 ? "\\$" : line.slice(index, index + run);
        index += run;
        continue;
      }
      const value = line.slice(index + run, closeAt);
      out += placeholder("inline", run === 1 ? value : value.trim());
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
 * Position of the `$` run that closes a math span opened at `start`, or -1 when
 * the span is not a delimiter pair. The single-`$` rules are the ones TeX
 * auto-renderers settled on, and they are what keeps money and shell variables
 * out of the renderer.
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
// end of file
