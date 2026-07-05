#!/usr/bin/env node
/* Build the CodeBuddy proxy as a single-file Node SEA executable.
   Output: ./bin/codebuddy-proxy(.exe) — no external Node runtime required.
   Usage: `node scripts/build-sea.mjs` (after `npm run build`). */
import { build } from "esbuild";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.resolve(__dirname, "..");
const seaDir = path.join(packageRoot, ".sea");
const binDir = path.join(packageRoot, "bin");
const appBundle = path.join(seaDir, "app.mjs");
const bootstrap = path.join(seaDir, "sea-bootstrap.cjs");
const seaBlob = path.join(seaDir, "sea-prep.blob");
const seaConfig = path.join(seaDir, "sea-config.json");
const buildIdFile = path.join(seaDir, "build-id.txt");
const outputName = process.platform === "win32" ? "codebuddy-proxy.exe" : "codebuddy-proxy";
const outputBinary = path.join(binDir, outputName);

fs.rmSync(seaDir, { recursive: true, force: true });
fs.mkdirSync(seaDir, { recursive: true });
fs.mkdirSync(binDir, { recursive: true });

await build({
  entryPoints: [path.join(packageRoot, "src", "index.ts")],
  outfile: appBundle,
  bundle: true,
  platform: "node",
  format: "esm",
  target: "node22",
  sourcemap: false,
  logLevel: "info",
  banner: {
    // Inject CJS shims so SDK code that was authored against CommonJS
    // (uses `__dirname` / `__filename` / `require` directly) keeps working
    // when bundled into a single ESM file. Without this, paths like
    // `path.resolve(__dirname, "../../cli/bin/codebuddy")` inside
    // `@tencent-ai/agent-sdk` throw `ReferenceError: __dirname is not
    // defined` at runtime — the SDK was never ESM-aware, and esbuild does
    // not auto-polyfill these CJS globals for `format: "esm"`.
    js: [
      "import { createRequire as __createRequire } from 'node:module';",
      "import { fileURLToPath as __fileURLToPath } from 'node:url';",
      "import { dirname as __pathDirname } from 'node:path';",
      "const require = __createRequire(import.meta.url);",
      "const __filename = __fileURLToPath(import.meta.url);",
      "const __dirname = __pathDirname(__filename);",
    ].join(" "),
  },
});

const buildId = hashFiles([appBundle]).slice(0, 16);
fs.writeFileSync(buildIdFile, `${buildId}\n`);
fs.writeFileSync(bootstrap, bootstrapSource(), "utf8");
fs.writeFileSync(
  seaConfig,
  JSON.stringify(
    {
      main: bootstrap,
      output: seaBlob,
      disableExperimentalSEAWarning: true,
      useCodeCache: false,
      assets: {
        "app.mjs": appBundle,
        "build-id": buildIdFile,
      },
    },
    null,
    2,
  ),
);

console.log("Generating SEA blob...");
execFileSync(process.execPath, ["--experimental-sea-config", seaConfig], {
  cwd: packageRoot,
  stdio: "inherit",
});

fs.rmSync(outputBinary, { force: true });
fs.copyFileSync(process.execPath, outputBinary);
prepareExecutableForInjection(outputBinary);
injectSeaBlob(outputBinary, seaBlob);
finalizeExecutable(outputBinary);

console.log(`Built ${outputBinary}`);

function hashFiles(files) {
  const hash = createHash("sha256");
  for (const file of files) {
    hash.update(fs.readFileSync(file));
  }
  return hash.digest("hex");
}

function injectSeaBlob(binary, blob) {
  const postjectCli = path.join(
    packageRoot,
    "node_modules",
    "postject",
    "dist",
    "cli.js",
  );
  const args = [
    binary,
    "NODE_SEA_BLOB",
    blob,
    "--sentinel-fuse",
    "NODE_SEA_FUSE_fce680ab2cc467b6e072b8b5df1996b2",
  ];
  if (process.platform === "darwin") {
    args.push("--macho-segment-name", "NODE_SEA");
  }
  execFileSync(process.execPath, [postjectCli, ...args], {
    cwd: packageRoot,
    stdio: "inherit",
  });
}

function prepareExecutableForInjection(binary) {
  if (process.platform !== "win32") {
    fs.chmodSync(binary, 0o755);
  }
  if (process.platform === "darwin") {
    try {
      execFileSync("codesign", ["--remove-signature", binary], {
        cwd: packageRoot,
        stdio: "inherit",
      });
    } catch {
      /* ignore */
    }
  }
}

function finalizeExecutable(binary) {
  if (process.platform === "darwin") {
    try {
      execFileSync("codesign", ["--sign", "-", "--force", binary], {
        cwd: packageRoot,
        stdio: "inherit",
      });
    } catch {
      /* ignore */
    }
  }
  if (process.platform !== "win32") {
    fs.chmodSync(binary, 0o755);
  }
}

function bootstrapSource() {
  return [
    "const fs = require('node:fs');",
    "const os = require('node:os');",
    "const path = require('node:path');",
    "const { pathToFileURL } = require('node:url');",
    "const sea = require('node:sea');",
    "function rawAsset(name) { return Buffer.from(sea.getRawAsset(name)); }",
    "function textAsset(name) { return sea.getAsset(name, 'utf8').trim(); }",
    "function writeIfChanged(target, bytes) {",
    "  try {",
    "    const existing = fs.readFileSync(target);",
    "    if (existing.length === bytes.length && existing.equals(bytes)) return;",
    "  } catch { /* missing or unreadable; rewrite below */ }",
    "  fs.writeFileSync(target, bytes);",
    "}",
    "(async () => {",
    "  const buildId = textAsset('build-id');",
    "  const root = path.join(os.tmpdir(), 'kodex-codebuddy-proxy', buildId);",
    "  fs.mkdirSync(root, { recursive: true });",
    "  const appPath = path.join(root, 'app.mjs');",
    "  writeIfChanged(appPath, rawAsset('app.mjs'));",
    "  await import(pathToFileURL(appPath).href);",
    "})().catch((error) => {",
    "  console.error(error instanceof Error ? error.stack || error.message : error);",
    "  process.exit(1);",
    "});",
    "",
  ].join("\n");
}
