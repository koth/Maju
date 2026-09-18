import { describe, expect, it } from "vitest";
import { maskMarkdownMath, mathPlaceholderForTests } from "./markdown-math";
import { repairCompactMarkdown, splitMarkdownBlocks } from "./MarkdownBody";

/** Mask → run the repair tiers → restore, i.e. exactly what MarkdownBody does. */
function roundTrip(content: string) {
  const math = maskMarkdownMath(content);
  return math.restore(repairCompactMarkdown(math.text));
}

describe("maskMarkdownMath", () => {
  it("turns a whole-line $$…$$ into one display block", () => {
    const masked = maskMarkdownMath("$$p = \\sum_{i=k}^{n} \\binom{n}{i}$$");

    expect(masked.text).toBe(mathPlaceholderForTests(0));
    expect(masked.restore(masked.text)).toBe(
      "$$\np = \\sum_{i=k}^{n} \\binom{n}{i}\n$$",
    );
  });

  it("keeps a multi-line display block on a single masked line", () => {
    const masked = maskMarkdownMath("$$\na + b\n\nc + d\n$$");

    expect(masked.text).toBe(mathPlaceholderForTests(0));
    expect(masked.text.includes("\n")).toBe(false);
    expect(masked.restore(masked.text)).toContain("a + b\n\nc + d");
    // The blank line inside the formula must not become a block boundary.
    expect(splitMarkdownBlocks(masked.text)).toHaveLength(1);
  });

  it("converts raw LaTeX delimiters markdown would otherwise eat", () => {
    const masked = maskMarkdownMath("\\[x^2\\]");

    expect(masked.restore(masked.text)).toBe("$$\nx^2\n$$");
    expect(maskMarkdownMath("公式 \\(y\\) 结束").restore(
      maskMarkdownMath("公式 \\(y\\) 结束").text,
    )).toBe("公式 $y$ 结束");
  });

  it("keeps an inline $$…$$ inline", () => {
    const masked = maskMarkdownMath("前面 $$x$$ 后面");

    expect(masked.text).toBe(`前面 ${mathPlaceholderForTests(0)} 后面`);
    expect(masked.restore(masked.text)).toBe("前面 $$x$$ 后面");
  });

  it("escapes dollars that cannot be delimiters", () => {
    expect(maskMarkdownMath("价格 $5 和 $6 元").text).toBe("价格 \\$5 和 \\$6 元");
    expect(maskMarkdownMath("$HOME/.kodex 与 $TEMP").text).toBe(
      "\\$HOME/.kodex 与 \\$TEMP",
    );
    expect(maskMarkdownMath("$ npm run build").text).toBe("\\$ npm run build");
    // A closing `$` directly followed by a digit is the classic `$5 and $6`.
    expect(maskMarkdownMath("$a$2").text).toBe("\\$a\\$2");
  });

  it("leaves real inline math alone", () => {
    const masked = maskMarkdownMath("其中 $p=0.5$ 是原假设");

    expect(masked.text).toBe(`其中 ${mathPlaceholderForTests(0)} 是原假设`);
    expect(masked.restore(masked.text)).toBe("其中 $p=0.5$ 是原假设");
  });

  it("ignores dollar signs inside code", () => {
    const fenced = "```sh\necho $HOME\n$$\n```";
    expect(maskMarkdownMath(fenced).text).toBe(fenced);

    const inline = "跑 `$HOME` 和 `$x$`";
    expect(maskMarkdownMath(inline).text).toBe(inline);
  });

  it("leaves an unterminated formula alone while it streams in", () => {
    // `$$` is kept verbatim — escaping it would flash extra characters as the
    // rest of the block arrives chunk by chunk.
    expect(maskMarkdownMath("$$p = \\sum").text).toBe("$$p = \\sum");
    // A lone `$` is escaped instead: identical rendered text, and it can never
    // be re-read as a delimiter by a later repair pass.
    expect(maskMarkdownMath("文字 $x").text).toBe("文字 \\$x");
  });

  it("does not touch messages without math", () => {
    const plain = "没有公式，只有 `code` 和 **重点**。";
    expect(maskMarkdownMath(plain).text).toBe(plain);
  });
});

describe("repair passes with math", () => {
  it("keeps \\n-prefixed macros from being read as line breaks", () => {
    const formula = "$$\\nabla f \\neq 0 \\text{ 且 } x \\notin S$$";

    expect(roundTrip(formula)).toBe(
      "$$\n\\nabla f \\neq 0 \\text{ 且 } x \\notin S\n$$",
    );
  });

  it("keeps pipe-heavy formulas out of the compact-table repair", () => {
    const formula = "$$\\left| a \\right| + \\left| b \\right| \\ge | a + b |$$";

    expect(roundTrip(formula)).toBe(
      "$$\n\\left| a \\right| + \\left| b \\right| \\ge | a + b |\n$$",
    );
  });

  it("survives a whole message the way an assistant writes one", () => {
    const message = [
      "数学上就是：",
      "",
      "$$p = \\sum_{i=k}^{n} \\binom{n}{i} \\left(\\frac{1}{2}\\right)^n$$",
      "",
      "零假设 $H_0$：策略相对恒等序没有改善。",
    ].join("\n");

    const rendered = roundTrip(message);

    expect(rendered).toContain(
      "$$\np = \\sum_{i=k}^{n} \\binom{n}{i} \\left(\\frac{1}{2}\\right)^n\n$$",
    );
    expect(rendered).toContain("零假设 $H_0$");
    expect(rendered.includes("\uE000")).toBe(false);
  });
});
