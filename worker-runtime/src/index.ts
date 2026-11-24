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
  HostedModelDiagnosticCode,
  HostedRunFailureCode,
  HostedSessionInspection,
  HostedSessionOptions,
  HostedSessionSummary,
  HostedState,
  HostedTurnDiagnosticEvent,
  HostedTurnDiagnosticsPage,
  MasterStatus,
  OperationReply,
  OperationSettlement,
  PendingOperation,
  RuntimeTool,
  RuntimeTools,
  ThreadContext,
  ThreadEntry,
  ThreadProjection,
  ToolContent,
  ToolContext,
  ToolInvocation,
  ToolResult,
  WorkspaceSkill,
} from "./contract.ts";
export { BOARD_MESSAGE_KIND, OPERATIONAL_SKILL_VERSION } from "./contract.ts";
export { masterState, type MasterMetadata } from "./master.ts";

import { RUNTIME_PROMPT } from "./profile.ts";

/** Approximate tokens in the fixed board instructions shared by both assistant scopes. */
export function estimateBuiltInInstructionsTokens(): number {
  return Math.ceil(RUNTIME_PROMPT.length / 4);
}
