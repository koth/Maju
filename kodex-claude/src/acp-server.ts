import { runAcp } from "./acp-runner.js";
import { nodeToWebReadable, nodeToWebWritable } from "./utils.js";
import type { WoaConfig } from "./woa/index.js";

export function parsePortArgs(argv: string[]) {
  let port: number | undefined;
  const args: string[] = [];
  for (let index = 0; index < argv.length; index++) {
    const arg = argv[index];
    if (arg === "--port") {
      const value = argv[index + 1];
      if (!value) {
        throw new Error("--port requires a value");
      }
      port = parsePort(value);
      index++;
      continue;
    }
    if (arg.startsWith("--port=")) {
      port = parsePort(arg.slice("--port=".length));
      continue;
    }
    args.push(arg);
  }
  return { args, port };
}

function parsePort(value: string) {
  const port = Number(value);
  if (!Number.isInteger(port) || port <= 0 || port > 65535) {
    throw new Error(`Invalid --port value: ${value}`);
  }
  return port;
}

export async function listenTcpAcp(port: number, woa?: WoaConfig) {
  const { createServer } = await import("node:net");
  let active = false;
  let agent: ReturnType<typeof runAcp>["agent"] | undefined;
  let closedResolve!: () => void;
  const closed = new Promise<void>((resolve) => {
    closedResolve = resolve;
  });

  const server = createServer((socket) => {
    if (active) {
      socket.destroy(new Error("Claude Agent ACP already has an active TCP connection"));
      return;
    }
    active = true;
    const connection = runAcp({
      woa,
      input: nodeToWebWritable(socket),
      output: nodeToWebReadable(socket),
    });
    agent = connection.agent;
    connection.connection.closed.finally(closedResolve);
  });

  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, "127.0.0.1", () => {
      server.off("error", reject);
      resolve();
    });
  });

  return {
    server,
    get agent() {
      return agent;
    },
    closed,
  };
}
