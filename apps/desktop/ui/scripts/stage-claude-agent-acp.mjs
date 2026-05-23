#!/usr/bin/env node

import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  rmSync,
  statSync,
} from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const uiDir = path.resolve(scriptDir, "..");
const repoRoot = path.resolve(uiDir, "../../..");
const binaryName =
  process.platform === "win32" ? "claude-agent-acp.exe" : "claude-agent-acp";
const source = path.join(repoRoot, "kodex-claude", "bin", binaryName);

if (!existsSync(source)) {
  console.error(
    `[claude:stage] Missing ${source}. Run npm --prefix apps/desktop/ui run claude:binary first.`,
  );
  process.exit(1);
}

const destinationDirs = [
  path.join(
    repoRoot,
    "apps",
    "desktop",
    "src-tauri",
    "resources",
    "claude-agent-acp",
  ),
];

const releaseResourceDir = path.join(
  repoRoot,
  "target",
  "release",
  "bundled-claude-agent-acp",
);

if (existsSync(releaseResourceDir)) {
  destinationDirs.push(releaseResourceDir);
}

for (const destinationDir of destinationDirs) {
  stageBinary(source, destinationDir, binaryName);
}

const { size } = statSync(source);
console.log(`[claude:stage] staged ${binaryName} (${size} bytes)`);

function stageBinary(sourcePath, destinationDir, outputName) {
  mkdirSync(destinationDir, { recursive: true });

  for (const staleName of [
    "claude-agent-acp",
    "claude-agent-acp.exe",
    "claude-agent-acp.cmd",
    "package",
  ]) {
    rmSync(path.join(destinationDir, staleName), {
      force: true,
      recursive: true,
    });
  }

  const destination = path.join(destinationDir, outputName);
  copyFileSync(sourcePath, destination);

  if (process.platform !== "win32") {
    chmodSync(destination, 0o755);
  }

  console.log(`[claude:stage] ${sourcePath} -> ${destination}`);
}
