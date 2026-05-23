import * as os from "node:os";
import * as path from "node:path";
import { WoaError } from "./error.js";

export const WOA_CHANNELS = ["default", "offline"] as const;
export type WoaChannel = (typeof WOA_CHANNELS)[number];

export type WoaConfig = {
  enabled: boolean;
  channel: WoaChannel;
  tokenPath: string;
};

export type WoaConfigInput = {
  argv?: readonly string[];
  env?: Record<string, string | undefined>;
  homeDir?: string;
};

export function defaultWoaTokenPath(homeDir = os.homedir()) {
  return path.join(homeDir, ".claude-woa-token.json");
}

export function parseWoaChannel(value: string | undefined, fallback: WoaChannel = "default") {
  if (!value || value.trim() === "") {
    return fallback;
  }
  const normalized = value.trim().toLowerCase();
  if (normalized === "default" || normalized === "offline") {
    return normalized;
  }
  throw new WoaError(
    "invalid_channel",
    `Invalid WOA channel "${value}". Expected "default" or "offline".`,
  );
}

export function isWoaEnabledValue(value: string | undefined) {
  if (!value) {
    return false;
  }
  const normalized = value.trim().toLowerCase();
  return normalized === "1" || normalized === "true" || normalized === "yes" || normalized === "on";
}

function optionValue(argv: readonly string[], name: string): string | undefined {
  const equalsPrefix = `${name}=`;
  for (let index = 0; index < argv.length; index++) {
    const arg = argv[index];
    if (arg === name) {
      const next = argv[index + 1];
      return next && !next.startsWith("--") ? next : undefined;
    }
    if (arg.startsWith(equalsPrefix)) {
      return arg.slice(equalsPrefix.length);
    }
  }
  return undefined;
}

export function parseWoaConfig(input: WoaConfigInput = {}): WoaConfig {
  const argv = input.argv ?? process.argv.slice(2);
  const env = input.env ?? process.env;
  const channelValue = optionValue(argv, "--woa-channel") ?? env.CLAUDE_WOA_CHANNEL;
  const tokenPath = optionValue(argv, "--woa-token-path") ?? env.CLAUDE_WOA_TOKEN_PATH;

  return {
    enabled: argv.includes("--woa") || isWoaEnabledValue(env.CLAUDE_ACP_WOA),
    channel: parseWoaChannel(channelValue),
    tokenPath: tokenPath ?? defaultWoaTokenPath(input.homeDir),
  };
}
