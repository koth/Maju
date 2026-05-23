import { sleep } from "../utils.js";
import { WoaConfig } from "./config.js";
import { loginInstruction, WoaError } from "./error.js";
import {
  isWoaTokenExpiringSoon,
  loadWoaToken,
  maskSecret,
  saveWoaToken,
  validateWoaToken,
  WoaToken,
} from "./token.js";

export const WOA_SERVER = "https://copilot.code.woa.com";
export const WOA_CLIENT_ID = "d15f1aada3db4be2be622afed0019a29";
export const WOA_DEVICE_CODE_URL = `${WOA_SERVER}/api/v2/auth/device/code`;
export const WOA_DEVICE_TOKEN_URL = `${WOA_SERVER}/api/v2/auth/device/token`;
export const WOA_REFRESH_URL = `${WOA_SERVER}/api/v2/auth/oauth_token/refresh`;

export type FetchLike = typeof fetch;

export type WoaDeviceCode = {
  deviceCode: string;
  userCode: string;
  verificationUri: string;
  verificationUriComplete?: string;
  expiresAt: number;
  intervalMs: number;
};

export type WoaIo = {
  log?: (message: string) => void;
  write?: (message: string) => void;
};

async function responseText(response: Response) {
  try {
    return await response.text();
  } catch {
    return "";
  }
}

async function responseJson(response: Response): Promise<unknown> {
  try {
    return await response.json();
  } catch (error) {
    throw new WoaError("token_poll_failed", "WOA response was not valid JSON.", { cause: error });
  }
}

function requireString(value: unknown, name: string) {
  if (typeof value !== "string" || value.length === 0) {
    throw new WoaError("token_poll_failed", `WOA response missing ${name}.`);
  }
  return value;
}

function tokenFromOAuthResponse(value: unknown, previousRefreshToken?: string): WoaToken {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new WoaError("token_poll_failed", "WOA token response was not an object.");
  }
  const data = value as Record<string, unknown>;
  const expiresIn = typeof data.expires_in === "number" ? data.expires_in : undefined;
  if (!expiresIn || expiresIn <= 0) {
    throw new WoaError("token_poll_failed", "WOA token response missing expires_in.");
  }
  return validateWoaToken({
    accessToken: requireString(data.access_token, "access_token"),
    refreshToken:
      typeof data.refresh_token === "string" && data.refresh_token.length > 0
        ? data.refresh_token
        : previousRefreshToken,
    expiresAt: Date.now() + expiresIn * 1000,
  });
}

export async function requestDeviceCode(fetchImpl: FetchLike = fetch): Promise<WoaDeviceCode> {
  const response = await fetchImpl(WOA_DEVICE_CODE_URL, {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({ client_id: WOA_CLIENT_ID }),
  });
  if (!response.ok) {
    throw new WoaError(
      "device_code_failed",
      `WOA device code request failed: ${response.status} ${await responseText(response)}`,
    );
  }

  const data = (await responseJson(response)) as Record<string, unknown>;
  let verificationUri = requireString(data.verification_uri, "verification_uri");
  if (!verificationUri.startsWith("http")) {
    verificationUri = new URL(verificationUri, WOA_DEVICE_CODE_URL).toString();
  }
  return {
    deviceCode: requireString(data.device_code, "device_code"),
    userCode: requireString(data.user_code, "user_code"),
    verificationUri,
    verificationUriComplete:
      typeof data.verification_uri_complete === "string"
        ? data.verification_uri_complete
        : undefined,
    expiresAt: Date.now() + (typeof data.expires_in === "number" ? data.expires_in : 600) * 1000,
    intervalMs: (typeof data.interval === "number" ? data.interval : 5) * 1000,
  };
}

async function tryGetDeviceToken(deviceCode: string, fetchImpl: FetchLike) {
  const response = await fetchImpl(WOA_DEVICE_TOKEN_URL, {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "urn:ietf:params:oauth:grant-type:device_code",
      device_code: deviceCode,
      client_id: WOA_CLIENT_ID,
    }),
  });
  if (!response.ok) {
    let body: Record<string, unknown> = {};
    try {
      const parsed = await response.json();
      if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
        body = parsed as Record<string, unknown>;
      }
    } catch {
      // Match claude-woa: non-JSON error bodies fall back to status_<code>.
    }
    return {
      ok: false as const,
      error:
        typeof body.error === "string"
          ? body.error
          : typeof body.error_description === "string"
            ? body.error_description
            : `status_${response.status}`,
    };
  }
  const data = await responseJson(response);
  return { ok: true as const, token: tokenFromOAuthResponse(data) };
}

