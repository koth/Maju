// Export the main agent class and utilities for library usage
export {
  ClaudeAcpAgent,
  isLocalCommandMetadata,
  stripLocalCommandMetadata,
  toAcpNotifications,
  streamEventToAcpNotifications,
  type ToolUpdateMeta,
  type NewSessionMeta,
  type SDKMessageFilter,
} from "./acp-agent.js";
export { runAcp, type RunAcpOptions } from "./acp-runner.js";
export { nodeToWebReadable, nodeToWebWritable, Pushable, unreachable } from "./utils.js";
export {
  toolInfoFromToolUse,
  toDisplayPath,
  planEntries,
  toolUpdateFromToolResult,
} from "./tools.js";
export { SettingsManager, type SettingsManagerOptions } from "./settings.js";
export type { WoaChannel, WoaConfig } from "./woa/index.js";

// Export types
export type { ClaudePlanEntry } from "./tools.js";
