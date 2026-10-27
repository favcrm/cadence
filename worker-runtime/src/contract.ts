/**
 * The provider-neutral contract of the Cadence hosted runtime.
 *
 * This module is the whole application-facing seam: no engine SDK type,
 * registry, hook, prompt section or provider model crosses it. The concrete
 * engine stays a private implementation detail.
 */

/**
 * Stable identity of one durable model request.
 *
 * It names the request the funding ledger keys on: the conversation, the
 * durable generation task (stable across provider retries and crash recovery)
 * and the transcript cutoff (which advances per tool round). A caller never
 * parses engine state to obtain it.
 */
export interface GenerationIdentity {
  /** Conversation id; the durable root is `"1"`. */
  readonly conversation: string;
  /** The durable generation task id. */
  readonly task: string;
  /** The transcript entry this request was cut at. */
  readonly cutoff: string;
}

/** One stored transcript entry, as the board renders it. */
export interface ThreadEntry {
  seq: number;
  role: string;
  kind: string;
  text: string;
  /** The recorded message id of the operator input, never inferred from text. */
  message?: string;
  payload?: {
    redacted?: true;
    tool_use_id?: string;
    is_error?: true;
    /** Safe terminal failure projection; never raw provider diagnostics. */
    status?: "failed";
    error?: string;
  };
}

/**
 * The minimal per-call surface a tool receives. Deliberately not the engine's
 * task model, registry or `executeTool` authority: a tool can identify its
 * stable key and read the run clock, nothing more.
 */
export interface ToolInvocation {
  readonly conversationId: number;
  readonly taskId: number;
  /** Epoch milliseconds, from the engine's run clock. */
  now(): number;
}

/** Per-call cancellation surface. */
export interface ToolContext {
  readonly abortSignal?: AbortSignal;
}

/** One text part of a tool result. */
export interface ToolContent {
  readonly type: "text";
  readonly text: string;
}

/** What a tool returns to the model. */
export interface ToolResult {
  /** Text parts for the model; omitted means no output of its own. */
  readonly output?: readonly ToolContent[];
  readonly isError?: boolean;
}

/**
 * One declared capability.
 *
 * `execute` is a method, so a typed handler (`RuntimeTool<{issueId: string}>`)
 * assigns to `RuntimeTool` without a cast. `parameters` is a plain JSON
 * schema; the engine validates arguments against it before `execute`.
 */
export interface RuntimeTool<Input = unknown> {
  readonly name: string;
  readonly description: string;
  readonly parameters: Readonly<Record<string, unknown>>;
  /** Whether an interrupted execution may rerun on recovery. Default `unsafe`. */
  readonly replay?: "safe" | "unsafe";
  execute(
    input: Input,
    invocation: ToolInvocation,
    context: ToolContext,
  ): Promise<ToolResult>;
}

/**
 * One named group of tools. The system prompt and skill profile belong to the
 * runtime, so an application never supplies sections or hooks.
 */
export interface RuntimeTools {
  readonly name: string;
  readonly tools?: readonly RuntimeTool[];
}

/**
 * The committed assistant identity a host has for one conversation.
 *
 * This is domain data only: the host resolves and validates its own canonical
 * store and supplies the result. The runtime decides how the committed identity
 * reaches the model and never accepts a prompt string, a model message, an
 * engine hook or an SDK type through this seam.
 *
 * - `personal` is actor-private: it belongs to the actor who owns the
 *   conversation. `user_name` and `preferences` are private context and must
 *   not be copied into any public projection.
 * - `workspace` is the shared operator-managed coordinator identity. It has no
 *   personal address or preferences.
 */
export type AssistantIdentityContext =
  | {
      readonly scope: "personal";
      readonly profile: {
        readonly identity: {
          readonly name: string | null;
          readonly tone: string | null;
          readonly look: string;
          readonly revision: number;
        };
        readonly bootstrap_state:
          "pending" | "complete" | "skipped" | "deferred";
        readonly user_name: string | null;
        readonly preferences: string | null;
      };
      /** The host's avatar catalogue, in its own established order. */
      readonly avatarLooks: readonly string[];
    }
  | {
      readonly scope: "workspace";
      readonly identity: {
        readonly name: string | null;
        readonly tone: string | null;
        readonly look: string;
        readonly revision: string;
      };
    };

/**
 * The current public projection of one board thread.
 *
 * `metadata` is application-owned business identity (for example a public
 * workspace/actor identity projection). The runtime composes it under its own
 * header fields, so a business field can never replace the model, provider,
 * session, turn or queue state. Private per-actor context does not belong here.
 */
export interface ThreadContext {
  readonly conversationId: string;
  readonly metadata: Readonly<Record<string, unknown>>;
}

/**
 * What the host application supplies to one hosted conversation.
 *
 * `requestModel` is the only model path: the runtime never calls a provider,
 * holds provider credentials or chooses a fallback route.
 */
export interface HostedSessionOptions {
  /** The platform default alias this deployment sends. */
  readonly modelAlias: string;
  /**
   * One model request through the platform proxy. The runtime relays the
   * returned bytes unchanged and only observes the served-model label.
   */
  requestModel(
    input: Record<string, unknown>,
    generation: GenerationIdentity,
    signal?: AbortSignal,
  ): Promise<Response>;
  /**
   * Resume one provider stream through the same proxy. Only an engine that
   * resumes streams calls it; without it the runtime refuses, never invents a
   * success.
   */
  resumeModel?(url: string, signal?: AbortSignal): Promise<Response>;
  readonly tools: readonly RuntimeTools[];
  /**
   * Revalidate the live authority of this conversation before an execution.
   * Runs on every attempt, including a replay whose intent is already durable.
   */
  authorizeTool(name: string): Promise<void>;
  /**
   * The application's refusal for one tool name, or `undefined` to allow it.
   * Consulted for the declared intent and again for each execution.
   */
  toolRefusal(name: string): string | undefined;
  /**
   * The committed identity of this conversation's actor or workspace.
   *
   * Optional for a host without an identity feature. When supplied, the
   * runtime formats it privately, snapshots it once per operation and enforces
   * it on every actual model generation; there is no generic fallback. A
   * refusal must throw (for example a malformed or unavailable committed row),
   * so admission fails closed before new work.
   */
  identity?(): Promise<AssistantIdentityContext>;
  /**
   * The current public projection of this thread, read for each new stream
   * connection so a reconnect observes the current committed identity. A
   * refusal throws; the runtime then fails the connection instead of serving a
   * stale or partial header.
   */
  threadContext?(): Promise<ThreadContext>;
}

/** How one asked turn ended. */
export interface AskResult {
  readonly status: "done" | "unanswered";
  /** The final assistant text, when answered. */
  readonly text: string | undefined;
  readonly operationId: string | undefined;
}

/** One submission the runtime has not settled yet. */
export interface PendingOperation {
  readonly status: "queued" | "running" | "parked";
}

/** Cheap header state for a conversation. */
export interface HostedState {
  readonly running: boolean;
  readonly queued: number;
  readonly model: string;
}

/**
 * The entry kind this runtime stores an operator submission as, and the
 * metadata kind an application records a chat message under.
 */
export const BOARD_MESSAGE_KIND = "tenant.board.message";

/**
 * Version of the bundled operational skill catalog. An application stamps its
 * domain drafts with it; the runtime's skill manifest uses the same value as
 * its fingerprint.
 */
export const OPERATIONAL_SKILL_VERSION = "tenant-operations-1";

/** Whether a turn is running and how many inputs are queued. */
export interface MasterStatus {
  readonly running: boolean;
  readonly queued: number;
}
