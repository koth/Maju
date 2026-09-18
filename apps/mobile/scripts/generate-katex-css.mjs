// Generates `src/features/conversation/katex-css.generated.ts`.
//
// The phone renders display math in a WebView (see MathFormula.tsx), so KaTeX
// needs its stylesheet AND its fonts inside that document. A WebView started
// from an HTML string has no filesystem to load a relative `url(fonts/...)`
// from — and the app ships no HTTP server — so both are inlined: the woff2
// files become `data:` URIs inside the same stylesheet.
//
// Run after upgrading `katex`:
//
//   node scripts/generate-katex-css.mjs
//
// The generated file is committed on purpose: `katex` is only needed at build
// time for this asset, and regenerating it requires nothing but the package.

import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const appRoot = path.resolve(here, "..");
const katexDist = path.join(appRoot, "node_modules", "katex", "dist");
const outputFile = path.join(
  appRoot,
  "src",
  "features",
  "conversation",
  "katex-css.generated.ts",
);

const css = readFileSync(path.join(katexDist, "katex.min.css"), "utf8");

// Keep woff2 only: it is the newest format every target WebView (Chromium /
// WKWebView) supports, and the css lists woff + ttf fallbacks that would
// triple the payload for no reachable benefit.
let inlined = 0;
const fontsCss = css.replace(/@font-face\{[^}]*\}/g, (block) => {
  const family = /font-family:([^;]+)/.exec(block)?.[1]?.trim();
  const woff2 = /url\(fonts\/([^)"']+\.woff2)\)/.exec(block)?.[1];
  if (!family || !woff2) return block;
  const base64 = readFileSync(path.join(katexDist, "fonts", woff2)).toString("base64");
  inlined += 1;
  const weight = /font-weight:([^;]+)/.exec(block)?.[1]?.trim();
  const style = /font-style:([^;]+)/.exec(block)?.[1]?.trim();
  return [
    "@font-face{",
    `font-family:${family};`,
    `font-style:${style ?? "normal"};`,
    `font-weight:${weight ?? "normal"};`,
    `src:url(data:font/woff2;base64,${base64}) format("woff2");`,
    "}",
  ].join("");
});

if (inlined === 0) {
  throw new Error("no KaTeX @font-face blocks found — did katex.min.css change shape?");
}

const banner = `// GENERATED FILE — do not edit by hand.
//
// KaTeX's stylesheet with every woff2 font inlined as a \`data:\` URI, so a
// WebView created from an HTML string can typeset display math with no
// filesystem or network access.
//
// Regenerate with: node scripts/generate-katex-css.mjs
// Source: node_modules/katex/dist/katex.min.css (${inlined} fonts inlined)

/* eslint-disable */
export const KATEX_CSS = ${JSON.stringify(fontsCss)};
`;

writeFileSync(outputFile, banner, "utf8");
console.log(
  `wrote ${path.relative(appRoot, outputFile)} (${inlined} fonts, ${Math.round(
    banner.length / 1024,
  )} KB)`,
);
