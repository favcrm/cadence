/**
 * `@cadence/hosted-runtime`: a provider-neutral hosted agent conversation.
 *
 * Application code sees only the declarations below. The concrete engine, its
 * SDK, its prompt sections and its hooks stay private to this package.
 */
export { HostedSession } from "./hosted-session.js";
export type { AppScope, AskResult, AssistantIdentityContext, GenerationIdentity, HostedModelDiagnosticCode, HostedRunFailureCode, HostedSessionInspection, HostedSessionOptions, HostedSessionSummary, HostedState, HostedTurnDiagnosticEvent, HostedTurnDiagnosticsPage, MasterStatus, OperationReply, OperationSettlement, PendingOperation, RuntimeTool, RuntimeTools, ThreadContext, ThreadEntry, ThreadProjection, ToolContent, ToolContext, ToolInvocation, ToolResult, WorkspaceSkill, } from "./contract.js";
export { BOARD_MESSAGE_KIND, OPERATIONAL_SKILL_VERSION } from "./contract.js";
export { masterState, type MasterMetadata } from "./master.js";
/** Approximate tokens in the fixed board instructions shared by both assistant scopes. */
export declare function estimateBuiltInInstructionsTokens(): number;
