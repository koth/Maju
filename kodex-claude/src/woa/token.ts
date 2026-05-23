import * as fs from "node:fs/promises";
import * as path from "node:path";
import { randomUUID } from "node:crypto";
import { WoaError } from "./error.js";

export const WOA_TOKEN_REFRESH_THRESHOLD_MS = 5 * 60 * 1000;

export type WoaToken = {
  accessToken: string;
  refreshToken?: string;
  expiresAt: number;
};

export function validateWoaToken(value: unknown, tokenPath = "WOA token file"): WoaToken {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new WoaError("malformed_token", `${tokenPath} does not contain a WOA token object.`);
  }
  const token = value as Record<string, unknown>;
  if (typeof token.accessToken !== "string" || token.accessToken.length === 0) {
    throw new WoaError("malformed_token", `${tokenPath} is missing accessToken.`);
  }
  if (token.refreshToken !== undefined && typeof token.refreshToken !== "string") {
    throw new WoaError("malformed_token", `${tokenPath} has an invalid refreshToken.`);
  }
  if (typeof token.expiresAt !== "number" || !Number.isFinite(token.expiresAt)) {
    throw new WoaError("malformed_token", `${tokenPath} is missing numeric expiresAt.`);
  }

  return {
    accessToken: token.accessToken,
    refreshToken: token.refreshToken,
    expiresAt: token.expiresAt,
  };
}

export async function loadWoaToken(tokenPath: string): Promise<WoaToken | null> {
  let raw: string;
  try {
    raw = await fs.readFile(tokenPath, "utf8");
  } catch (error: any) {
    if (error?.code === "ENOENT") {
      return null;
    }
    throw new WoaError("token_file_error", `Failed to read WOA token file ${tokenPath}.`, {
      cause: error,
    });
  }

  try {
    return validateWoaToken(JSON.parse(raw), tokenPath);
  } catch (error) {
    if (error instanceof SyntaxError) {
      throw new WoaError("malformed_token", `${tokenPath} is not valid JSON.`, { cause: error });
    }
    throw error;
  }
}

export async function saveWoaToken(tokenPath: string, token: WoaToken): Promise<void> {
  const validToken = validateWoaToken(token, "WOA token");
  const dir = path.dirname(tokenPath);
  const tempPath = path.join(dir, `.${path.basename(tokenPath)}.${randomUUID()}.tmp`);

  try {
    await fs.mkdir(dir, { recursive: true });
    await fs.writeFile(tempPath, `${JSON.stringify(validToken, null, 2)}\n`, { mode: 0o600 });
    if (process.platform !== "win32") {
      await fs.chmod(tempPath, 0o600);
    }
    await fs.rename(tempPath, tokenPath);
  } catch (error) {
    try {
      await fs.rm(tempPath, { force: true });
    } catch {
      // best effort cleanup
    }
    throw new WoaError("token_file_error", `Failed to save WOA token file ${tokenPath}.`, {
      cause: error,
    });
  }
}

export function isWoaTokenExpiringSoon(
  token: WoaToken,
  now = Date.now(),
  thresholdMs = WOA_TOKEN_REFRESH_THRESHOLD_MS,
) {
  return token.expiresAt <= now + thresholdMs;
}

export function maskSecret(secret: string | undefined) {
  if (!secret) {
    return "(none)";
  }
  if (secret.length <= 8) {
    return `${secret.slice(0, 2)}...${secret.slice(-2)}`;
  }
  return `${secret.slice(0, 4)}...${secret.slice(-4)}`;
}
