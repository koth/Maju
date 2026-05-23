import { describe, expect, it } from "vitest";
import { parseWoaCli } from "../woa/cli.js";
import { defaultWoaTokenPath, parseWoaConfig, parseWoaChannel } from "../woa/config.js";
import { WoaError } from "../woa/error.js";

describe("WOA config", () => {
  it("defaults to disabled default channel and home token path", () => {
    expect(parseWoaConfig({ argv: [], env: {}, homeDir: "/home/test" })).toEqual({
      enabled: false,
      channel: "default",
      tokenPath: defaultWoaTokenPath("/home/test"),
    });
  });

  it("parses WOA env values", () => {
    expect(
      parseWoaConfig({
        argv: [],
        env: {
          CLAUDE_ACP_WOA: "1",
          CLAUDE_WOA_CHANNEL: "offline",
          CLAUDE_WOA_TOKEN_PATH: "/tokens/woa.json",
        },
        homeDir: "/home/test",
      }),
    ).toEqual({
      enabled: true,
      channel: "offline",
      tokenPath: "/tokens/woa.json",
    });
  });

  it("lets CLI values override env values", () => {
    expect(
      parseWoaConfig({
        argv: ["--woa", "--woa-channel", "default", "--woa-token-path=/cli/token.json"],
        env: {
          CLAUDE_WOA_CHANNEL: "offline",
          CLAUDE_WOA_TOKEN_PATH: "/env/token.json",
        },
        homeDir: "/home/test",
      }),
    ).toEqual({
      enabled: true,
      channel: "default",
      tokenPath: "/cli/token.json",
    });
  });

  it("rejects invalid channels", () => {
    expect(() => parseWoaChannel("staging")).toThrow(WoaError);
    expect(() => parseWoaConfig({ argv: ["--woa-channel", "staging"], env: {} })).toThrow(
      /Invalid WOA channel/,
    );
  });

  it("detects WOA process commands", () => {
    expect(parseWoaCli(["--woa-login"], {}).command).toBe("login");
    expect(parseWoaCli(["--woa-status"], {}).command).toBe("status");
    expect(parseWoaCli(["--woa-refresh"], {}).command).toBe("refresh");
    expect(parseWoaCli(["--woa"], {}).command).toBeUndefined();
  });
});
