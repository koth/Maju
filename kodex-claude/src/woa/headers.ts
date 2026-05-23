import { WoaChannel, WoaConfig } from "./config.js";
import { WoaToken } from "./token.js";

export const WOA_GATEWAY_URLS: Record<WoaChannel, string> = {
  default: "https://copilot.code.woa.com/server/chat/codebuddy-gateway/codebuddy-code",
  offline: "https://copilot.code.woa.com/server/chat/codebuddy-gateway-offline/codebuddy-code",
};

export function getWoaGatewayUrl(channel: WoaChannel) {
  return WOA_GATEWAY_URLS[channel];
}

export function buildWoaCustomHeaders(options: {
  token: WoaToken;
  channel: WoaChannel;
  conversationId: string;
}) {
  return [
    ["x-api-key", options.token.accessToken],
    ["x-conversation-id", options.conversationId],
    ["x-app-version", "1.1.7"],
    ["x-app-name", "codebuddy-code"],
    ["x-request-platform", "CodeBuddy-Code"],
    ["x-scene-name", "common_chat"],
    ["User-Agent", "Claude-Code-Internal/1.1.7"],
    ["x-request-platform-v2", "Claude-Code-Internal"],
    ["x-app-name-v2", "claude-code-internal"],
    ["x-claude-code-internal", "true"],
    ["x-channel", options.channel],
  ]
    .map(([key, value]) => `${key}: ${value}`)
    .join("\n");
}

export function buildWoaEnv(options: {
  token: WoaToken;
  config: WoaConfig;
  conversationId: string;
}): Record<string, string | undefined> {
  return {
    ANTHROPIC_BASE_URL: getWoaGatewayUrl(options.config.channel),
    ANTHROPIC_AUTH_TOKEN: options.token.accessToken,
    AUTH_TOKEN: options.token.accessToken,
    ANTHROPIC_CUSTOM_HEADERS: buildWoaCustomHeaders({
      token: options.token,
      channel: options.config.channel,
      conversationId: options.conversationId,
    }),
    DISABLE_ERROR_REPORTING: "1",
    DISABLE_TELEMETRY: "1",
    DISABLE_AUTOUPDATER: "1",
    DISABLE_COST_WARNINGS: "1",
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1",
    CLAUDE_CONFIG_DIR: undefined,
    CLAUDE_SETTINGS_DIR: undefined,
  };
}
