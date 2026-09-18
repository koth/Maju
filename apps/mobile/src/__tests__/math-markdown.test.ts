import { describe, expect, it } from "vitest";
import {
  MATH_PLACEHOLDER_CLOSE,
  MATH_PLACEHOLDER_OPEN,
  maskMarkdownMath,
  prepareMathBody,
  splitMathPieces,
} from "../features/conversation/math-markdown";

// The phone extracts math with the same rules the desktop uses, so one session
// reads the same on both surfaces. Everything here is pure string work.

describe("mobile math masking", () => {
  it("masks a whole-line $$…$$ as one display formula", () => {
    const masked = maskMarkdownMath("$$p = \\sum_{i=k}^{n} \\binom{n}{i}$$");

    expect(masked.text).toBe(`${MATH_PLACEHOLDER_OPEN}0${MATH_PLACEHOLDER_CLOSE}`);
    expect(masked.spans).toEqual([
      { kind: "display", tex: "p = \\sum_{i=k}^{n} \\binom{n}{i}" },
    ]);
  });

  it("keeps a multi-line display block in one span", () => {
    const masked = maskMarkdownMath("$$\na + b\n\nc + d\n$$");

    expect(masked.text.includes("\n")).toBe(false);
    expect(masked.spans).toEqual([{ kind: "display", tex: "a + b\n\nc + d" }]);
  });

  it("converts raw LaTeX delimiters markdown would otherwise eat", () => {
    expect(maskMarkdownMath("\\[x^2\\]").spans).toEqual([{ kind: "display", tex: "x^2" }]);
    expect(maskMarkdownMath("公式 \\(y\\) 结束").spans).toEqual([
      { kind: "inline", tex: "y" },
    ]);
  });

  it("keeps an inline $$…$$ inline", () => {
    const masked = maskMarkdownMath("前面 $$x$$ 后面");

    expect(masked.text).toBe(`前面 ${MATH_PLACEHOLDER_OPEN}0${MATH_PLACEHOLDER_CLOSE} 后面`);
    expect(masked.spans).toEqual([{ kind: "inline", tex: "x" }]);
  });

  it("escapes dollars that cannot be delimiters", () => {
    expect(maskMarkdownMath("价格 $5 和 $6 元").text).toBe("价格 \\$5 和 \\$6 元");
    expect(maskMarkdownMath("$HOME/.kodex 与 $TEMP").text).toBe("\\$HOME/.kodex 与 \\$TEMP");
    expect(maskMarkdownMath("$ npm run build").text).toBe("\\$ npm run build");
    // A closing `$` directly followed by a digit is the classic `$5 and $6`.
    expect(maskMarkdownMath("$a$2").text).toBe("\\$a\\$2");
  });

  it("leaves real inline math alone", () => {
    const masked = maskMarkdownMath("其中 $p=0.5$ 是原假设");

    expect(masked.text).toBe(`其中 ${MATH_PLACEHOLDER_OPEN}0${MATH_PLACEHOLDER_CLOSE} 是原假设`);
    expect(masked.spans).toEqual([{ kind: "inline", tex: "p=0.5" }]);
  });

  it("ignores dollar signs inside code", () => {
    const fenced = "```sh\necho $HOME\n$$\n```";
    expect(maskMarkdownMath(fenced).text).toBe(fenced);
    expect(maskMarkdownMath(fenced).spans).toEqual([]);

    const inline = "跑 `$HOME` 和 `$x$`";
    expect(maskMarkdownMath(inline).text).toBe(inline);
  });

  it("leaves an unterminated formula alone while it streams in", () => {
    expect(maskMarkdownMath("$$p = \\sum").text).toBe("$$p = \\sum");
    expect(maskMarkdownMath("文字 $x").text).toBe("文字 \\$x");
  });

  it("does not touch messages without math", () => {
    const plain = "没有公式，只有 `code` 和 **重点**。";
    const masked = maskMarkdownMath(plain);
    expect(masked.text).toBe(plain);
    expect(masked.spans).toEqual([]);
  });
});

describe("mobile math segmentation", () => {
  it("splits a message into prose and display formulas", () => {
    const pieces = prepareMathBody(
      ["数学上就是：", "", "$$p = x$$", "", "零假设 $H_0$：没有改善。"].join("\n"),
    ).pieces;

    expect(pieces.map((piece) => piece.kind)).toEqual(["text", "display", "text"]);
    expect(pieces[1]).toEqual({ kind: "display", tex: "p = x" });
    // The inline placeholder survives into the prose piece; the renderer's text
    // rule resolves it (substituting here would let markdown re-read the
    // rendering as markup).
    expect(pieces[2].kind === "text" && pieces[2].text).toContain(MATH_PLACEHOLDER_OPEN);
  });

  it("produces a single prose piece when there is no display formula", () => {
    const { pieces, spans } = prepareMathBody("只有行内 $x^2$ 公式。");

    expect(pieces).toHaveLength(1);
    expect(pieces[0].kind).toBe("text");
    expect(spans).toEqual([{ kind: "inline", tex: "x^2" }]);
  });

  it("keeps the prose around a formula intact", () => {
    const pieces = splitMathPieces(
      `${MATH_PLACEHOLDER_OPEN}0${MATH_PLACEHOLDER_CLOSE}`,
      [{ kind: "display", tex: "x" }],
    );

    expect(pieces).toEqual([{ kind: "display", tex: "x" }]);
  });

  it("drops blank-only prose pieces between formulas", () => {
    const pieces = splitMathPieces(
      [
        `${MATH_PLACEHOLDER_OPEN}0${MATH_PLACEHOLDER_CLOSE}`,
        "",
        `${MATH_PLACEHOLDER_OPEN}1${MATH_PLACEHOLDER_CLOSE}`,
      ].join("\n"),
      [
        { kind: "display", tex: "a" },
        { kind: "display", tex: "b" },
      ],
    );

    expect(pieces).toEqual([
      { kind: "display", tex: "a" },
      { kind: "display", tex: "b" },
    ]);
  });

  it("survives the compact-markdown repair passes", () => {
    // `\nabla` / `\neq` / `\notin` all start with `\n`; the literal-line-break
    // repair used to split them across lines.
    const pieces = prepareMathBody(
      "$$\\nabla f \\neq 0 \\text{ 且 } x \\notin S$$",
    ).pieces;

    expect(pieces).toEqual([
      { kind: "display", tex: "\\nabla f \\neq 0 \\text{ 且 } x \\notin S" },
    ]);
  });

  it("survives a whole message the way an assistant writes one", () => {
    const message = [
      "数学上就是：",
      "",
      "$$p = \\sum_{i=k}^{n} \\binom{n}{i} \\left(\\frac{1}{2}\\right)^n = P(X \\ge k)$$",
      "",
      "零假设 $H_0$：策略相对恒等序没有改善 —— 即 gain 正负号是公平抛硬币(p=0.5)。",
    ].join("\n");

    const { pieces, spans } = prepareMathBody(message);

    expect(pieces).toHaveLength(3);
    expect(spans[0].tex).toBe(
      "p = \\sum_{i=k}^{n} \\binom{n}{i} \\left(\\frac{1}{2}\\right)^n = P(X \\ge k)",
    );
    expect(spans[1]).toEqual({ kind: "inline", tex: "H_0" });
  });
});
// end of file
