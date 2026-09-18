import { describe, expect, it } from "vitest";
import { inlineMathRuns, inlineMathText } from "../features/conversation/latex-inline";

// Inline math cannot be handed to KaTeX on React Native (no inline-block), so
// the phone approximates it with Unicode. These tests pin what a reader
// actually sees.

describe("inline math typography", () => {
  it("sets variables in italic and numbers upright", () => {
    expect(inlineMathRuns("p=0.0009")).toEqual([
      { text: "p", italic: true },
      { text: " = 0.0009", italic: false },
    ]);
  });

  it("spaces operators by class, not by what the source happened to contain", () => {
    // TeX does this in math mode; without it `a+b` reads as a word.
    expect(inlineMathText("a+b")).toBe("a + b");
    expect(inlineMathText("a + b")).toBe("a + b");
    expect(inlineMathText("f(x)=2x")).toBe("f(x) = 2x");
    expect(inlineMathText("p<0.05")).toBe("p < 0.05");
  });

  it("renders subscripts and superscripts as real glyphs", () => {
    expect(inlineMathText("H_0")).toBe("H₀");
    expect(inlineMathText("x^2")).toBe("x²");
    expect(inlineMathText("x_i^2")).toBe("xᵢ²");
    expect(inlineMathText("a_{i+1}")).toBe("aᵢ₊₁");
  });

  it("falls back to a caret when a glyph does not exist", () => {
    // There is no superscript 'q'; a caret reads correctly.
    expect(inlineMathText("x^q")).toBe("x^q");
    // Scripts are set tight, so no operator spacing sneaks into the exponent.
    expect(inlineMathText("e^{-\\lambda t}")).toBe("e^(-λt)");
    // Multi-character scripts use the glyphs that do exist.
    expect(inlineMathText("x^{ab}")).toBe("xᵃᵇ");
  });

  it("expands greek letters and operators", () => {
    expect(inlineMathText("\\alpha + \\beta \\le \\gamma")).toBe("α + β ≤ γ");
    expect(inlineMathText("\\sum_{i=1}^{n} x_i")).toBe("∑ᵢ₌₁ⁿ xᵢ");
    expect(inlineMathText("x \\neq 0")).toBe("x ≠ 0");
    // TeX eats the space that terminates a control word.
    expect(inlineMathText("\\nabla f")).toBe("∇f");
    expect(inlineMathText("O(n)")).toBe("O(n)");
  });

  it("keeps text argument upright but leaves the math after it alone", () => {
    expect(inlineMathRuns("\\text{Binomial}(n, 1/2)")).toEqual([
      { text: "Binomial(", italic: false },
      { text: "n", italic: true },
      { text: ", 1/2)", italic: false },
    ]);
    expect(inlineMathText("\\operatorname{Var}(X)")).toBe("Var(X)");
    expect(inlineMathText("\\text{其中 } x > 0")).toBe("其中 x > 0");
  });

  it("combines accents with the preceding character", () => {
    expect(inlineMathText("\\hat{p} = 0.65")).toBe("p\u0302 = 0.65");
    expect(inlineMathText("\\bar{x}")).toBe("x\u0304");
    expect(inlineMathText("\\vec{v}")).toBe("v\u20D7");
  });

  it("degrades fractions and radicals to a readable form", () => {
    expect(inlineMathText("\\frac{1}{2}")).toBe("1/2");
    expect(inlineMathText("\\sqrt{x}")).toBe("√x");
    expect(inlineMathText("\\sqrt[3]{x}")).toBe("³√x");
    expect(inlineMathText("\\frac{a+b}{2}")).toBe("a + b/2");
  });

  it("keeps an unknown macro's name instead of dropping it", () => {
    expect(inlineMathText("\\argmax_x f(x)")).toBe("argmaxₓ f(x)");
  });

  it("strips layout-only macros", () => {
    expect(inlineMathText("\\left(\\frac{1}{2}\\right)^n")).toBe("(1/2)ⁿ");
    expect(inlineMathText("a \\quad b")).toBe("a   b");
  });

  it("never emits an empty result for a real formula", () => {
    for (const tex of ["H_0", "O(n)", "\\hat{p}", "\\sum_{i=k}^{n}", "\\frac{1}{2}"]) {
      expect(inlineMathText(tex).length).toBeGreaterThan(0);
    }
  });
});
// end of file
