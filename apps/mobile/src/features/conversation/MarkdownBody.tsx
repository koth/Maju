import { memo, useMemo } from "react";
import type { ReactNode } from "react";
import Markdown from "react-native-markdown-display";
import type { ASTNode, RenderRules } from "react-native-markdown-display";
import { Text, View } from "react-native";
import { colors, spacing, radius } from "../theme";
import { repairCompactMarkdown } from "./repair-compact-markdown";
import { MathFormula } from "./MathFormula";
import { inlineMathRuns, type InlineMathRun } from "./latex-inline";
import {
  MATH_PLACEHOLDER_CLOSE,
  MATH_PLACEHOLDER_OPEN,
  prepareMathBody,
  type MathSpan,
} from "./math-markdown";

// Renders assistant/user message bodies as markdown on React Native, aligned
// with the desktop `MarkdownBody` (react-markdown + .md-* CSS in
// ConversationTimeline.css) so the same session reads identically on PC and
// phone:
//
// - Same pre-pass: `repairCompactMarkdown` (compact tables, escaped headings,
//   run-on numbered lists, literal \n breaks) runs before parsing.
// - Same typography: 15px body at 1.68 line-height, headings at 1em/600 (the
//   desktop renders h1-h6 body-sized), 12px paragraph rhythm.
// - Same code look: inline code has NO background pill — muted steel-blue
//   (#9db8d6) only, exactly like `.md-inline-code`; fenced blocks render the
//   `.md-code-block` frame (1px accent-tint border, 16px radius) with the
//   `.md-code-header` language strip.
// - Tables/blocks: GFM tables via the lib's table rules styled like
//   `.md-table`; blockquote/hr/list rhythms mirror the desktop values.
//
// Deliberately not ported: Prism syntax highlighting (no RN equivalent without
// heavy deps) — fenced code is plain mono, like the desktop with highlighting
// disabled. Inline file-path links stay non-interactive (no editor to open).

// Colors resolved from the app palette. Code blocks sit one step above the
// canvas (surfaceAlt) instead of a separate blue-black, and rules use the
// neutral hairline so inline code/blocks stop tinting the whole reply blue.
const mdText = colors.textDim;
const mdStrong = colors.text;
const mdSoft = colors.textFaint;
const mdCodeText = "#c9b8a8";
const mdCodeBlockBg = colors.surfaceAlt;
const mdCodeHeaderBg = colors.surfaceRaised;
const mdCodeBorder = colors.border;
const mdTableBorder = colors.border;
const mdQuoteBorder = colors.borderStrong;

const BODY_FONT_SIZE = 15;
const BODY_LINE_HEIGHT = 25; // 15 * 1.68, the desktop chat line-height

