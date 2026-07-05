/** Tiny leveled logger writing to stdout/stderr + a log file. */
type Level = 'debug' | 'info' | 'warn' | 'error' | 'silent';
const ORDER: Record<Level, number> = { debug: 10, info: 20, warn: 30, error: 40, silent: 100 };

let current: Level = 'info';
let logFile: import('fs').WriteStream | null = null;

function getLogFile(): import('fs').WriteStream | null {
  if (logFile) return logFile;
  const dir = process.env.CODEBUDDY_PROXY_LOG_DIR
    || (process.env.HOME || process.env.USERPROFILE
      ? require('path').join(process.env.HOME || process.env.USERPROFILE!, '.kodex', 'logs')
      : null);
  if (!dir) return null;
  const fs = require('fs') as typeof import('fs');
  try {
    fs.mkdirSync(dir, { recursive: true });
    logFile = fs.createWriteStream(
      require('path').join(dir, 'codebuddy-proxy.log'),
      { flags: 'a' },
    );
  } catch { /* ignore */ }
  return logFile;
}

function writeLine(level: string, msg: string): void {
  const f = getLogFile();
  if (f) f.write(`[${ts()}] ${level} ${msg}\n`);
}

export function setLevel(level: string): void {
  if (level in ORDER) current = level as Level;
}

function ts(): string {
  return new Date().toISOString();
}

export const logger = {
  debug(fmt: string, ...args: unknown[]): void {
    if (ORDER[current] <= ORDER.debug) {
      const msg = format(fmt, args);
      console.debug(`[${ts()}] DEBUG ${msg}`);
      writeLine('DEBUG', msg);
    }
  },
  info(fmt: string, ...args: unknown[]): void {
    if (ORDER[current] <= ORDER.info) {
      const msg = format(fmt, args);
      console.log(`[${ts()}] INFO  ${msg}`);
      writeLine('INFO ', msg);
    }
  },
  warn(fmt: string, ...args: unknown[]): void {
    if (ORDER[current] <= ORDER.warn) {
      const msg = format(fmt, args);
      console.warn(`[${ts()}] WARN ${msg}`);
      writeLine('WARN ', msg);
    }
  },
  error(fmt: string, ...args: unknown[]): void {
    if (ORDER[current] <= ORDER.error) {
      const msg = format(fmt, args);
      console.error(`[${ts()}] ERROR ${msg}`);
      writeLine('ERROR', msg);
    }
  },
};

function format(fmt: string, args: unknown[]): string {
  if (args.length === 0) return fmt;
  // Minimal %s/%d/%j substitution.
  let i = 0;
  return fmt.replace(/%([sdj])/g, (_m, spec: string) => {
    const a = args[i++];
    switch (spec) {
      case 's': return String(a);
      case 'd': return String(Number(a));
      case 'j': return JSON.stringify(a);
      default: return String(a);
    }
  });
}
