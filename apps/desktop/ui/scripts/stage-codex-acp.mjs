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
const binaryName = process.platform === "win32" ? "codex-acp.exe" : "codex-acp";
const expectedHost = expectedHostTriple();

if (expectedHost && expectedHost !== `${process.platform}-${process.arch}`) {
  console.error(
    `[codex:stage] Host platform mismatch: expected ${expectedHost}, got ${process.platform}-${process.arch}.`,
  );
  process.exit(1);
}

const source = findSource();

if (!source) {
  console.error(
    `[codex:stage] Missing codex-acp binary. Run npm --prefix apps/desktop/ui run codex:build first, or set KODEX_CODEX_ACP_BINARY.`,
  );
  process.exit(1);
}

const destinationDirs = [
  path.join(repoRoot, "apps", "desktop", "src-tauri", "resources", "codex-acp"),
];

const releaseResourceDir = path.join(
  repoRoot,
  "target",
  "release",
  "bundled-codex-acp",
);

if (existsSync(releaseResourceDir)) {
  destinationDirs.push(releaseResourceDir);
}

for (const destinationDir of destinationDirs) {
  stageBinary(source, destinationDir, binaryName);
}

const { size } = statSync(source);
console.log(`[codex:stage] staged ${binaryName} (${size} bytes)`);

function findSource() {
  const candidates = [];

  if (process.env.KODEX_CODEX_ACP_BINARY) {
    candidates.push(process.env.KODEX_CODEX_ACP_BINARY);
  }

  if (process.env.KODEX_CODEX_ACP_TARGET) {
    candidates.push(
      path.join(
        repoRoot,
        "codex-acp",
        "target",
        process.env.KODEX_CODEX_ACP_TARGET,
        "release",
        binaryName,
      ),
    );
  }

  candidates.push(
    path.join(repoRoot, "codex-acp", "target", "release", binaryName),
    path.join(repoRoot, "target", "release", binaryName),
  );

  return candidates.find((candidate) => existsSync(candidate));
}

function expectedHostTriple() {
  const platform = process.env.KODEX_EXPECTED_PLATFORM;
  const arch = process.env.KODEX_EXPECTED_ARCH;

  if (!platform && !arch) {
    return null;
  }

  return `${platform ?? process.platform}-${arch ?? process.arch}`;
}

function stageBinary(sourcePath, destinationDir, outputName) {
  mkdirSync(destinationDir, { recursive: true });

  for (const staleName of ["codex-acp", "codex-acp.exe"]) {
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

  console.log(`[codex:stage] ${sourcePath} -> ${destination}`);
}