const markdownStyles: Record<string, object> = {
  body: { color: mdText, fontSize: BODY_FONT_SIZE, lineHeight: BODY_LINE_HEIGHT },
  paragraph: { marginTop: 0, marginBottom: 12 },
  // `react-native-markdown-display` ships a LIGHT stylesheet and deep-merges it
  // under these overrides, so any key we do not set keeps its white default.
  // The blockquote was the visible one: `backgroundColor: '#F5F5F5'` +
  // `borderColor: '#CCC'` turned every quote into a light card whose
  // `mdText`-colored text was unreadable. Transparent + no border matches the
  // desktop `.md-blockquote` (a bare accent rule, `background: none`).
  // The other light defaults (`hr` #000000, `code_inline`/`code_block`/`fence`
  // #CCCCCC + #f5f5f5, `table`/`tr` #000000, `link` underline) are overridden
  // below for the same reason.
  // Desktop headings render at 1em / weight 580 (~600) — all six levels.
  heading1: { color: mdStrong, fontSize: BODY_FONT_SIZE, fontWeight: "600", marginTop: 22, marginBottom: 10 },
  heading2: { color: mdStrong, fontSize: BODY_FONT_SIZE, fontWeight: "600", marginTop: 22, marginBottom: 10 },
  heading3: { color: mdStrong, fontSize: BODY_FONT_SIZE, fontWeight: "600", marginTop: 22, marginBottom: 10 },
  heading4: { color: mdStrong, fontSize: BODY_FONT_SIZE, fontWeight: "600", marginTop: 22, marginBottom: 10 },
  heading5: { color: mdStrong, fontSize: BODY_FONT_SIZE, fontWeight: "600", marginTop: 22, marginBottom: 10 },
  heading6: { color: mdStrong, fontSize: BODY_FONT_SIZE, fontWeight: "600", marginTop: 22, marginBottom: 10 },
  // `.md-inline-code`: no pill — color-only emphasis, muted steel-blue.
  // `borderWidth: 0` on purpose: the library default is a 1px #CCCCCC box.
  code_inline: {
    color: mdCodeText,
    backgroundColor: "transparent",
    borderWidth: 0,
    borderColor: "transparent",
    fontFamily: "monospace",
    fontSize: 14, // 0.92em of 15
    fontWeight: "400",
    paddingHorizontal: 0,
    paddingVertical: 0,
    borderRadius: 0,
  },
  // `.md-list` margin 4px 0 12px; `.md-list-item` margin-bottom 7px.
  bullet_list: { marginTop: 4, marginBottom: 12 },
  ordered_list: { marginTop: 4, marginBottom: 12 },
  list_item: { marginBottom: 7, marginTop: 0, paddingVertical: 0 },
  list_item_icon: { color: mdSoft, marginLeft: 8, marginRight: 8 },
  ordered_list_icon: { color: mdSoft, marginLeft: 8, marginRight: 8 },
  list_item_content: { flex: 1 },
  strong: { fontWeight: "600", color: mdStrong },
  em: { fontStyle: "italic" },
  s: { textDecorationLine: "line-through" },
  // `.md-link`: accent-hover, no decoration.
  link: { color: colors.accentBright, fontWeight: "400", textDecorationLine: "none" },
  // `.md-blockquote`: 2px accent(58%) left rule, 8px vertical / 12px inline
  // padding, and NO background (the library default is a light card).
  blockquote: {
    borderLeftWidth: 2,
    borderLeftColor: mdQuoteBorder,
    backgroundColor: "transparent",
    paddingLeft: 12,
    paddingRight: 12,
    paddingVertical: 4,
    marginVertical: 8,
    opacity: 1,
  },
  // `.md-hr`: 1px top rule, 10px vertical margin.
  hr: { height: 1, backgroundColor: mdTableBorder, marginVertical: 10, flex: 1 },
  // GFM tables styled like `.md-table`: 14px cells, 28px column gutters,
  // hairline row separators, no outer frame (the library default is a black
  // 1px frame with black row rules).
  table: { borderWidth: 0, borderColor: "transparent", marginTop: 8, marginBottom: 12 },
  thead: { borderBottomWidth: 1, borderBottomColor: mdTableBorder },
  tbody: {},
  tr: {
    borderBottomWidth: 1,
    borderBottomColor: mdTableBorder,
    borderColor: "transparent",
    flexDirection: "row",
  },
  th: {
    fontWeight: "600",
    color: mdStrong,
    textAlign: "left",
    paddingTop: 3,
    paddingBottom: 5,
    paddingRight: 28,
    fontSize: 14,
    lineHeight: 22,
    flexShrink: 1,
  },
  td: {
    color: mdText,
    paddingTop: 3,
    paddingBottom: 3,
    paddingRight: 28,
    fontSize: 14,
    lineHeight: 22,
    flexShrink: 1,
  },
  // Kept for the default fence rule path when the custom rule below is not
  // applied (e.g. indented code_block): match the fenced-body look.
  code_block: {
    color: mdText,
    backgroundColor: mdCodeBlockBg,
    fontFamily: "monospace",
    fontSize: 13,
    lineHeight: 20,
    padding: 12,
    borderRadius: radius.md,
    borderWidth: 1,
    borderColor: mdCodeBorder,
    marginTop: 8,
    marginBottom: 12,
  },
  fence: {
    color: mdText,
    backgroundColor: mdCodeBlockBg,
    fontFamily: "monospace",
    fontSize: 13,
    lineHeight: 20,
    padding: spacing.md,
    borderRadius: radius.md,
    borderWidth: 1,
    borderColor: mdCodeBorder,
    marginTop: 8,
    marginBottom: 12,
  },
  softbreak: { color: mdText },
};

// Custom `fence` rule mirroring the desktop `md-code-block` + `md-code-header`
// frame: 16px-radius bordered container, an uppercase language strip on top,
// then the mono body (top/right/bottom 12px padding, flush left — the desktop
// SyntaxHighlighter padding). The body Text is selectable so long-press gives
// the copy affordance the desktop button provides.
const markdownRules: RenderRules = {
  fence: (node, _children, _parentNodes, _styles, styleObj) => {
    const fenced = node as ASTNode & { sourceInfo?: string };
    const lang = typeof fenced.sourceInfo === "string" ? fenced.sourceInfo.trim() : "";
    let content = typeof fenced.content === "string" ? fenced.content : "";
    if (content.endsWith("\n")) content = content.slice(0, -1);
    return (
      <View key={node.key} style={mdStyles.codeBlock}>
        {lang.length > 0 ? (
          <View style={mdStyles.codeHeader}>
            <Text style={mdStyles.codeLang}>{lang.toLowerCase()}</Text>
          </View>
        ) : null}
        <Text style={[mdStyles.fenceBody, styleObj as object]} selectable>
          {content}
        </Text>
      </View>
    );
  },
};

