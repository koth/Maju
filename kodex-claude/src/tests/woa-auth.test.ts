import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  ensureWoaToken,
  pollForToken,
  refreshWoaToken,
  requestDeviceCode,
  WOA_DEVICE_CODE_URL,
  WOA_DEVICE_TOKEN_URL,
  WOA_REFRESH_URL,
} from "../woa/auth.js";
import { WoaConfig } from "../woa/config.js";
import { saveWoaToken } from "../woa/token.js";

function jsonResponse(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

describe("WOA auth", () => {
  let tempDir: string;
  let config: WoaConfig;

  beforeEach(async () => {
    tempDir = await fs.mkdtemp(path.join(os.tmpdir(), "woa-auth-"));
    config = { enabled: true, channel: "default", tokenPath: path.join(tempDir, "token.json") };
  });

  afterEach(async () => {
    await fs.rm(tempDir, { recursive: true, force: true });
  });

  it("requests a device code", async () => {
    const fetchImpl = vi.fn(async () =>
      jsonResponse({
        device_code: "device",
        user_code: "user",
        verification_uri: "/oauth/device",
        verification_uri_complete: "https://complete",
        expires_in: 600,
        interval: 5,
      }),
    ) as any;

    const device = await requestDeviceCode(fetchImpl);

    expect(fetchImpl).toHaveBeenCalledWith(
      WOA_DEVICE_CODE_URL,
      expect.objectContaining({ method: "POST" }),
    );
    expect(device).toEqual(
      expect.objectContaining({
        deviceCode: "device",
        userCode: "user",
        verificationUri: "https://copilot.code.woa.com/oauth/device",
        verificationUriComplete: "https://complete",
        intervalMs: 5000,
      }),
    );
  });

  it("polls through authorization_pending and slow_down before success", async () => {
    const fetchImpl = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse({ error: "authorization_pending" }, 400))
      .mockResolvedValueOnce(jsonResponse({ error: "slow_down" }, 400))
      .mockResolvedValueOnce(
        jsonResponse({ access_token: "access", refresh_token: "refresh", expires_in: 3600 }),
      ) as any;
    const sleeps: number[] = [];

    const token = await pollForToken(
      {
        deviceCode: "device",
        userCode: "user",
        verificationUri: "https://verify",
        expiresAt: Date.now() + 60000,
        intervalMs: 5000,
      },
      fetchImpl,
      async (time) => {
        sleeps.push(time);
      },
    );

    expect(fetchImpl).toHaveBeenCalledTimes(3);
    expect(fetchImpl).toHaveBeenCalledWith(
      WOA_DEVICE_TOKEN_URL,
      expect.objectContaining({ method: "POST" }),
    );
    expect(sleeps).toEqual([5000, 5000, 7000]);
    expect(token).toEqual(
      expect.objectContaining({ accessToken: "access", refreshToken: "refresh" }),
    );
  });

  it("falls back to status code when device token error is not JSON", async () => {
    const fetchImpl = vi.fn(async () => new Response("bad gateway", { status: 502 })) as any;

    await expect(
      pollForToken(
        {
          deviceCode: "device",
          userCode: "user",
          verificationUri: "https://verify",
          expiresAt: Date.now() + 60000,
          intervalMs: 5000,
        },
        fetchImpl,
        async () => {},
      ),
    ).rejects.toThrow(/status_502/);
  });

  it("refreshes a token and preserves an omitted refresh token", async () => {
    const fetchImpl = vi.fn(async () =>
      jsonResponse({ access_token: "fresh", expires_in: 3600 }),
    ) as any;

    const token = await refreshWoaToken(
      { accessToken: "old", refreshToken: "refresh", expiresAt: 0 },
      fetchImpl,
    );

    expect(fetchImpl).toHaveBeenCalledWith(
      WOA_REFRESH_URL,
      expect.objectContaining({ method: "POST" }),
    );
    expect(token).toEqual(
      expect.objectContaining({ accessToken: "fresh", refreshToken: "refresh" }),
    );
  });

  it("ensures a valid token without refresh", async () => {
    const token = {
      accessToken: "access",
      refreshToken: "refresh",
      expiresAt: Date.now() + 3600000,
    };
    await saveWoaToken(config.tokenPath, token);
    const fetchImpl = vi.fn() as any;

    await expect(ensureWoaToken(config, { fetchImpl })).resolves.toEqual(token);
    expect(fetchImpl).not.toHaveBeenCalled();
  });

  it("refreshes a token during ensure when expiring soon", async () => {
    await saveWoaToken(config.tokenPath, {
      accessToken: "old",
      refreshToken: "refresh",
      expiresAt: Date.now(),
    });
    const fetchImpl = vi.fn(async () =>
      jsonResponse({ access_token: "fresh", refresh_token: "new-refresh", expires_in: 3600 }),
    ) as any;

    const token = await ensureWoaToken(config, { fetchImpl });

    expect(token).toEqual(
      expect.objectContaining({ accessToken: "fresh", refreshToken: "new-refresh" }),
    );
  });

  it("fails ensure when token is missing", async () => {
    await expect(ensureWoaToken(config)).rejects.toThrow(/No WOA token/);
  });
});