export async function pollForToken(
  device: WoaDeviceCode,
  fetchImpl: FetchLike = fetch,
  sleepImpl: (time: number) => Promise<void> = sleep,
  io: WoaIo = {},
): Promise<WoaToken> {
  let intervalMs = device.intervalMs;
  while (Date.now() < device.expiresAt) {
    await sleepImpl(intervalMs);
    const result = await tryGetDeviceToken(device.deviceCode, fetchImpl);
    if (result.ok) {
      return result.token;
    }
    if (result.error === "authorization_pending") {
      io.write?.(".");
      continue;
    }
    if (result.error === "slow_down") {
      intervalMs = Math.min(intervalMs + 2000, 15000);
      io.log?.(
        `WOA authorization server requested slower polling; interval is now ${intervalMs / 1000}s.`,
      );
      continue;
    }
    throw new WoaError("token_poll_failed", `WOA token exchange failed: ${result.error}`);
  }
  throw new WoaError("token_poll_failed", `WOA device code expired. ${loginInstruction()}`);
}

export async function refreshWoaToken(
  token: WoaToken,
  fetchImpl: FetchLike = fetch,
): Promise<WoaToken> {
  if (!token.refreshToken) {
    throw new WoaError(
      "missing_refresh_token",
      `WOA token has no refresh token. ${loginInstruction()}`,
    );
  }
  const response = await fetchImpl(WOA_REFRESH_URL, {
    method: "POST",
    headers: {
      "Content-Type": "application/x-www-form-urlencoded",
      "OAUTH-TOKEN": token.accessToken,
    },
    body: new URLSearchParams({
      refresh_token: token.refreshToken,
      client_id: WOA_CLIENT_ID,
      grant_type: "refresh_token",
    }),
  });
  if (!response.ok) {
    throw new WoaError(
      "refresh_failed",
      `WOA token refresh failed: ${response.status} ${await responseText(response)}. ${loginInstruction()}`,
    );
  }
  try {
    return tokenFromOAuthResponse(await responseJson(response), token.refreshToken);
  } catch (error) {
    if (error instanceof WoaError) {
      throw new WoaError("refresh_failed", error.message, { cause: error });
    }
    throw error;
  }
}

export async function loginWoa(
  config: WoaConfig,
  options: { fetchImpl?: FetchLike; sleepImpl?: (time: number) => Promise<void>; io?: WoaIo } = {},
) {
  const device = await requestDeviceCode(options.fetchImpl);
  options.io?.log?.(`Visit: ${device.verificationUri}`);
  options.io?.log?.(`Code: ${device.userCode}`);
  if (device.verificationUriComplete) {
    options.io?.log?.(`Or: ${device.verificationUriComplete}`);
  }
  const token = await pollForToken(device, options.fetchImpl, options.sleepImpl, options.io);
  await saveWoaToken(config.tokenPath, token);
  return token;
}

export async function refreshAndSaveWoaToken(
  config: WoaConfig,
  options: { fetchImpl?: FetchLike } = {},
) {
  const token = await loadWoaToken(config.tokenPath);
  if (!token) {
    throw new WoaError(
      "missing_token",
      `No WOA token found at ${config.tokenPath}. ${loginInstruction()}`,
    );
  }
  const fresh = await refreshWoaToken(token, options.fetchImpl);
  await saveWoaToken(config.tokenPath, fresh);
  return fresh;
}

export async function ensureWoaToken(
  config: WoaConfig,
  options: { fetchImpl?: FetchLike; now?: number } = {},
) {
  const token = await loadWoaToken(config.tokenPath);
  if (!token) {
    throw new WoaError(
      "missing_token",
      `No WOA token found at ${config.tokenPath}. ${loginInstruction()}`,
    );
  }
  if (!isWoaTokenExpiringSoon(token, options.now)) {
    return token;
  }
  const fresh = await refreshWoaToken(token, options.fetchImpl);
  await saveWoaToken(config.tokenPath, fresh);
  return fresh;
}

export async function getWoaTokenStatus(config: WoaConfig, now = Date.now()) {
  const token = await loadWoaToken(config.tokenPath);
  if (!token) {
    return {
      exists: false as const,
      tokenPath: config.tokenPath,
      channel: config.channel,
    };
  }
  return {
    exists: true as const,
    tokenPath: config.tokenPath,
    channel: config.channel,
    accessToken: maskSecret(token.accessToken),
    refreshToken: maskSecret(token.refreshToken),
    expiresAt: new Date(token.expiresAt).toISOString(),
    validForMinutes: Math.floor((token.expiresAt - now) / 60000),
    refreshNeeded: isWoaTokenExpiringSoon(token, now),
  };
}
