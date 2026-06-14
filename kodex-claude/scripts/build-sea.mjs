#!/usr/bin/env node

import { build } from "esbuild";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
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
const outputName = process.platform === "win32" ? "claude-agent-acp.exe" : "claude-agent-acp";
const outputBinary = path.join(binDir, outputName);

fs.rmSync(seaDir, { recursive: true, force: true });
fs.mkdirSync(seaDir, { recursive: true });
fs.mkdirSync(binDir, { recursive: true });

const nativeClaude = findNativeClaudeBinary();
console.log(`Using Claude native binary: ${nativeClaude}`);

await build({
  entryPoints: [path.join(packageRoot, "src", "index.ts")],
  outfile: appBundle,
  bundle: true,
  platform: "node",
  format: "esm",
  target: "node22",
  sourcemap: false,
  logLevel: "info",
});

const buildId = hashFiles([appBundle, nativeClaude]).slice(0, 16);
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
        "claude-native": nativeClaude,
        "build-id": buildIdFile,
      },
    },
    null,
    2,
  ),
);

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

function findNativeClaudeBinary() {
  if (process.env.CLAUDE_CODE_EXECUTABLE) {
    const override = path.resolve(process.env.CLAUDE_CODE_EXECUTABLE);
    if (fs.existsSync(override)) {
      return override;
    }
    throw new Error(`CLAUDE_CODE_EXECUTABLE does not exist: ${override}`);
  }

  const ext = process.platform === "win32" ? ".exe" : "";
  const candidates =
    process.platform === "linux"
      ? linuxClaudeNativePackageCandidates(process.arch)
      : [`@anthropic-ai/claude-agent-sdk-${process.platform}-${process.arch}`];

  for (const packageName of candidates) {
    const candidate = path.join(
      packageRoot,
      "node_modules",
      ...packageName.split("/"),
      `claude${ext}`,
    );
    if (fs.existsSync(candidate)) {
      return candidate;
    }
  }

  throw new Error(
    `Claude native binary not found for ${process.platform}-${process.arch}. Run npm install in ${packageRoot}.`,
  );
}

function linuxClaudeNativePackageCandidates(arch) {
  const libc = process.env.KODEX_CLAUDE_NATIVE_LIBC ?? "auto";
  if (libc === "glibc") {
    return [`@anthropic-ai/claude-agent-sdk-linux-${arch}`];
  }
  if (libc === "musl") {
    return [`@anthropic-ai/claude-agent-sdk-linux-${arch}-musl`];
  }
  if (libc !== "auto") {
    throw new Error(
      `Invalid KODEX_CLAUDE_NATIVE_LIBC=${JSON.stringify(libc)}. Expected glibc, musl, or auto.`,
    );
  }
  return isMuslLibc()
    ? [
        `@anthropic-ai/claude-agent-sdk-linux-${arch}-musl`,
        `@anthropic-ai/claude-agent-sdk-linux-${arch}`,
      ]
    : [
        `@anthropic-ai/claude-agent-sdk-linux-${arch}`,
        `@anthropic-ai/claude-agent-sdk-linux-${arch}-musl`,
      ];
}

function isMuslLibc() {
  const report = process.report?.getReport?.();
  return !report?.header?.glibcVersionRuntime;
}

function hashFiles(files) {
  const hash = createHash("sha256");
  for (const file of files) {
    hash.update(fs.readFileSync(file));
  }
  return hash.digest("hex");
}

function injectSeaBlob(binary, blob) {
  const args = [
    binary,
    "NODE_SEA_BLOB",
    blob,
    "--sentinel-fuse",
    "NODE_SEA_FUSE_fce680ab2cc467b6e072b8b5df1996b2",
    "--overwrite",
  ];
  if (process.platform === "darwin") {
    args.push("--macho-segment-name", "NODE_SEA");
  }
  execFileSync(
    process.execPath,
    [path.join(packageRoot, "node_modules", "postject", "dist", "cli.js"), ...args],
    {
      cwd: packageRoot,
      stdio: "inherit",
    },
  );
}

function prepareExecutableForInjection(binary) {
  if (process.platform !== "win32") {
    fs.chmodSync(binary, 0o755);
  }

  if (process.platform === "darwin") {
    execFileSync("codesign", ["--remove-signature", binary], {
      cwd: packageRoot,
      stdio: "inherit",
    });
  }
}

function finalizeExecutable(binary) {
  if (process.platform === "darwin") {
    execFileSync("codesign", ["--sign", "-", "--force", binary], {
      cwd: packageRoot,
      stdio: "inherit",
    });
  }

  if (process.platform !== "win32") {
    fs.chmodSync(binary, 0o755);
  }
}

function bootstrapSource() {
  return String.raw`const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { pathToFileURL } = require("node:url");
const sea = require("node:sea");

function rawAsset(name) {
  return Buffer.from(sea.getRawAsset(name));
}

function textAsset(name) {
  return sea.getAsset(name, "utf8").trim();
}

function writeIfChanged(target, bytes, mode) {
  try {
    const existing = fs.readFileSync(target);
    if (existing.length === bytes.length && existing.equals(bytes)) {
      if (mode !== undefined) fs.chmodSync(target, mode);
      return;
    }
  } catch {
    // Missing or unreadable; rewrite below.
  }
  fs.writeFileSync(target, bytes, mode !== undefined ? { mode } : undefined);
}

(async () => {
  const buildId = textAsset("build-id");
  const root = path.join(resolveExtractRoot(), buildId);
  fs.mkdirSync(root, { recursive: true });

  const appPath = path.join(root, "app.mjs");
  const nativeName = process.platform === "win32" ? "claude.exe" : "claude";
  const nativePath = path.join(root, nativeName);

  writeIfChanged(appPath, rawAsset("app.mjs"));
  writeIfChanged(nativePath, rawAsset("claude-native"), process.platform === "win32" ? undefined : 0o755);

  process.env.CLAUDE_CODE_EXECUTABLE ??= nativePath;
  await import(pathToFileURL(appPath).href);
})().catch((error) => {
  console.error(error instanceof Error ? error.stack || error.message : error);
  process.exit(1);
});

function resolveExtractRoot() {
  const candidates = [];
  if (process.env.KODEX_CLAUDE_AGENT_ACP_EXTRACT_DIR) {
    candidates.push(process.env.KODEX_CLAUDE_AGENT_ACP_EXTRACT_DIR);
  }
  if (process.env.XDG_CACHE_HOME) {
    candidates.push(path.join(process.env.XDG_CACHE_HOME, "kodex", "claude-agent-acp"));
  }
  if (process.env.HOME) {
    candidates.push(path.join(process.env.HOME, ".cache", "kodex", "claude-agent-acp"));
  }
  candidates.push(path.join(os.tmpdir(), "kodex-claude-agent-acp"));

  for (const candidate of candidates) {
    if (canUseDirectory(candidate)) {
      return candidate;
    }
  }

  return path.join(os.tmpdir(), "kodex-claude-agent-acp");
}

function canUseDirectory(dir) {
  try {
    fs.mkdirSync(dir, { recursive: true });
    const probe = path.join(dir, ".write-test-" + process.pid);
    fs.writeFileSync(probe, "");
    fs.unlinkSync(probe);
    return true;
  } catch {
    return false;
  }
}
`;
}
