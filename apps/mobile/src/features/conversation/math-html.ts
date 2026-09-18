// Display-math rendering for the phone: LaTeX → KaTeX HTML → a self-contained
// WebView document.
//
// Display formulas get a WebView because that is the only way to lay out real
// TeX on React Native — `react-native-svg` would mean porting KaTeX's
// absolutely-positioned vlist model by hand, and RN has no inline layout at all
// (see latex-inline.ts for how inline math is handled instead).
//
// The document is built here as a pure function of the formula so it can be
// unit-tested without a device; `MathFormula.tsx` only owns the WebView.

import katex from "katex";
import { KATEX_CSS } from "./katex-css.generated";

/// Display math is set one step above the 15px body, the way the desktop
/// does, so a formula reads as a block rather than as prose.
export const MATH_FONT_SIZE = 17;

/// Height used before the document reports its real one. Kept close to a
/// single-line formula so the correction is small.
export const MATH_FALLBACK_HEIGHT = 46;

const MIN_HEIGHT = 12;
const MAX_HEIGHT = 4000;

/**
 * KaTeX rendering with the failure modes an LLM's LaTeX actually produces:
 * `throwOnError: false` prints the offending source in red instead of taking
 * down the message, and `strict: "ignore"` accepts the unicode and bare CJK
 * that show up inside `$…$` / `\text{…}`.
 */
export function renderMathHtml(tex: string): string {
  return katex.renderToString(tex, {
    displayMode: true,
    throwOnError: false,
    strict: "ignore",
    trust: false,
  });
}

/**
 * A minimal document around one rendered formula. Everything it needs is
 * inlined — the stylesheet carries its fonts as `data:` URIs (see
 * scripts/generate-katex-css.mjs), so the WebView never touches the network or
 * the filesystem.
 *
 * Layout is handled entirely here:
 *  - a formula wider than the column is scaled down to fit rather than clipped
 *    (the phone is ~360dp wide and real display math regularly exceeds that);
 *  - the resulting height is reported through
 *    `ReactNativeWebView.postMessage`, once immediately, again once the KaTeX
 *    fonts have loaded (they change the metrics), and on resize.
 */
export function buildMathDocument(html: string, color: string, fontSize = MATH_FONT_SIZE): string {
  return `<!doctype html>
<html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, maximum-scale=1, user-scalable=no">
<style>
${KATEX_CSS}
html,body{margin:0;padding:0;background:transparent;color:${color};font-size:${fontSize}px;-webkit-text-size-adjust:100%;}
#math{display:block;padding:2px 0;}
.katex-display{margin:0;text-align:center;}
/* An inline-block that fills the row is never offset off-screen when the
   formula does overflow. */
.katex-display>.katex{display:inline-block;min-width:100%;}
.katex-error{font-family:monospace;font-size:0.9em;}
</style></head>
<body><div id="math">${html}</div>
<script>
(function () {
  var host = window.ReactNativeWebView;
  var node = document.getElementById("math");
  if (!node) return;
  var MIN_SCALE = 0.5;

  function report() {
    if (!host) return;
    var rect = node.getBoundingClientRect();
    var height = Math.ceil(rect.height);
    if (height > 0) host.postMessage(String(height));
  }

  function layout() {
    // Reset, measure the natural width, then scale only if it overflows.
    node.style.transform = "";
    node.style.width = "";
    var available = document.documentElement.clientWidth;
    var needed = node.scrollWidth;
    if (available > 0 && needed > available) {
      var scale = Math.max(MIN_SCALE, available / needed);
      node.style.transformOrigin = "top left";
      node.style.width = available / scale + "px";
      node.style.transform = "scale(" + scale + ")";
    }
    report();
  }

  layout();
  requestAnimationFrame(layout);
  if (document.fonts && document.fonts.ready) {
    document.fonts.ready.then(layout).catch(function () {});
  }
  window.addEventListener("resize", layout);
})();
</script></body></html>`;
}

/** Parse a height reported by the document. Returns null for anything unusable. */
export function readReportedHeight(message: string): number | null {
  const height = Number.parseFloat(message);
  if (!Number.isFinite(height) || height < MIN_HEIGHT) return null;
  return Math.min(Math.round(height), MAX_HEIGHT);
}
// end of file
