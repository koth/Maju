import { describe, expect, it } from "vitest";
import {
  MATH_FALLBACK_HEIGHT,
  buildMathDocument,
  readReportedHeight,
  renderMathHtml,
} from "../features/conversation/math-html";

// The document handed to the WebView is built by a pure function, so what the
// phone will render can be asserted here without a device.

describe("display math documents", () => {
  it("renders LaTeX with KaTeX in display mode", () => {
    const html = renderMathHtml("p = \\sum_{i=k}^{n} \\binom{n}{i}");

    expect(html).toContain("katex-display");
    // The source is echoed into the MathML annotation KaTeX emits.
    expect(html).toContain("application/x-tex");
    expect(html).toContain("\\sum");
  });

  it("renders a broken formula instead of throwing", () => {
    const html = renderMathHtml("\\frac{1}{");

    expect(html.length).toBeGreaterThan(0);
    expect(html).toContain("katex");
  });

  it("inlines the stylesheet so the WebView needs no filesystem or network", () => {
    const document = buildMathDocument(renderMathHtml("x^2"), "#a3a3a3");

    expect(document).toContain("@font-face");
    expect(document).toContain("data:font/woff2;base64,");
    expect(document).toContain("#a3a3a3");
    // Every resource is inline: no stylesheet/font/script is fetched. (The
    // MathML namespace URI is a name, not a request.)
    expect(/url\(\s*['"]?(?!data:)/.test(document)).toBe(false);
    expect(/<script[^>]+src=/.test(document)).toBe(false);
    expect(/<link\b/.test(document)).toBe(false);
  });

  it("reports its height back to React Native", () => {
    const document = buildMathDocument(renderMathHtml("x^2"), "#a3a3a3");

    expect(document).toContain("window.ReactNativeWebView");
    expect(document).toContain("postMessage");
    expect(document).toContain("document.fonts.ready");
  });

  it("scales a formula that is wider than the column instead of clipping it", () => {
    const document = buildMathDocument(renderMathHtml("x^2"), "#a3a3a3");

    expect(document).toContain("scrollWidth");
    expect(document).toContain("clientWidth");
    expect(document).toContain("scale(");
    // Never shrink to unreadable.
    expect(document).toContain("MIN_SCALE = 0.5");
  });

  it("accepts only usable reported heights", () => {
    expect(readReportedHeight("42")).toBe(42);
    expect(readReportedHeight("41.6")).toBe(42);
    expect(readReportedHeight("0")).toBeNull();
    expect(readReportedHeight("-3")).toBeNull();
    expect(readReportedHeight("not a number")).toBeNull();
    expect(readReportedHeight("")).toBeNull();
    // A runaway document cannot blow up the timeline row.
    expect(readReportedHeight("999999")).toBe(4000);
  });

  it("starts from a plausible fallback height", () => {
    expect(MATH_FALLBACK_HEIGHT).toBeGreaterThan(0);
    expect(MATH_FALLBACK_HEIGHT).toBeLessThan(120);
  });
});
// end of file