// Inline math reaches the renderer as an opaque placeholder (see
// math-markdown.ts). Formatting it here rather than before parsing is what
// keeps markdown from re-reading the approximation as markup — the placeholder
// has no markdown-significant characters, the rendering may.
//
// The substitution lives in the `text` rule, so it applies everywhere text can
// appear: paragraphs, list items, table cells, headings, links.
function mathAwareTextRule(spans: MathSpan[]): NonNullable<RenderRules["text"]> {
  return (node, _children, _parentNodes, styles, inheritedStyles = {}) => {
    const content = typeof node.content === "string" ? node.content : "";
    if (!content.includes(MATH_PLACEHOLDER_OPEN)) {
      return (
        <Text key={node.key} style={[inheritedStyles, styles.text]}>
          {content}
        </Text>
      );
    }
    return (
      <Text key={node.key} style={[inheritedStyles, styles.text]}>
        {MathTextRuns({ content, spans })}
      </Text>
    );
  };
}

/// Splits one text node into literal text and inline-math runs.
function MathTextRuns({ content, spans }: { content: string; spans: MathSpan[] }) {
  const parts: ReactNode[] = [];
  let cursor = 0;
  let key = 0;

  while (cursor < content.length) {
    const open = content.indexOf(MATH_PLACEHOLDER_OPEN, cursor);
    if (open < 0) {
      parts.push(content.slice(cursor));
      break;
    }
    const close = content.indexOf(MATH_PLACEHOLDER_CLOSE, open + 1);
    if (close < 0) {
      parts.push(content.slice(cursor));
      break;
    }
    if (open > cursor) parts.push(content.slice(cursor, open));
    const span = spans[Number.parseInt(content.slice(open + 1, close), 10)];
    if (span) {
      let runKey = 0;
      for (const run of inlineMathRuns(span.tex)) {
        parts.push(
          <Text key={`m${key}:${runKey}`} style={run.italic ? mdStyles.mathItalic : undefined}>
            {run.text}
          </Text>,
        );
        runKey += 1;
      }
    }
    key += 1;
    cursor = close + 1;
  }

  return parts;
}

const mdStyles = {
  codeBlock: {
    borderWidth: 1,
    borderColor: mdCodeBorder,
    borderRadius: 16,
    backgroundColor: mdCodeBlockBg,
    overflow: "hidden",
    marginTop: 8,
    marginBottom: 12,
  } as const,
  codeHeader: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    backgroundColor: mdCodeHeaderBg,
    paddingVertical: 6,
    paddingRight: 8,
    borderBottomWidth: 1,
    borderBottomColor: mdCodeBorder,
  } as const,
  codeLang: {
    fontSize: 11,
    fontWeight: "500",
    color: mdSoft,
    textTransform: "uppercase",
    marginLeft: 12,
  } as const,
  fenceBody: {
    color: mdText,
    fontFamily: "monospace",
    fontSize: 13,
    lineHeight: 20,
    paddingTop: 12,
    paddingBottom: 12,
    paddingRight: 12,
  } as const,
  // Inline math: TeX sets variables in italic and everything else upright
  // (see latex-inline.ts). Inherits the surrounding colour and size.
  mathItalic: {
    fontStyle: "italic",
  } as const,
};

// Memoized on `body`: markdown re-parsing is the most expensive part of a
// timeline row on phones, and it ran for every mounted row on every snapshot
// emit. With memo, only rows whose body actually changed re-parse.
export const MarkdownBody = memo(function MarkdownBody({ body }: { body: string }) {
  const { pieces, spans } = useMemo(() => prepareMathBody(body), [body]);
  const rules = useMemo<RenderRules>(
    () => ({ ...markdownRules, text: mathAwareTextRule(spans) }),
    [spans],
  );

  // The common case — no display formula at all — stays a single markdown parse.
  if (pieces.length === 1 && pieces[0].kind === "text") {
    return (
      <Markdown style={markdownStyles} rules={rules}>
        {pieces[0].text}
      </Markdown>
    );
  }

  // Display formulas get their own renderer; the prose around them keeps
  // going through the native one.
  return (
    <View>
      {pieces.map((piece, index) =>
        piece.kind === "display" ? (
          <MathFormula key={`d${index}`} tex={piece.tex} />
        ) : (
          <Markdown key={`t${index}`} style={markdownStyles} rules={rules}>
            {piece.text}
          </Markdown>
        ),
      )}
    </View>
  );
});
// end of file