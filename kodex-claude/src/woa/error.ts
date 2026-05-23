export type WoaErrorCode =
  | "invalid_channel"
  | "missing_token"
  | "malformed_token"
  | "missing_refresh_token"
  | "device_code_failed"
  | "token_poll_failed"
  | "refresh_failed"
  | "token_file_error";

export class WoaError extends Error {
  readonly code: WoaErrorCode;
  readonly cause?: unknown;

  constructor(code: WoaErrorCode, message: string, options?: { cause?: unknown }) {
    super(message);
    this.name = "WoaError";
    this.code = code;
    this.cause = options?.cause;
  }
}

export function toWoaError(error: unknown, fallbackCode: WoaErrorCode, fallbackMessage: string) {
  if (error instanceof WoaError) {
    return error;
  }
  if (error instanceof Error) {
    return new WoaError(fallbackCode, `${fallbackMessage}: ${error.message}`, { cause: error });
  }
  return new WoaError(fallbackCode, fallbackMessage, { cause: error });
}

export function loginInstruction(command = "claude-agent-acp --woa-login") {
  return `Run \`${command}\` to complete WOA login.`;
}
