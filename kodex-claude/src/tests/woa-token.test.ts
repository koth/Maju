import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  isWoaTokenExpiringSoon,
  loadWoaToken,
  maskSecret,
  saveWoaToken,
  validateWoaToken,
} from "../woa/token.js";
import { WoaError } from "../woa/error.js";

describe("WOA token storage", () => {
  let tempDir: string;

  beforeEach(async () => {
    tempDir = await fs.mkdtemp(path.join(os.tmpdir(), "woa-token-"));
  });

  afterEach(async () => {
    await fs.rm(tempDir, { recursive: true, force: true });
  });

  it("validates the claude-woa token shape", () => {
    expect(
      validateWoaToken({
        accessToken: "access",
        refreshToken: "refresh",
        expiresAt: 123,
      }),
    ).toEqual({ accessToken: "access", refreshToken: "refresh", expiresAt: 123 });
  });

  it("rejects malformed tokens", () => {
    expect(() => validateWoaToken({ accessToken: "", expiresAt: 123 })).toThrow(WoaError);
    expect(() => validateWoaToken({ accessToken: "access", expiresAt: "soon" })).toThrow(WoaError);
  });

  it("returns null for a missing token file", async () => {
    await expect(loadWoaToken(path.join(tempDir, "missing.json"))).resolves.toBeNull();
  });

  it("throws on malformed JSON", async () => {
    const tokenPath = path.join(tempDir, "bad.json");
    await fs.writeFile(tokenPath, "{");
    await expect(loadWoaToken(tokenPath)).rejects.toThrow(/not valid JSON/);
  });

  it("roundtrips token files and creates parent directories", async () => {
    const tokenPath = path.join(tempDir, "nested", "token.json");
    const token = { accessToken: "access", refreshToken: "refresh", expiresAt: Date.now() + 1000 };
    await saveWoaToken(tokenPath, token);

    await expect(loadWoaToken(tokenPath)).resolves.toEqual(token);
    if (process.platform !== "win32") {
      const stat = await fs.stat(tokenPath);
      expect(stat.mode & 0o777).toBe(0o600);
    }
  });

  it("detects tokens expiring within threshold", () => {
    const now = 1000;
    expect(isWoaTokenExpiringSoon({ accessToken: "a", expiresAt: now + 100 }, now, 500)).toBe(true);
    expect(isWoaTokenExpiringSoon({ accessToken: "a", expiresAt: now + 1000 }, now, 500)).toBe(
      false,
    );
  });

  it("masks secrets without returning the full value", () => {
    expect(maskSecret(undefined)).toBe("(none)");
    expect(maskSecret("abcdefghijklmnop")).toBe("abcd...mnop");
    expect(maskSecret("abcd")).toBe("ab...cd");
  });
});
