import { spawn } from "node:child_process";

const CLAUDE_CLI_PREFLIGHT_TIMEOUT_MS = 4000;
const CLAUDE_CLI_DIAGNOSTIC_TIMEOUT_MS = 2500;
const CLAUDE_CLI_OUTPUT_LIMIT = 12_000;

type CapturedProcess = {
  code: number | null;
  signal: NodeJS.Signals | null;
  stdout: string;
  stderr: string;
  error?: Error;
  timedOut: boolean;
};

let verifiedClaudeCliPathCache:
  | {
      executable: string;
      promise: Promise<string>;
    }
  | undefined;

export async function claudeCliPath(): Promise<string> {
  if (process.env.CLAUDE_CODE_EXECUTABLE) {
    return process.env.CLAUDE_CODE_EXECUTABLE;
  }
  // The SDK's CLI is a native binary shipped as a platform-specific optional
  // dependency of @anthropic-ai/claude-agent-sdk. Resolve via a require bound
  // to the SDK so nested installs are found even when npm doesn't hoist.
  const { createRequire } = await import("node:module");
  const req = createRequire(import.meta.resolve("@anthropic-ai/claude-agent-sdk"));
  const ext = process.platform === "win32" ? ".exe" : "";
  // On linux, both glibc and musl variants may be installed side-by-side
  // (e.g. bunx hydrates every optional dep), so picking one by trial is
  // unreliable: the wrong binary segfaults at runtime instead of failing to
  // spawn. Detect the runtime libc and prefer the matching variant, falling
  // back to the other only if the preferred one isn't installed.
  const candidates =
    process.platform === "linux"
      ? isMuslLibc()
        ? [
            `@anthropic-ai/claude-agent-sdk-linux-${process.arch}-musl/claude${ext}`,
            `@anthropic-ai/claude-agent-sdk-linux-${process.arch}/claude${ext}`,
          ]
        : [
            `@anthropic-ai/claude-agent-sdk-linux-${process.arch}/claude${ext}`,
            `@anthropic-ai/claude-agent-sdk-linux-${process.arch}-musl/claude${ext}`,
          ]
      : [`@anthropic-ai/claude-agent-sdk-${process.platform}-${process.arch}/claude${ext}`];
  for (const candidate of candidates) {
    try {
      return req.resolve(candidate);
    } catch {
      // try next candidate
    }
  }
  throw new Error(
    `Claude native binary not found for ${process.platform}-${process.arch}. ` +
      `Reinstall @anthropic-ai/claude-agent-sdk without --omit=optional, or set CLAUDE_CODE_EXECUTABLE.`,
  );
}

export async function verifiedClaudeCliPath(): Promise<string> {
  const executable = await claudeCliPath();
  if (verifiedClaudeCliPathCache?.executable === executable) {
    return verifiedClaudeCliPathCache.promise;
  }

  const promise = assertClaudeCliLaunchable(executable)
    .then(() => executable)
    .catch((error) => {
      if (verifiedClaudeCliPathCache?.executable === executable) {
        verifiedClaudeCliPathCache = undefined;
      }
      throw error;
    });
  verifiedClaudeCliPathCache = { executable, promise };
  return promise;
}

export async function enrichClaudeCliLaunchError(
  error: unknown,
  executable: string,
): Promise<unknown> {
  if (!isClaudeCliLaunchError(error)) {
    return error;
  }

  const result = await captureProcess(executable, ["--version"], CLAUDE_CLI_PREFLIGHT_TIMEOUT_MS);
  const message = await claudeCliLaunchFailureMessage(executable, result);
  const original = error instanceof Error ? error.message : String(error);
  return new Error(`${original}\n\n${message}`);
}

async function assertClaudeCliLaunchable(executable: string): Promise<void> {
  const result = await captureProcess(executable, ["--version"], CLAUDE_CLI_PREFLIGHT_TIMEOUT_MS);
  if (!result.error && !result.timedOut && result.code === 0) {
    return;
  }

  throw new Error(await claudeCliLaunchFailureMessage(executable, result));
}

function isClaudeCliLaunchError(error: unknown): boolean {
  const message = error instanceof Error ? error.message : String(error);
  return (
    message.includes("Claude Code native binary") ||
    message.includes("pathToClaudeCodeExecutable") ||
    message.includes("failed to launch")
  );
}

async function claudeCliLaunchFailureMessage(
  executable: string,
  launchResult: CapturedProcess,
): Promise<string> {
  const diagnostics = await claudeCliDiagnostics(executable);
  return [
    `Claude Code native binary failed to launch: ${executable}`,
    `Runtime: ${runtimeDescription()}`,
    "",
    formatCapturedProcess(`${executable} --version`, launchResult),
    ...diagnostics,
    "",
    "Common causes: wrong platform/architecture binary, glibc vs musl mismatch, missing dynamic loader/library, corrupted extracted binary, or a noexec /tmp mount.",
  ]
    .filter(Boolean)
    .join("\n");
}

