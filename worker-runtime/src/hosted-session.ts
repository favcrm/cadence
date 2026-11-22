import { DurableObject } from "cloudflare:workers";
import { createModels } from "@earendil-works/pi-ai/models";
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import type { Op } from "@earendil-works/chord/delta";
import {
  type LiveState,
  type Cursor,
  CompactionTask,
  GenerationTask,
  createRegistry,
  Harness,
  hook,
  InboxDoc,
  LiveDoc,
  ROOT_CONVERSATION_ID,
  type JsonObject,
} from "@earendil-works/pi-durable";
import { PiHarness, skills } from "agents/harness/pi";
import { Lifecycle } from "agents/lifecycle";
import { createAI } from "agents/models/pi-ai";
import type {
  AppScope,
  AskResult,
  GenerationIdentity,
  HostedSessionInspection,
  HostedSessionOptions,
  HostedState,
  OperationReply,
  OperationSettlement,
  PendingOperation,
  ThreadContext,
  ThreadEntry,
} from "./contract.ts";
import { compactionFailureText } from "./contract.ts";
import type { CompactionFailureReason } from "./identity.ts";
import { assistantIdentityText } from "./assistant-identity.ts";
import { ContextBudgetProofs } from "./context-budget-proofs.ts";
import {
  authorizeExtension,
  intentGuard,
  runtimeExtension,
  type AppScopeResolver,
} from "./engine.ts";
import { LoopGuard, loopGuardExtension } from "./loop-guard.ts";
import {
  configureOperationReplyCapture,
  readCompactionFailure,
  readCompactionIdentity,
  readGenerationIdentity,
  readHeldSubmission,
  readOperationReply,
  readSubmissionEntry,
  readSubmissionRequestId,
  readSubmissionSettlement,
  type HeldSubmission,
  type TaskSql,
} from "./identity.ts";
import {
  guardModelRequests,
  identityContextExtension,
} from "./identity-context.ts";
import {
  IdentitySnapshots,
  type IdentitySnapshotSql,
} from "./identity-snapshots.ts";
import { masterState, operationStatus } from "./master.ts";
import { defaultModel, HOSTED_COMPACTION_POLICY, meteredBinding, RequestSizes, withDefaultModel } from "./model.ts";
import { ModelTaskContext } from "./model-task-context.ts";
import {
  guardOperationRequests,
  operationContextExtension,
  runAppScope,
} from "./operation-context.ts";
import {
  OperationScopes,
  parseEntryAppScope,
  parseEntryOperationId,
  requireOperationId,
  type EngineEvidence,
  type OperationEnvelope,
  type OperationScopeSql,
} from "./operation-scope.ts";
import { runtimeProfile, skillFreeRuntimeProfile } from "./profile.ts";
import { runtimeSkillSources } from "./workspace-skills.ts";
import { PreviewGuard } from "./preview.ts";
import { rawSessionFrame, sessionEventResponse } from "./session-stream.ts";
import { readSessionSummary } from "./session-summary.ts";
import { readSessionTranscript } from "./session-transcript.ts";
import type { ToolAuthority } from "./tool-policy.ts";
import { BOARD_MESSAGE_KIND } from "./contract.ts";
import { projectedThreadEntries, readAllRecords } from "./transcript.ts";

/**
 * The last model the platform gateway reported serving for this conversation.
 * Absent until the first call resolves the platform default.
 */
const RESOLVED_MODEL_KEY = "tenant_resolved_model";

/**
 * Re-read durable projections only when a view change can affect them. Partial
 * generation text alone must not trigger a transcript read for every token.
 */
function engineViewChanges(ops: readonly Op[]): {
  readonly master: boolean;
  readonly entries: boolean;
} {
  let master = false;
  let entries = false;
  for (const op of ops) {
    if (op[0] === "r" || op[1].length === 0) return { master: true, entries: true };
    const path = op[1];
    if (path[0] === "entries" || path[0] === "before") entries = true;
    if (path[0] === "conversation") {
      master = true;
      entries = true;
    }
    if (path[0] !== "docs") continue;
    if (path.length === 1) return { master: true, entries: true };
    if (path[1] === "pi.inbox") master = true;
    if (path[1] === "pi.live") {
      if (path.length === 2 || path[2] === "run" || path[2] === "runFailure") {
        master = true;
        entries = true;
      } else if (path[2] === "compactionFailure") {
        master = true;
      }
    }
  }
  return { master, entries };
}


/** CAD-2430: the board's sentence for a failed blocking compaction, or none. */
function failureSentence(reason: CompactionFailureReason | null): string | null {
  return reason === null ? null : compactionFailureText(reason);
}

