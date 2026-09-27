import { describe, expect, it } from "vitest";
import { computeSessionFingerprint } from "../acp-agent.js";
import type { NewSessionRequest } from "@agentclient/protocol";

function browserServer(): NonNullable<NewSessionRequest["mcpServers"]>[number] {
  return {
    type: "http",
    name: "kodex-browser",
    url: "http://127.0.0.1:34567/mcp",
    headers: [{ name: "x-kodex-browser-token", value: "browser-1" }],
  };
}

function webToolsServer(): NonNullable<NewSessionRequest["mcpServers"]>[number] {
  return {
    type: "http",
    name: "kodex-web-tools",
    url: "http://127.0.0.1:34568/mcp",
    headers: [],
  };
}

describe("browser MCP server passthrough", () => {
  it("reaches the session fingerprint, so injecting it changes session identity", () => {
    // The Claude adapter passes `mcpServers` through untouched. The only
    // place it matters is the fingerprint, which decides whether a resume has
    // to tear down and recreate the Query process.
    const without = computeSessionFingerprint({ cwd: "/tmp/project" });
    const withBrowser = computeSessionFingerprint({
      cwd: "/tmp/project",
      mcpServers: [browserServer()],
    });

    expect(withBrowser).not.toBe(without);
  });

  it("does not recreate for a reordering of the same servers", () => {
    const forward = computeSessionFingerprint({
      cwd: "/tmp/project",
      mcpServers: [webToolsServer(), browserServer()],
    });
    const reversed = computeSessionFingerprint({
      cwd: "/tmp/project",
      mcpServers: [browserServer(), webToolsServer()],
    });

    // A resumed session must not be torn down just because the injected
    // servers came back in a different order.
    expect(reversed).toBe(forward);
  });

  it("keeps the per-session browser token in the identity", () => {
    // Two sessions share one server URL but not one token, so they must not
    // be treated as the same session.
    const first = computeSessionFingerprint({
      cwd: "/tmp/project",
      mcpServers: [browserServer()],
    });
    const second = computeSessionFingerprint({
      cwd: "/tmp/project",
      mcpServers: [
        {
          ...browserServer(),
          headers: [{ name: "x-kodex-browser-token", value: "browser-2" }],
        },
      ],
    });

    expect(second).not.toBe(first);
  });

  it("carries the browser server alongside the existing Kodex servers", () => {
    const combined = computeSessionFingerprint({
      cwd: "/tmp/project",
      mcpServers: [webToolsServer(), browserServer()],
    });

    expect(JSON.parse(combined).mcpServers).toHaveLength(2);
    expect(combined).toContain("kodex-browser");
    expect(combined).toContain("kodex-web-tools");
  });
});