async function claudeCliDiagnostics(executable: string): Promise<string[]> {
  const checks: Array<{ label: string; command: string; args: string[] }> = [];

  if (process.platform !== "win32") {
    checks.push(
      { label: `ls -l ${executable}`, command: "ls", args: ["-l", executable] },
      { label: `file ${executable}`, command: "file", args: [executable] },
    );
  }

  if (process.platform === "linux") {
    checks.push(
      { label: `ldd ${executable}`, command: "ldd", args: [executable] },
      { label: "uname -m", command: "uname", args: ["-m"] },
      { label: "getconf GNU_LIBC_VERSION", command: "getconf", args: ["GNU_LIBC_VERSION"] },
      {
        label: "/tmp mount options",
        command: "sh",
        args: ["-lc", "findmnt -no OPTIONS /tmp 2>/dev/null || mount | grep ' /tmp ' || true"],
      },
    );
  }

  const results: string[] = [];
  for (const check of checks) {
    const result = await captureProcess(
      check.command,
      check.args,
      CLAUDE_CLI_DIAGNOSTIC_TIMEOUT_MS,
    );
    results.push(formatCapturedProcess(check.label, result));
  }
  return results;
}

function runtimeDescription(): string {
  const report = process.report?.getReport() as
    | { header?: { glibcVersionRuntime?: string } }
    | undefined;
  const libc = report?.header?.glibcVersionRuntime
    ? `glibc ${report.header.glibcVersionRuntime}`
    : "musl/unknown libc";
  return `${process.platform}-${process.arch}, ${libc}`;
}

function captureProcess(
  command: string,
  args: string[],
  timeoutMs: number,
): Promise<CapturedProcess> {
  return new Promise((resolve) => {
    let child;
    try {
      child = spawn(command, args, {
        env: process.env,
        stdio: ["ignore", "pipe", "pipe"],
      });
    } catch (error) {
      resolve({
        code: null,
        signal: null,
        stdout: "",
        stderr: "",
        error: error instanceof Error ? error : new Error(String(error)),
        timedOut: false,
      });
      return;
    }

    let stdout = "";
    let stderr = "";
    let timedOut = false;
    let settled = false;

    const finish = (result: Omit<CapturedProcess, "stdout" | "stderr" | "timedOut">) => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimeout(timer);
      resolve({
        ...result,
        stdout,
        stderr,
        timedOut,
      });
    };

    const timer = setTimeout(() => {
      timedOut = true;
      child.kill("SIGKILL");
    }, timeoutMs);

    child.stdout?.on("data", (chunk: Buffer) => {
      stdout = appendCapturedOutput(stdout, chunk);
    });
    child.stderr?.on("data", (chunk: Buffer) => {
      stderr = appendCapturedOutput(stderr, chunk);
    });
    child.on("error", (error) => {
      finish({
        code: null,
        signal: null,
        error,
      });
    });
    child.on("close", (code, signal) => {
      finish({
        code,
        signal,
      });
    });
  });
}

function appendCapturedOutput(current: string, chunk: Buffer): string {
  const next = current + chunk.toString("utf8");
  if (next.length <= CLAUDE_CLI_OUTPUT_LIMIT) {
    return next;
  }
  return `[truncated]\n${next.slice(-CLAUDE_CLI_OUTPUT_LIMIT)}`;
}

function formatCapturedProcess(label: string, result: CapturedProcess): string {
  const lines = [`${label}:`];
  if (result.error) {
    lines.push(`  spawn error: ${result.error.message}`);
  } else if (result.timedOut) {
    lines.push(`  timed out`);
  } else {
    lines.push(
      `  exit: ${result.code ?? "null"}${result.signal ? ` signal: ${result.signal}` : ""}`,
    );
  }
  if (result.stdout.trim()) {
    lines.push(indentBlock("stdout", result.stdout.trim()));
  }
  if (result.stderr.trim()) {
    lines.push(indentBlock("stderr", result.stderr.trim()));
  }
  return lines.join("\n");
}

function indentBlock(label: string, content: string): string {
  return [`  ${label}:`, ...content.split("\n").map((line) => `    ${line}`)].join("\n");
}

function isMuslLibc(): boolean {
  // process.report.getReport().header.glibcVersionRuntime is populated when
  // Node is dynamically linked against glibc, and absent on musl.
  const report = process.report?.getReport() as
    | { header?: { glibcVersionRuntime?: string } }
    | undefined;
  return !report?.header?.glibcVersionRuntime;
}
