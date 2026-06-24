import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { AgentSideConnection, SessionNotification } from "@agentclientprotocol/sdk";
import type { Options } from "@anthropic-ai/claude-agent-sdk";
import type { ClaudeAcpAgent as ClaudeAcpAgentType } from "../acp-agent.js";

let capturedOptions: Options | undefined;
const KODEX_RULES_MARKER = "Do not guess APIs; consult the documentation first.";

function systemPromptAppend(): string {
  const systemPrompt = capturedOptions!.systemPrompt;
  if (typeof systemPrompt !== "object" || systemPrompt === null || Array.isArray(systemPrompt)) {
    throw new Error("Expected object systemPrompt");
  }
  return String((systemPrompt as { append?: unknown }).append ?? "");
}
vi.mock("@anthropic-ai/claude-agent-sdk", async () => {
  const actual = await vi.importActual<typeof import("@anthropic-ai/claude-agent-sdk")>(
    "@anthropic-ai/claude-agent-sdk",
  );
  return {
    ...actual,
    query: (args: { prompt: unknown; options: Options }) => {
      capturedOptions = args.options;
      return {
        initializationResult: async () => ({
          models: [
            {
              value: "claude-sonnet-4-6",
              displayName: "Claude Sonnet",
              description: "Fast",
              supportsAutoMode: true,
            },
          ],
        }),
        setModel: async () => {},
        setPermissionMode: async () => {},
        supportedCommands: async () => [],
        [Symbol.asyncIterator]: async function* () {},
      };
    },
  };
});

vi.mock("../tools.js", async () => {
  const actual = await vi.importActual<typeof import("../tools.js")>("../tools.js");
  return {
    ...actual,
    registerHookCallback: vi.fn(),
  };
});

