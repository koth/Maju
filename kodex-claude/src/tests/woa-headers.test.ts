import { describe, expect, it } from "vitest";
import { buildWoaCustomHeaders, buildWoaEnv, getWoaGatewayUrl } from "../woa/headers.js";

describe("WOA headers and env", () => {
  const token = { accessToken: "access-token", refreshToken: "refresh-token", expiresAt: 1 };

  it("selects gateway URLs by channel", () => {
    expect(getWoaGatewayUrl("default")).toBe(
      "https://copilot.code.woa.com/server/chat/codebuddy-gateway/codebuddy-code",
    );
    expect(getWoaGatewayUrl("offline")).toBe(
      "https://copilot.code.woa.com/server/chat/codebuddy-gateway-offline/codebuddy-code",
    );
  });

  it("builds the required WOA headers", () => {
    const headers = buildWoaCustomHeaders({
      token,
      channel: "offline",
      conversationId: "session-id",
    });

    expect(headers).toContain("x-api-key: access-token");
    expect(headers).toContain("x-conversation-id: session-id");
    expect(headers).toContain("x-app-version: 1.1.7");
    expect(headers).toContain("x-app-name: codebuddy-code");
    expect(headers).toContain("x-request-platform: CodeBuddy-Code");
    expect(headers).toContain("x-scene-name: common_chat");
    expect(headers).toContain("User-Agent: Claude-Code-Internal/1.1.7");
    expect(headers).toContain("x-request-platform-v2: Claude-Code-Internal");
    expect(headers).toContain("x-app-name-v2: claude-code-internal");
    expect(headers).toContain("x-claude-code-internal: true");
    expect(headers).toContain("x-channel: offline");
  });

  it("builds WOA environment variables", () => {
    const env = buildWoaEnv({
      token,
      config: { enabled: true, channel: "default", tokenPath: "/tmp/token.json" },
      conversationId: "session-id",
    });

    expect(env).toEqual(
      expect.objectContaining({
        ANTHROPIC_BASE_URL:
          "https://copilot.code.woa.com/server/chat/codebuddy-gateway/codebuddy-code",
        ANTHROPIC_AUTH_TOKEN: "access-token",
        AUTH_TOKEN: "access-token",
        DISABLE_ERROR_REPORTING: "1",
        DISABLE_TELEMETRY: "1",
        DISABLE_AUTOUPDATER: "1",
        DISABLE_COST_WARNINGS: "1",
        CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1",
      }),
    );
    expect(env.ANTHROPIC_CUSTOM_HEADERS).toContain("x-conversation-id: session-id");
  });
});
