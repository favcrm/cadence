/**
 * `@cadence/hosted-runtime`: a provider-neutral hosted agent conversation.
 *
 * Application code sees only the declarations below. The concrete engine, its
 * SDK, its prompt sections and its hooks stay private to this package.
 */
export { HostedSession } from "./hosted-session.ts";
export type {
  AppScope,
  AskResult,
  AssistantIdentityContext,
  GenerationIdentity,
  HostedSessionOptions,
  HostedState,
  MasterStatus,
  PendingOperation,
  RuntimeTool,
  RuntimeTools,
  ThreadContext,
  ThreadEntry,
  ToolContent,
  ToolContext,
  ToolInvocation,
  ToolResult,
} from "./contract.ts";
export { BOARD_MESSAGE_KIND, OPERATIONAL_SKILL_VERSION } from "./contract.ts";
export { masterState, type MasterMetadata } from "./master.ts";
