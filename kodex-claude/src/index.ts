#!/usr/bin/env node

import { resolveSettings } from "@anthropic-ai/claude-agent-sdk";
import { claudeCliPath } from "./acp-agent.js";
import { runAcp } from "./acp-runner.js";
import { listenTcpAcp, parsePortArgs } from "./acp-server.js";
import { parseWoaCli, runWoaCommand, ensureWoaBeforeAcp } from "./woa/index.js";

const parsedArgs = parsePortArgs(process.argv.slice(2));
const rawArgs = parsedArgs.args;
const hasWoaCommand =
  rawArgs.includes("--woa-login") ||
  rawArgs.includes("--woa-status") ||
  rawArgs.includes("--woa-refresh");

if (hasWoaCommand) {
  try {
    const { command, config } = parseWoaCli(rawArgs);
    if (command) {
      await runWoaCommand(command, config);
      process.exit(0);
    }
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exit(1);
  }
} else if (process.argv.includes("--cli")) {
  const { spawn } = await import("node:child_process");
  const args = process.argv.slice(2).filter((arg) => arg !== "--cli");
  const child = spawn(await claudeCliPath(), args, { stdio: "inherit" });

  const signals =
    process.platform === "win32"
      ? (["SIGINT", "SIGTERM"] as const)
      : (["SIGINT", "SIGTERM", "SIGHUP"] as const);
  for (const sig of signals) {
    process.on(sig, () => {
      if (!child.killed) child.kill(sig);
    });
  }

  child.on("exit", (code, signal) => {
    if (signal && process.platform !== "win32") {
      // Remove our listener so re-raising actually terminates instead of
      // re-entering the no-op handler, which would let us exit with code 0
      // instead of the signal's conventional 128+N.
      process.removeAllListeners(signal);
      process.kill(process.pid, signal);
    } else {
      process.exit(code ?? 1);
    }
  });
  child.on("error", (err) => {
    console.error(err);
    process.exit(1);
  });
} else {
  // Apply env vars from the managed-policy tier before any SDK call so the
  // SDK subprocess inherits them. Going through resolveSettings (vs. a raw
  // read of managed-settings.json) also picks up MDM sources on macOS and
  // HKLM/HKCU on Windows.
  const policy = await resolveSettings({ settingSources: [] });
  for (const [key, value] of Object.entries(policy.effective.env ?? {})) {
    process.env[key] = value;
  }

  // stdout is used to send messages to the client
  // we redirect everything else to stderr to make sure it doesn't interfere with ACP
  console.log = console.error;
  console.info = console.error;
  console.warn = console.error;
  console.debug = console.error;

  process.on("unhandledRejection", (reason, promise) => {
    console.error("Unhandled Rejection at:", promise, "reason:", reason);
  });

  let woaConfig;
  try {
    woaConfig = parseWoaCli(rawArgs).config;
    await ensureWoaBeforeAcp(woaConfig);
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exit(1);
  }

  const tcpServer = parsedArgs.port
    ? await listenTcpAcp(parsedArgs.port, woaConfig.enabled ? woaConfig : undefined)
    : undefined;
  const stdioAcp = tcpServer
    ? undefined
    : runAcp({ woa: woaConfig.enabled ? woaConfig : undefined });

  async function shutdown() {
    tcpServer?.server.close();
    await (tcpServer?.agent ?? stdioAcp?.agent)?.dispose().catch((err) => {
      console.error("Error during cleanup:", err);
    });
    process.exit(0);
  }

  // Exit cleanly when the ACP connection closes (e.g. stdin EOF, transport
  // error). Without this, `process.stdin.resume()` keeps the event loop
  // alive indefinitely, causing orphan process accumulation in oneshot mode.
  (tcpServer?.closed ?? stdioAcp?.connection.closed)?.then(shutdown);

  process.on("SIGTERM", shutdown);
  process.on("SIGINT", shutdown);

  if (!tcpServer) {
    // Keep process alive while connection is open
    process.stdin.resume();
  }
}