/**
 * One agent conversation, one Durable Object.
 *
 * The engine owns the loop: transcript, inbox, model calls, tools, retries and
 * crash recovery. It keeps all of it in this object's SQLite (`pi_*` tables)
 * and a Lifecycle job, so an evicted or redeployed object restarts from the
 * engine's last checkpoint and finishes the turn instead of losing it.
 *
 * The object name is the application's conversation key. The engine owns its
 * own session ids (the root is "1"), so the name is never treated as one.
 *
 * The application supplies capabilities and the authority policy; this base
 * supplies the prompt/skill profile, the model transport, the transcript
 * projection, the board wire behavior and the execution guards. Only declared
 * tools are installed, and every execution revalidates the live authority.
 */
export class HostedSession<
  Env extends object = Record<string, unknown>,
> extends DurableObject<Env> {
  private readonly models = createModels();
  private readonly registry = createRegistry();
  private readonly options: HostedSessionOptions;
  /**
   * The session's insert-once operation envelopes. The engine deduplicates by
   * request id alone, so this guard is what refuses a repeated id whose text
   * or app scope changed.
   */
  private readonly operationScopes: OperationScopes;
  private readonly contextBudgetProofs: ContextBudgetProofs;
  private readonly modelTaskContext = new ModelTaskContext();
  private readonly loopGuard = new LoopGuard();
  /** The app scope of the operation that owns one tool call, read from the engine. */
  private readonly toolAppScope: AppScopeResolver = async (api, context) => {
    const live = await api.snapshot(LiveDoc, api.conversationId, context);
    const run = live?.run;
    // A tool executes inside a run that has inputs. Reading anything else as
    // Home would run an app turn unscoped, so the call refuses instead.
    if (run === undefined || run.inputs.length === 0)
      throw new Error("The active run's operation envelope is unavailable.");
    return runAppScope(run.inputs, (root) => this.operationAppScope(root));
  };

  private readonly harness: PiHarness;
  /** The last model the gateway reported in this object's lifetime. */
  private resolvedModel: string | null = null;

  constructor(
    ctx: DurableObjectState,
    env: Env,
    options: HostedSessionOptions,
  ) {
    super(ctx, env);
    this.options = options;
    if (!ctx.id.name)
      throw new Error("HostedSession requires a named conversation");
    this.operationScopes = new OperationScopes(
      ctx.storage.sql as unknown as OperationScopeSql,
    );
    this.contextBudgetProofs = new ContextBudgetProofs(
      ctx.storage.sql as unknown as TaskSql,
    );
    const acceptsImages = options.acceptsToolImages === true;
    const model = defaultModel(
      options.modelAlias,
      acceptsImages || options.attachedImages !== undefined,
    );
    const authority: ToolAuthority = {
      toolRefusal: (name) => options.toolRefusal(name),
      authorizeTool: (name) => options.authorizeTool(name),
    };
    const requestSizes = new RequestSizes();
    const ai = createAI({
      binding: meteredBinding(
        options,
        (signal) => this.modelIdentity(signal),
        (served) => this.recordResolvedModel(served),
        options.maxImagesPerRequest,
        this.contextBudgetProofs,
        (signal) => this.modelTaskContext.read(signal)?.kind === "compaction",
        requestSizes,
      ) as unknown as Ai,
    });
    this.models.setProvider(withDefaultModel(ai.provider, model));
    // CAD-2480: blocking compaction also starts on the request's bytes.
    this.registry.install({
      name: "hosted-request-bytes",
      hooks: [
        hook(GenerationTask, {
          afterResponse: (message, api) => requestSizes.withByteFloor(String(api.taskId), message),
        }),
      ],
    });
    this.registry.install(intentGuard(options.toolRefusal));
    // Install after intentGuard so policy and authority refusals always take precedence.
    this.registry.install(loopGuardExtension(this.loopGuard));
    const skillsEnabled = options.builtInSkills !== false;
    this.registry.install(skillsEnabled ? runtimeProfile : skillFreeRuntimeProfile);
    // Pi's token-based cut may fall inside the current operation, including
    // an artifact read. Decline rather than silently summarize that input.
    this.registry.install({
      name: "hosted-preserve-active-operation",
      hooks: [
        hook(CompactionTask, {
          beforeCompact: async (candidate, api, context) => {
            try {
              const conversation = await this.harness.conversation(this.harness.session().id);
              if (conversation.id !== api.conversationId) return { decline: true };
              const view = await conversation.context(context);
              const firstKept = view.entries.findIndex((entry) => entry.id === candidate.firstKept);
              if (firstKept < 0) return { decline: true };
              let completed = -1;
              for (let index = view.contributions.length - 1; index >= 0; index--) {
                if (view.contributions[index]!.some((message) =>
                  message.role === "assistant" &&
                  (message.stopReason === "stop" || message.stopReason === "length") &&
                  !message.content.some((part) => part.type === "toolCall"),
                )) {
                  completed = index;
                  break;
                }
              }
              for (let index = completed + 1; index < firstKept; index++) {
                if (view.contributions[index]!.some((message) =>
                  message.role === "user" ||
                  message.role === "toolResult" ||
                  (message.role === "assistant" &&
                    message.content.some((part) => part.type === "toolCall")),
                )) return { decline: true };
              }
              return undefined;
            } catch {
              return { decline: true };
            }
          },
        }),
      ],
    });
    if (options.identity) {
      const identityMessages = new WeakSet<object>();
      const identitySnapshots = new IdentitySnapshots(
        ctx.storage.sql as unknown as IdentitySnapshotSql,
      );
      // The host has a committed identity: this session privately formats it,
      // snapshots it once per operation and refuses a generation that did not
      // receive the very object it authored. A generic host installs neither,
      // and never silently receives a fallback.
      this.registry.install(
        identityContextExtension(
          () => this.identityContextText(),
          identityMessages,
          identitySnapshots,
        ),
      );
      guardModelRequests(this.models, identityMessages);
    }
    // The operation's admitted app scope reaches every model request and every
    // tool call of that operation. A Home operation resolves to `null`, so a
    // host that never sends a scope keeps its current context. The attestation
    // set is shared: the hook authors one message object per attempt, and the
    // boundary installed here refuses any request that carries none — so a
    // skipped or failing hook can never reach the provider unscoped.
    const operationMessages = new WeakSet<object>();
    this.registry.install(
      operationContextExtension(
        (root) => this.operationAppScope(root),
        operationMessages,
        options.attachedImages === undefined
          ? undefined
          : (roots, app, signal) => {
              const operationIds = this.operationIds(roots);
              return operationIds.length === 0
                ? Promise.resolve([])
                : options.attachedImages!({ operationIds, app }, signal);
            },
        options.maxImagesPerRequest,
      ),
    );
    guardOperationRequests(this.models, operationMessages);
    this.modelTaskContext.install(this.models);
    for (const group of options.tools) {
      this.registry.install(
        runtimeExtension(group, authority, this.toolAppScope, acceptsImages),
      );
    }
    this.harness = new PiHarness({
      harness: async ({ storage, context, onWake }) => {
        configureOperationReplyCapture(
          this.ctx.storage.sql as unknown as TaskSql,
          options.captureOperationReplies === true,
        );
        if (skillsEnabled) {
          // The bundled skills are engine-owned and get the same live
          // authorization as application tools. Workspace skills come last.
          const sources = await runtimeSkillSources(
            options.workspaceSkills === undefined ? undefined : () => options.workspaceSkills!(),
          );
          this.registry.install(authorizeExtension(await skills(sources), authority));
        }
        return Harness.open(
          storage,
          {
            models: this.models,
            registry: this.registry,
            onWake,
            // Keep the deferred progress window at zero, rather than adopting
            // the new one-second default for hosted conversation state.
            // CAD-2414: compact before the gateway's 256 KiB request limit.
            settings: { progress: { persistWithinMs: 0 }, compaction: HOSTED_COMPACTION_POLICY },
            telemetry: this.modelTaskContext.telemetry,
          },
          context,
        );
      },
      defaults: { model },
    });
    Lifecycle.install(this).use(this.harness);
  }

  /**
   * Board transcript SSE and owner-only diagnostic views. Diagnostic reconnects
   * get a fresh full view; board reconnects resume from a transcript cursor.
   */
  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (request.method !== "GET")
      return new Response("not found", { status: 404 });

    // The board's own thread stream: one `entry` frame per stored message,
    // `id:` the entry's `seq` so `?after=` resumes after the last receipt.
    // View operations determine when to re-read the authoritative transcript
    // or header. Partial generation updates need neither read.
    if (url.pathname === "/thread-stream") {
      // Raw thinking is an owner view; anything but an explicit owner reader
      // (set by the trusted HTTP adapter) gets none.
      const thinking = url.searchParams.get("reader") === "owner";
      const requested = Number(url.searchParams.get("after") ?? "0");
      let last =
        Number.isSafeInteger(requested) && requested > 0 ? requested : 0;
      // Observe every publication synchronously before attaching the async
      // watch: its folded updates alone cannot preserve privacy quarantine.
      const guard = new PreviewGuard();
      const pi = await this.harness.pi();
      let observerEnded = false;
      let stopWatch: (() => void) | undefined;
      const stopPreview = () => {
        observerEnded = true;
        stopWatch?.();
      };
      const observation = await pi.observeConversation(
        ROOT_CONVERSATION_ID,
        {
          advance: (call) => {
            if (call.kind === "failed" || call.retired) return stopPreview();
            try {
              guard.advance(call.value.docs["pi.live"] as LiveState | undefined);
            } catch {
              stopPreview();
            }
          },
          end: stopPreview,
        },
        BACKGROUND_CONTEXT,
      );
      try {
        guard.attach(observation.value.docs["pi.live"] as LiveState | undefined);
      } catch (error) {
        observation.detach();
        throw error;
      }
      let watch;
      try {
        watch = await this.harness.session().watch();
      } catch (error) {
        observation.detach();
        throw error;
      }
      stopWatch = () => { void watch.stop(); };
      if (observerEnded) {
        observation.detach();
        await watch.stop();
        return Response.json(
          { ok: false, error: { code: "capability_unavailable", message: "The thread observation is unavailable." } },
          { status: 503 },
        );
      }
      const stream = {
        value: watch.value,
        start: watch.start.bind(watch),
        stop: () => {
          observation.detach();
          return watch.stop();
        },
        closed: watch.closed.finally(() => {
          stopWatch = undefined;
          observation.detach();
        }),
      };
      // Read the projection now, at this connection's snapshot. A reconnect
      // reads the host's store again, so a connection never pins an older
      // revision/avatar/name over a newer committed edit; an unavailable,
      // malformed or unserializable projection fails the connection rather than
      // serving a partial header. The projection validated here also composes
      // every later `master` frame on this connection, so a refresh never
      // reaches for unvalidated host metadata.
      const master = await this.connectionMaster();
      if (master === null) {
        // The watch is this connection's now, so nothing else will stop it.
        await stream.stop();
        return Response.json(
          {
            ok: false,
            error: {
              code: "capability_unavailable",
              message: "The master identity projection is unavailable.",
            },
          },
          { status: 503 },
        );
      }
      const pinned = master.context;
      const masterFrame = master.frame;
      // The watch snapshot can precede this independently read header. Never
      // attach one run's partial to another run's current turn.
      let masterOperation = master.operationId;
      const initialLive = stream.value.docs["pi.live"] as LiveState | undefined;
      let previewOperation = this.previewOperation(initialLive?.run?.inputs);
      let previewGeneration = initialLive?.run?.taskId;
      let previewAttempt = initialLive?.generation?.attempt;
      let lastMasterFrame = masterFrame;
      // Read and project before SSE 200, so a failure refuses like the master
      // projection above rather than opening a body that errors and reconnects.
      // The engine's own snapshot is the active context, which numbers from 1
      // again after a reset: a reconnect must see the same `seq` as a page read.
      let snapshotEntries: ThreadEntry[];
      try {
        snapshotEntries = await this.transcript(thinking);
      } catch {
        await stream.stop();
        return Response.json(
          {
            ok: false,
            error: {
              code: "capability_unavailable",
              message: "The thread projection is unavailable.",
            },
          },
          { status: 503 },
        );
      }
      function emit(
        entry: ThreadEntry,
        send: (frame: string) => boolean,
      ): boolean {
        if (entry.seq <= last) return true;
        if (
          !send(
            `id: ${entry.seq}\nevent: entry\ndata: ${JSON.stringify(entry)}\n\n`,
          )
        )
          return false;
        last = entry.seq;
        return true;
      }
      let shownPreview: string | null = null;
      return sessionEventResponse(stream, {
        snapshot: (send, sendPreview) => {
          if (observerEnded) return;
          // The board flips its link to `live` on a `master` frame and on
          // nothing else, so a stream that never sends one reads
          // "reconnecting" forever — the stale banner then never clears.
          // It reports this object's real turn state. A host projection
          // composes under those runtime fields and cannot replace them.
          if (!send(": connected\n\n") || !send(masterFrame)) return;
          for (const entry of snapshotEntries) if (!emit(entry, send)) break;
          // The snapshot and registration are atomic with each other, but the
          // header was read later. A queued successor may already be running.
          // Only show a partial whose engine submission proves the same turn.
          if (masterOperation !== null && previewOperation === masterOperation)
            shownPreview = guard.text(initialLive);
          sendPreview(shownPreview);
        },
        change: async (change, send, sendPreview) => {
          const changes = engineViewChanges(change.ops);
          const live = change.value.docs["pi.live"] as LiveState | undefined;
          if (changes.master) {
            const next = await this.composedMasterFrame(pinned);
            if (observerEnded) return;
            if (next.operationId !== masterOperation) {
              if (shownPreview !== null) sendPreview(null);
              shownPreview = null;
              masterOperation = next.operationId;
            }
            if (next.frame !== lastMasterFrame) {
              lastMasterFrame = next.frame;
              if (!send(next.frame)) return;
            }
          }
          const source = changes.master
            ? this.previewOperation(live?.run?.inputs)
            : previewOperation;
          const generation = live?.run?.taskId;
          const attempt = live?.generation?.attempt;
          if (
            source !== previewOperation ||
            generation !== previewGeneration ||
            attempt !== previewAttempt
          ) {
            if (shownPreview !== null) sendPreview(null);
            shownPreview = null;
          }
          previewOperation = source;
          previewGeneration = generation;
          previewAttempt = attempt;
          const before = shownPreview;
          // A predecessor's committed entry must precede a successor's partial.
          if (changes.entries) {
            const entries = await this.transcript(thinking);
            if (observerEnded) return;
            for (const entry of entries) if (!emit(entry, send)) break;
          }
          if (observerEnded) return;
          const current = masterOperation !== null && source === masterOperation
            ? guard.text(live)
            : null;
          if (current !== null) {
            if (current !== before || changes.entries) sendPreview(current);
            shownPreview = current;
          } else if (before !== null) {
            sendPreview(null);
            shownPreview = null;
          }
        },
      }, { overflow: "after-cursor" });
    }

    if (url.pathname !== "/events")
      return new Response("not found", { status: 404 });
    const stream = await this.harness.session().watch();
    return sessionEventResponse(stream, {
      snapshot: (send) => {
        send(rawSessionFrame({ type: "snapshot", version: 1, view: stream.value }));
      },
      change: async (change, send) => {
        send(rawSessionFrame({ type: "view", version: 1, view: change.value }));
      },
    });
  }

  /**
   * Ask the session and wait for the answer.
   *
   * `app` is the host-verified scope of this turn; omit it for Home. The
   * operation id is bound to the text and scope it was first admitted with, so
   * a retry with the same id replays that operation and a retry that changes
   * either is refused before the engine runs.
   * Only a host-admitted user message may declare `transcriptRole`.
   */
  async ask(
    prompt: string,
    operationId?: string,
    app?: AppScope,
    transcriptRole?: "operator",
  ): Promise<AskResult> {
    await this.requireIdentity();
    const envelope = await this.admitOperation(prompt, operationId, app);
    const result = await this.harness.prompt(envelope.text, {
      operationId: envelope.operationId,
      entry: this.inputEntry(envelope, transcriptRole),
    });
    return {
      status: result.status,
      text: result.text,
      operationId: result.operationId,
    };
  }

  /** Map the engine's committed records, including durable input metadata, for the board. */
  async transcript(includeThinking: boolean): Promise<ThreadEntry[]> {
    const conversation = await this.harness.conversation(
      this.harness.session().id,
    );
    return projectedThreadEntries(
      await readAllRecords<Cursor>((cursor, limit) =>
        conversation.entries(
          { order: "ascending" },
          limit,
          cursor,
          BACKGROUND_CONTEXT,
        ),
      ),
      includeThinking,
      this.options.projectThread,
      (entry, message) => this.contextBudgetProofs.matches(entry, message),
      (entry, message) => this.contextBudgetProofs.settled(entry, message),
    );
  }

  /**
   * Inspect the current or a previously observed generation without opening
   * the engine, submitting work or reading the transcript.
   */
  sessionSummary(sessionId?: string): HostedSessionInspection {
    return readSessionSummary(
      this.ctx.storage.sql as unknown as TaskSql,
      this.ctx.id.toString(),
      sessionId,
    );
  }

  /** Read only the proven generation's committed, user-visible text. */
  sessionTranscript(sessionId: string, after: number): {
    messages: { role: "operator" | "agent"; text: string; created?: string }[];
    nextCursor: number | null;
  } | null {
    return readSessionTranscript(
      this.ctx.storage.sql as unknown as TaskSql,
      this.ctx.id.toString(),
      sessionId,
      after,
      this.options.projectThread,
    );
  }

  /** Stored runtime records, not expanded presentation entries. Read-only. */
  async recordCount(): Promise<number> {
    return (await this.harness.session().messages()).length;
  }

  /** Abort this instance; durable state is retained when it is reconstructed. */
  reload(): string {
    this.ctx.abort();
    return "aborting";
  }

  /**
   * Start a fresh context for this conversation. The model stops seeing what
   * came before, while the board keeps showing it, with a marker where it
   * happened. This also recovers a stale session a normal reload cannot. The
   * callers are owner-only routes; this is deliberately not an automatic retry.
   */
  async resetConversation(): Promise<void> {
    await this.harness.session().reset();
    this.loopGuard?.reset();
  }

  /**
   * Submit without waiting: the board's send is queued, not a blocking turn.
   *
   * `app` is the host-verified scope of this turn; omit it for Home. The
   * operation id is bound to the text and scope it was first admitted with, so
   * a retried send with the same id replays that operation and one that changes
   * either is refused before the engine is asked.
   * Only a host-admitted user message may declare `transcriptRole`.
   */
  async submit(
    text: string,
    operationId: string,
    app?: AppScope,
    transcriptRole?: "operator",
  ): Promise<boolean> {
    await this.requireIdentity();
    const envelope = await this.admitOperation(text, operationId, app);
    const receipt = await this.harness.submit(envelope.text, {
      operationId: envelope.operationId,
      entry: this.inputEntry(envelope, transcriptRole),
    });
    return receipt.accepted;
  }

  /**
   * Bind one operation to the exact text and app scope it was admitted with,
   * and return the stored envelope. A changed text or scope throws here, before
   * the engine sees the id, so a repeated id can never reuse another turn.
   *
   * An id this session never admitted may still be the engine's, from a build
   * that predates the guard, so the engine's own records are read first and
   * passed with the envelope: the guard adopts a legacy operation only as the
   * Home envelope those records prove, and refuses anything else.
   */
  private async admitOperation(
    text: string,
    operationId: string | undefined,
    app: AppScope | undefined,
  ): Promise<OperationEnvelope> {
    const id = operationId ?? crypto.randomUUID();
    const envelope = await this.operationScopes.admit(
      app === undefined
        ? { operationId: id, text }
        : { operationId: id, text, app },
      await this.engineEvidence(id),
    );
    this.loopGuard?.reset();
    return envelope;
  }

  /**
   * What the engine's own records prove about an operation id this guard has no
   * envelope for: `null` when it holds nothing for it, otherwise the submission
   * type, the exact text and the entry metadata the guard needs to recognise a
   * legacy Home operation it may adopt.
   *
   * A placed input keeps its text and metadata in the entry it was placed as. A
   * queued one has no entry yet, so its draft is read from the engine's own
   * inbox document. That read awaits, and the engine may place the input and
   * drop the draft while it does — one commit, so the draft is gone exactly
   * when the submission has become placed. An absent draft is therefore re-read
   * once from the submission itself, never in a loop, and recovered from the
   * entry it now names; only a record that proves neither is left proving
   * nothing, and the guard refuses it. A submission that is not an input, and a
   * record the engine cannot produce, also yield evidence that proves nothing.
   * Reading a record the engine cannot parse throws, so an unreadable id is
   * refused rather than assumed free.
   */
  private async engineEvidence(
    operationId: string,
  ): Promise<EngineEvidence | null> {
    const sql = this.ctx.storage.sql as unknown as TaskSql;
    const held = readHeldSubmission(sql, ROOT_CONVERSATION_ID, operationId);
    if (held === null) return null;
    if (
      held.entry !== null ||
      held.type !== "input" ||
      held.status !== "queued"
    )
      return this.placedEvidence(held);
    const draft = await this.queuedInput(held.id);
    if (draft !== null)
      return { type: held.type, text: draft.text, entry: draft.entry };
    // Placing the input and removing its draft happen in one engine commit, so
    // the submission now says where the text went. Re-read it once.
    const placed = readHeldSubmission(sql, ROOT_CONVERSATION_ID, operationId);
    return this.placedEvidence(placed ?? held);
  }

  /**
   * The evidence one stored submission proves: its type, the exact text and the
   * entry metadata, when the engine placed an entry for it. A submission with
   * no entry proves no text here, which its caller refuses.
   */
  private placedEvidence(held: HeldSubmission): EngineEvidence {
    const entry = held.entry;
    return entry === null
      ? { type: held.type, text: held.text, entry: null }
      : {
          type: held.type,
          text: held.text,
          entry: { kind: entry.kind, data: entry.data },
        };
  }

  /**
   * The draft one queued input waits in the engine's inbox with, or `null` when
   * the inbox does not hold it. `content` is the exact text it was submitted
   * with and `entry` the metadata it will be placed as, so both are the
   * engine's own record of the operation rather than anything a caller sent.
   */
  private async queuedInput(
    submissionId: number,
  ): Promise<{
    text: unknown;
    entry: { kind: unknown; data: unknown } | null;
  } | null> {
    const pi = await this.harness.pi();
    const inbox = await pi.snapshot(
      InboxDoc,
      ROOT_CONVERSATION_ID,
      BACKGROUND_CONTEXT,
    );
    if (inbox === undefined) return null;
    for (const item of inbox.items) {
      if (item.mode === "write" || item.id !== submissionId) continue;
      const entry = item.entry;
      return {
        text: item.content,
        entry:
          entry === undefined
            ? null
            : {
                kind: entry.kind,
                data: "data" in entry ? entry.data : undefined,
              },
      };
    }
    return null;
  }

  /**
   * The durable input entry an operation is placed as. The recorded message id
   * and the admitted app scope travel with the entry, so the transcript, a
   * rebuilt object and a replayed retry all read the same envelope.
   */
  private inputEntry(envelope: OperationEnvelope, transcriptRole: "operator" | undefined): {
    kind: string;
    data: JsonObject;
  } {
    const data: JsonObject = { messageId: envelope.operationId };
    if (transcriptRole === "operator") data.transcriptRole = transcriptRole;
    if (envelope.app !== undefined)
      data.app =
        envelope.app.contextId === undefined
          ? { installId: envelope.app.installId }
          : {
              installId: envelope.app.installId,
              contextId: envelope.app.contextId,
            };
    return { kind: BOARD_MESSAGE_KIND, data };
  }

  /**
   * The app scope of the operation whose input root is this engine submission,
   * read from the placed entry the engine stored. `null` is proven Home: a
   * readable input entry that carries no scope. An entry that cannot be read
   * throws, so the caller refuses rather than running an app turn as Home.
   */
  private operationAppScope(root: string): AppScope | null {
    const entry = readSubmissionEntry(
      this.ctx.storage.sql as unknown as TaskSql,
      root,
    );
    return parseEntryAppScope(entry.kind, entry.data);
  }

  /** The message ids of the run's own placed board inputs, never prompt text. */
  private operationIds(roots: readonly number[]): string[] {
    const ids: string[] = [];
    const seen = new Set<string>();
    for (const root of roots) {
      const entry = readSubmissionEntry(
        this.ctx.storage.sql as unknown as TaskSql,
        String(root),
      );
      const id = parseEntryOperationId(entry.kind, entry.data);
      // A pre-board legacy input cannot carry host attachment bindings.
      if (id === null) continue;
      if (
        id !== readSubmissionRequestId(this.ctx.storage.sql as unknown as TaskSql, String(root)) ||
        seen.has(id)
      )
        throw new Error("The active operation's message identity is unavailable.");
      seen.add(id);
      ids.push(id);
    }
    return ids;
  }

  /** What the runtime has not settled yet, so the board can show queued/running/parked. */
  async pendingOperations(): Promise<PendingOperation[]> {
    return this.harness.pending();
  }

  /**
   * Resolve and render the host's committed identity before admitting new
   * work. A refusal propagates unchanged, so an operation never starts when its
   * canonical identity is unavailable; a host without the feature is unchanged.
   */
  private async requireIdentity(): Promise<void> {
    const identity = this.options.identity;
    if (identity) assistantIdentityText(await identity());
  }

  /**
   * The committed identity text this conversation's operations see. It is
   * resolved for admission and again for an operation's first request; only the
   * immutable operation snapshot stores it, so a change committed mid-operation
   * applies to the next one.
   */
  private async identityContextText(): Promise<string> {
    const identity = this.options.identity;
    if (!identity)
      throw new Error("This session has no identity context configured.");
    return assistantIdentityText(await identity());
  }

  /**
   * The one connection's public projection, checked before it is framed. A
   * malformed projection fails the connection exactly like an unavailable one,
   * so a partial header is never served.
   */
  private validatedThreadContext(context: ThreadContext): ThreadContext {
    const metadata: unknown = context?.metadata;
    if (
      typeof context?.conversationId !== "string" ||
      context.conversationId.length === 0 ||
      metadata === null ||
      typeof metadata !== "object" ||
      Array.isArray(metadata)
    )
      throw new Error("The thread context is malformed.");
    return context;
  }

  /**
   * This connection's first `master` frame and the validated host projection it
   * composes under, or `null` when either cannot be produced.
   *
   * The projection is re-read here, once per connection, so a reconnect never
   * pins an older revision/avatar/name over a newer committed edit. This
   * connection serves one conversation, so later frames compose under the same
   * validated projection. The engine state is read only after the watch this
   * connection has attached, so an already-running turn is reported truthfully
   * and no run can begin between that read and the subscription. An
   * unavailable or malformed projection, an unserializable frame and a failed
   * state read all return `null` and refuse the connection with `503` rather
   * than serve a stale or partial header; the caller stops the watch it opened.
   * After `200`, a failing read rejects the event listener, which ends the
   * stream exactly like a failed transcript read.
   */
  private async connectionMaster(): Promise<{
    readonly context: ThreadContext | null;
    readonly frame: string;
    readonly operationId: string | null;
  } | null> {
    try {
      const context = this.options.threadContext
        ? this.validatedThreadContext(await this.options.threadContext())
        : null;
      return { context, ...(await this.composedMasterFrame(context)) };
    } catch {
      return null;
    }
  }

  /**
   * One `master` frame: the engine's current turn and queue facts under the
   * runtime's own fields, with the host projection this connection validated.
   *
   * The state is read for every frame, never derived from an event, so a frame
   * reports only what the durable records prove. Metadata may contain non-JSON
   * values, while reserved fields it cannot override retain runtime precedence;
   * a callable root `toJSON` refuses the frame.
   */
  private async composedMasterFrame(
    context: ThreadContext | null,
  ): Promise<{ readonly frame: string; readonly operationId: string | null }> {
    const status = await this.state();
    return {
      operationId: status.running ? status.message : null,
      frame: `event: master\ndata: ${JSON.stringify(
        masterState(
          status.model,
          status,
          context?.conversationId ?? null,
          context?.metadata,
        ),
      )}\n\n`,
    };
  }

  /** Resolve a watch run's first input to the engine's admitted operation id. */
  private previewOperation(inputs: readonly number[] | undefined): string | null {
    if (inputs === undefined || inputs.length === 0) return null;
    try {
      return readSubmissionRequestId(
        this.ctx.storage.sql as unknown as TaskSql,
        String(inputs[0]),
      );
    } catch {
      // An unknown or pruned input cannot safely bind a transient preview.
      return null;
    }
  }

  /**
   * Resolve the engine invocation that made this exact model call. Generation
   * retains its existing fail-closed uniqueness check; compaction must present
   * the task id attested by its own phase before its checkpoint can fund a call.
   */
  private modelIdentity(signal: AbortSignal | undefined): GenerationIdentity | null {
    const call = this.modelTaskContext.read(signal);
    if (!call) return null;
    const sql = this.ctx.storage.sql as unknown as TaskSql;
    if (call.kind === "compaction") return readCompactionIdentity(sql, call.task);
    const identity = readGenerationIdentity(sql);
    return call.task === undefined || identity?.task === String(call.task) ? identity : null;
  }

  /** Whether a run is in flight. */
  async isBusy(): Promise<boolean> {
    return this.harness.session().busy();
  }

  /**
   * Cheap header state for `/api/master/state`: whether a turn is running, how
   * many inputs are queued, and the model label. `parked` operations are
   * neither running nor queued — the engine stopped them on its crash-loop
   * guard, so counting them as queued would invent work. The label is the
   * model the gateway actually served, or the configured alias until the first
   * call resolves the platform default.
   */
  async state(): Promise<HostedState> {
    const [pending, model] = await Promise.all([
      this.pendingOperations(),
      this.modelLabel(),
    ]);
    return {
      ...operationStatus(pending),
      model,
      compactionFailure: failureSentence(
        readCompactionFailure(
          this.ctx.storage.sql as unknown as TaskSql,
          Number(this.harness.session().id),
        ),
      ),
    };
  }

  /**
   * Inspect durable settlement without opening the engine or waking work.
   * Absence is evidence only; the host must separately fence admission.
   * Only interactive chat admission may opt in to overlapping a passive summary.
   */
  async settledOperation(operationId: string, mode?: "chat"): Promise<OperationSettlement> {
    const id = requireOperationId(operationId);
    const conversation = String(ROOT_CONVERSATION_ID);
    const settlement = readSubmissionSettlement(
      this.ctx.storage.sql as unknown as TaskSql,
      ROOT_CONVERSATION_ID,
      id,
      mode === "chat",
    );
    if (settlement.state === "settled")
      return {
        state: "settled",
        operationId: id,
        conversation,
        submission: String(settlement.submission),
        status: settlement.status,
      };
    return { state: settlement.state, operationId: id, conversation };
  }

  /** Inspect one reply without resending, waking the engine, or reading a transcript. */
  async operationReply(operationId: string): Promise<OperationReply> {
    const id = requireOperationId(operationId);
    if (this.options.captureOperationReplies !== true)
      return { state: "unconfirmed", operationId: id };
    return readOperationReply(
      this.ctx.storage.sql as unknown as TaskSql,
      ROOT_CONVERSATION_ID,
      id,
    );
  }

  /**
   * Stop the turn with this id. The id is required: the engine reads a missing
   * one as "abort this session" even when nothing has been asked to stop.
   * Stopping a running turn aborts its conversation, and the engine withdraws
   * the conversation's queued inputs with it: a caller reads `queuedMessages`
   * first and hands those back to the person who sent them. False when nothing
   * matched.
   */
  async stopOperation(operationId: string): Promise<boolean> {
    if (!operationId) return false;
    return this.harness.session().abort(operationId);
  }

  private recordResolvedModel(served: string): void {
    // Only a change is written: every tool round reports the same id.
    if (this.resolvedModel === served) return;
    this.resolvedModel = served;
    void this.ctx.storage
      .put(RESOLVED_MODEL_KEY, served)
      .catch(() => undefined);
  }

  private async modelLabel(): Promise<string> {
    return (
      this.resolvedModel ??
      (await this.ctx.storage.get<string>(RESOLVED_MODEL_KEY)) ??
      this.options.modelAlias
    );
  }
}
