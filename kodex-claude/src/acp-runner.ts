import { AgentSideConnection, ndJsonStream } from "@agentclientprotocol/sdk";
import { ClaudeAcpAgent } from "./acp-agent.js";
import { nodeToWebReadable, nodeToWebWritable } from "./utils.js";
import type { WoaConfig } from "./woa/index.js";

export type RunAcpOptions = {
  woa?: WoaConfig;
  input?: WritableStream<Uint8Array>;
  output?: ReadableStream<Uint8Array>;
};

export function runAcp(options?: RunAcpOptions) {
  const input = options?.input ?? nodeToWebWritable(process.stdout);
  const output = options?.output ?? nodeToWebReadable(process.stdin);

  const stream = ndJsonStream(input, output);
  let agent!: ClaudeAcpAgent;
  const connection = new AgentSideConnection((client) => {
    agent = new ClaudeAcpAgent(client, { woa: options?.woa });
    return agent;
  }, stream);
  return { connection, agent };
}