describe("createSession options merging", () => {
  let agent: ClaudeAcpAgentType;
  let ClaudeAcpAgent: typeof ClaudeAcpAgentType;

  function createMockClient(): AgentSideConnection {
    return {
      sessionUpdate: async (_notification: SessionNotification) => {},
      requestPermission: async () => ({ outcome: { outcome: "cancelled" } }),
      readTextFile: async () => ({ content: "" }),
      writeTextFile: async () => ({}),
    } as unknown as AgentSideConnection;
  }

  beforeEach(async () => {
    capturedOptions = undefined;

    vi.resetModules();
    const acpAgent = await import("../acp-agent.js");
    ClaudeAcpAgent = acpAgent.ClaudeAcpAgent;

    agent = new ClaudeAcpAgent(createMockClient());
  });

  it("preserves user-provided disallowedTools", async () => {
    await agent.newSession({
      cwd: "/test",
      mcpServers: [],
      _meta: {
        claudeCode: {
          options: {
            disallowedTools: ["WebSearch", "WebFetch"],
          },
        },
      },
    });

    // User-provided tools should be present
    expect(capturedOptions!.disallowedTools).toContain("WebSearch");
    expect(capturedOptions!.disallowedTools).toContain("WebFetch");
    expect(capturedOptions!.disallowedTools).not.toContain("AskUserQuestion");
  });

  it("does not disable AskUserQuestion when user provides no disallowedTools", async () => {
    await agent.newSession({
      cwd: "/test",
      mcpServers: [],
    });

    expect(capturedOptions!.disallowedTools ?? []).not.toContain("AskUserQuestion");
  });

  it("does not add internal disallowedTools when user provides an empty list", async () => {
    await agent.newSession({
      cwd: "/test",
      mcpServers: [],
      _meta: {
        claudeCode: {
          options: {
            disallowedTools: [],
          },
        },
      },
    });

    expect(capturedOptions!.disallowedTools).toEqual([]);
  });

  it("sets tools to empty array when disableBuiltInTools is true", async () => {
    await agent.newSession({
      cwd: "/test",
      mcpServers: [],
      _meta: {
        disableBuiltInTools: true,
        claudeCode: {
          options: {
            disallowedTools: ["CustomTool"],
          },
        },
      },
    });

    // disableBuiltInTools removes all built-in tools from context
    expect(capturedOptions!.tools).toEqual([]);
    // User-provided disallowedTools still apply.
    expect(capturedOptions!.disallowedTools).toContain("CustomTool");
    expect(capturedOptions!.disallowedTools).not.toContain("AskUserQuestion");
  });

  it("merges user-provided hooks with ACP hooks", async () => {
    const userPreToolUseHook = { hooks: [{ command: "echo pre" }] };

    await agent.newSession({
      cwd: "/test",
      mcpServers: [],
      _meta: {
        claudeCode: {
          options: {
            hooks: {
              PreToolUse: [userPreToolUseHook],
              PostToolUse: [{ hooks: [{ command: "echo user-post" }] }],
            },
          },
        },
      },
    });

    // User's PreToolUse hooks should be preserved
    expect(capturedOptions!.hooks?.PreToolUse).toEqual([userPreToolUseHook]);
    // PostToolUse should contain both user and ACP hooks
    expect(capturedOptions!.hooks?.PostToolUse).toHaveLength(2);
  });

  it("inherits HOME and PATH from process.env when no env is provided", async () => {
    await agent.newSession({
      cwd: "/test",
      mcpServers: [],
    });

    expect(capturedOptions?.env?.HOME).toBe(process.env.HOME);
    expect(capturedOptions?.env?.PATH).toBe(process.env.PATH);
  });

  it("merges user-provided env vars on top of process.env", async () => {
    await agent.newSession({
      cwd: "/test",
      mcpServers: [],
      _meta: {
        claudeCode: {
          options: {
            env: {
              CUSTOM_VAR: "custom-value",
            },
          },
        },
      },
    });

    expect(capturedOptions?.env?.HOME).toBe(process.env.HOME);
    expect(capturedOptions?.env?.PATH).toBe(process.env.PATH);
    expect(capturedOptions?.env?.CUSTOM_VAR).toBe("custom-value");
  });

  it("allows user-provided env vars to override process.env entries", async () => {
    await agent.newSession({
      cwd: "/test",
      mcpServers: [],
      _meta: {
        claudeCode: {
          options: {
            env: {
              HOME: "/custom/home",
            },
          },
        },
      },
    });

    expect(capturedOptions?.env?.HOME).toBe("/custom/home");
  });

  it("defaults tools to claude_code preset when not provided", async () => {
    await agent.newSession({
      cwd: "/test",
      mcpServers: [],
    });

    expect(capturedOptions!.tools).toEqual({ type: "preset", preset: "claude_code" });
  });

  it("passes through user-provided tools string array", async () => {
    await agent.newSession({
      cwd: "/test",
      mcpServers: [],
      _meta: {
        claudeCode: {
          options: {
            tools: ["Read", "Glob"],
          },
        },
      },
    });

    expect(capturedOptions!.tools).toEqual(["Read", "Glob"]);
  });

  it("explicit tools array takes precedence over disableBuiltInTools", async () => {
    await agent.newSession({
      cwd: "/test",
      mcpServers: [],
      _meta: {
        disableBuiltInTools: true,
        claudeCode: {
          options: {
            tools: ["Read", "Glob"],
          },
        },
      },
    });

    expect(capturedOptions!.tools).toEqual(["Read", "Glob"]);
  });

  it("passes through empty tools array to disable all built-in tools", async () => {
    await agent.newSession({
      cwd: "/test",
      mcpServers: [],
      _meta: {
        claudeCode: {
          options: {
            tools: [],
          },
        },
      },
    });

    expect(capturedOptions!.tools).toEqual([]);
  });

  describe("systemPrompt via _meta", () => {
    it("defaults to the claude_code preset when not provided", async () => {
      await agent.newSession({ cwd: "/test", mcpServers: [] });

      expect(capturedOptions!.systemPrompt).toEqual({
        type: "preset",
        preset: "claude_code",
        append: expect.stringContaining(KODEX_RULES_MARKER),
      });
    });

    it("preserves custom string prompts and appends Kodex rules", async () => {
      await agent.newSession({
        cwd: "/test",
        mcpServers: [],
        _meta: { systemPrompt: "custom prompt" },
      });

      expect(capturedOptions!.systemPrompt).toEqual(expect.stringContaining("custom prompt"));
      expect(capturedOptions!.systemPrompt).toEqual(expect.stringContaining(KODEX_RULES_MARKER));
    });

    it("appends Kodex rules to claudeCode option system prompts", async () => {
      await agent.newSession({
        cwd: "/test",
        mcpServers: [],
        _meta: {
          claudeCode: {
            options: {
              systemPrompt: "sdk option prompt",
            },
          },
        },
      });

      expect(capturedOptions!.systemPrompt).toEqual(expect.stringContaining("sdk option prompt"));
      expect(capturedOptions!.systemPrompt).toEqual(expect.stringContaining(KODEX_RULES_MARKER));
    });

    it("forwards append and adds Kodex rules", async () => {
      await agent.newSession({
        cwd: "/test",
        mcpServers: [],
        _meta: { systemPrompt: { append: "extra instructions" } },
      });

      expect(capturedOptions!.systemPrompt).toEqual({
        type: "preset",
        preset: "claude_code",
        append: expect.stringContaining("extra instructions"),
      });
      expect(systemPromptAppend()).toContain(KODEX_RULES_MARKER);
    });

    it("forwards excludeDynamicSections", async () => {
      await agent.newSession({
        cwd: "/test",
        mcpServers: [],
        _meta: { systemPrompt: { excludeDynamicSections: true } },
      });

      expect(capturedOptions!.systemPrompt).toEqual({
        type: "preset",
        preset: "claude_code",
        excludeDynamicSections: true,
        append: expect.stringContaining(KODEX_RULES_MARKER),
      });
    });

    it("forwards append and excludeDynamicSections together", async () => {
      await agent.newSession({
        cwd: "/test",
        mcpServers: [],
        _meta: {
          systemPrompt: {
            append: "extra instructions",
            excludeDynamicSections: true,
          },
        },
      });

      expect(capturedOptions!.systemPrompt).toEqual({
        type: "preset",
        preset: "claude_code",
        append: expect.stringContaining("extra instructions"),
        excludeDynamicSections: true,
      });
      expect(systemPromptAppend()).toContain(KODEX_RULES_MARKER);
    });

    it("ignores caller-provided type/preset overrides", async () => {
      await agent.newSession({
        cwd: "/test",
        mcpServers: [],
        _meta: {
          systemPrompt: {
            type: "something-else",
            preset: "other-preset",
            append: "extra",
          },
        },
      });

      expect(capturedOptions!.systemPrompt).toEqual({
        type: "preset",
        preset: "claude_code",
        append: expect.stringContaining("extra"),
      });
      expect(systemPromptAppend()).toContain(KODEX_RULES_MARKER);
    });
  });

  describe("CLAUDE_MODEL_CONFIG", () => {
    let originalModelConfig: string | undefined;

    beforeEach(() => {
      originalModelConfig = process.env.CLAUDE_MODEL_CONFIG;
      delete process.env.CLAUDE_MODEL_CONFIG;
    });

    afterEach(() => {
      if (originalModelConfig !== undefined) {
        process.env.CLAUDE_MODEL_CONFIG = originalModelConfig;
      } else {
        delete process.env.CLAUDE_MODEL_CONFIG;
      }
    });

    it("passes modelOverrides as settings", async () => {
      process.env.CLAUDE_MODEL_CONFIG = JSON.stringify({
        modelOverrides: { "claude-opus-4-6": "us.anthropic.claude-opus-4-6-v1" },
      });

      await agent.newSession({ cwd: "/test", mcpServers: [] });

      expect(capturedOptions!.settings).toEqual({
        modelOverrides: { "claude-opus-4-6": "us.anthropic.claude-opus-4-6-v1" },
      });
    });

    it("passes availableModels as settings", async () => {
      process.env.CLAUDE_MODEL_CONFIG = JSON.stringify({
        availableModels: ["opus", "sonnet"],
      });

      await agent.newSession({ cwd: "/test", mcpServers: [] });

      expect(capturedOptions!.settings).toEqual({
        availableModels: ["opus", "sonnet"],
      });
    });

    it("passes both modelOverrides and availableModels", async () => {
      process.env.CLAUDE_MODEL_CONFIG = JSON.stringify({
        modelOverrides: { "claude-opus-4-6": "us.anthropic.claude-opus-4-6-v1" },
        availableModels: ["opus"],
      });

      await agent.newSession({ cwd: "/test", mcpServers: [] });

      expect(capturedOptions!.settings).toEqual({
        modelOverrides: { "claude-opus-4-6": "us.anthropic.claude-opus-4-6-v1" },
        availableModels: ["opus"],
      });
    });

    it("does not add settings when env var is not set", async () => {
      await agent.newSession({ cwd: "/test", mcpServers: [] });

      expect(capturedOptions!.settings).toBeUndefined();
    });

    it("ignores env var when _meta provides settings", async () => {
      process.env.CLAUDE_MODEL_CONFIG = JSON.stringify({
        modelOverrides: { "claude-opus-4-6": "us.anthropic.claude-opus-4-6-v1" },
      });

      await agent.newSession({
        cwd: "/test",
        mcpServers: [],
        _meta: {
          claudeCode: {
            options: {
              settings: {
                model: "claude-sonnet-4-6",
                modelOverrides: { "claude-opus-4-6": "meta-value" },
              },
            },
          },
        },
      });

      // _meta settings take precedence; env var is ignored entirely
      expect(capturedOptions!.settings).toEqual({
        model: "claude-sonnet-4-6",
        modelOverrides: { "claude-opus-4-6": "meta-value" },
      });
    });

    it("throws on invalid JSON", async () => {
      process.env.CLAUDE_MODEL_CONFIG = "not-json";

      await expect(agent.newSession({ cwd: "/test", mcpServers: [] })).rejects.toThrow();
    });
  });

  it("merges user-provided mcpServers with ACP mcpServers", async () => {
    await agent.newSession({
      cwd: "/test",
      mcpServers: [
        {
          name: "acp-server",
          command: "node",
          args: ["acp-server.js"],
          env: [],
        },
      ],
      _meta: {
        claudeCode: {
          options: {
            mcpServers: {
              "user-server": {
                type: "stdio",
                command: "node",
                args: ["server.js"],
              },
            },
          },
        },
      },
    });

    // User-provided MCP server should be present
    expect(capturedOptions!.mcpServers).toHaveProperty("user-server");
    // ACP-provided MCP server should also be present
    expect(capturedOptions!.mcpServers).toHaveProperty("acp-server");
  });

  it("injects WOA env after user and generic gateway env", async () => {
    const tempDir = await fs.mkdtemp(path.join(os.tmpdir(), "woa-session-"));
    try {
      const tokenPath = path.join(tempDir, "token.json");
      const { saveWoaToken } = await import("../woa/token.js");
      await saveWoaToken(tokenPath, {
        accessToken: "woa-access",
        refreshToken: "woa-refresh",
        expiresAt: Date.now() + 60 * 60 * 1000,
      });

      agent = new ClaudeAcpAgent(createMockClient(), {
        woa: { enabled: true, channel: "offline", tokenPath },
      });
      await agent.authenticate({
        methodId: "gateway",
        _meta: {
          gateway: {
            baseUrl: "https://gateway.example",
            headers: { "x-api-key": "generic" },
          },
        },
      });

      const response = await agent.newSession({
        cwd: "/test",
        mcpServers: [],
        _meta: {
          claudeCode: {
            options: {
              env: {
                ANTHROPIC_BASE_URL: "https://user.example",
                ANTHROPIC_AUTH_TOKEN: "user-token",
                ANTHROPIC_CUSTOM_HEADERS: "x-user: value",
                CLAUDE_CONFIG_DIR: "/tmp/claude-internal",
                CLAUDE_SETTINGS_DIR: "/tmp/claude-settings",
              },
            },
          },
        },
      });

      expect(capturedOptions!.env).toEqual(
        expect.objectContaining({
          ANTHROPIC_BASE_URL:
            "https://copilot.code.woa.com/server/chat/codebuddy-gateway-offline/codebuddy-code",
          ANTHROPIC_AUTH_TOKEN: "woa-access",
          AUTH_TOKEN: "woa-access",
          CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS: "1",
          DISABLE_TELEMETRY: "1",
        }),
      );
      expect(capturedOptions!.env!.ANTHROPIC_CUSTOM_HEADERS).toContain("x-api-key: woa-access");
      expect(capturedOptions!.env!.ANTHROPIC_CUSTOM_HEADERS).toContain("x-channel: offline");
      expect(capturedOptions!.env!.ANTHROPIC_CUSTOM_HEADERS).toContain(
        `x-conversation-id: ${response.sessionId}`,
      );
      expect(capturedOptions!.env).not.toHaveProperty("CLAUDE_CONFIG_DIR");
      expect(capturedOptions!.env).not.toHaveProperty("CLAUDE_SETTINGS_DIR");
    } finally {
      await fs.rm(tempDir, { recursive: true, force: true });
    }
  });
});
