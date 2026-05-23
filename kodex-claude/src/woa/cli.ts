import { ensureWoaToken, getWoaTokenStatus, loginWoa, refreshAndSaveWoaToken } from "./auth.js";
import { parseWoaConfig, WoaConfig } from "./config.js";
import { maskSecret, WoaToken } from "./token.js";

export type WoaCommand = "login" | "status" | "refresh";

export type WoaCliParseResult = {
  command?: WoaCommand;
  config: WoaConfig;
};

export function parseWoaCli(argv = process.argv.slice(2), env = process.env): WoaCliParseResult {
  const command = argv.includes("--woa-login")
    ? "login"
    : argv.includes("--woa-status")
      ? "status"
      : argv.includes("--woa-refresh")
        ? "refresh"
        : undefined;
  return {
    command,
    config: parseWoaConfig({ argv, env }),
  };
}

export function formatTokenSaved(token: WoaToken, tokenPath: string) {
  return [
    `WOA token saved to ${tokenPath}`,
    `accessToken: ${maskSecret(token.accessToken)}`,
    `refreshToken: ${maskSecret(token.refreshToken)}`,
    `expiresAt: ${new Date(token.expiresAt).toISOString()}`,
  ].join("\n");
}

export function formatWoaStatus(status: Awaited<ReturnType<typeof getWoaTokenStatus>>) {
  if (!status.exists) {
    return [
      "WOA token: missing",
      `channel: ${status.channel}`,
      `tokenPath: ${status.tokenPath}`,
      "Run `claude-agent-acp --woa-login` to complete WOA login.",
    ].join("\n");
  }
  return [
    "WOA token: present",
    `channel: ${status.channel}`,
    `tokenPath: ${status.tokenPath}`,
    `accessToken: ${status.accessToken}`,
    `refreshToken: ${status.refreshToken}`,
    `expiresAt: ${status.expiresAt}`,
    `validForMinutes: ${status.validForMinutes}`,
    `refreshNeeded: ${status.refreshNeeded}`,
  ].join("\n");
}

export async function runWoaCommand(
  command: WoaCommand,
  config: WoaConfig,
  io: { log?: (message: string) => void; write?: (message: string) => void } = console,
) {
  if (command === "login") {
    const token = await loginWoa(config, { io });
    io.log?.(formatTokenSaved(token, config.tokenPath));
    return;
  }
  if (command === "refresh") {
    const token = await refreshAndSaveWoaToken(config);
    io.log?.(formatTokenSaved(token, config.tokenPath));
    return;
  }
  const status = await getWoaTokenStatus(config);
  io.log?.(formatWoaStatus(status));
}

export async function ensureWoaBeforeAcp(config: WoaConfig) {
  if (!config.enabled) {
    return;
  }
  await ensureWoaToken(config);
}
