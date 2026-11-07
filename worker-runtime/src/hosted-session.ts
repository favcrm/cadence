import { DurableObject } from "cloudflare:workers";
import { createModels } from "@earendil-works/pi-ai/models";
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import {
  type AgentEvent,
  type Cursor,
  createRegistry,
  Harness,
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
  HostedSessionOptions,
  HostedState,
  PendingOperation,
  ThreadContext,
  ThreadEntry,
} from "./contract.ts";
import { assistantIdentityText } from "./assistant-identity.ts";
import { ContextBudgetProofs } from "./context-budget-proofs.ts";
import {
  authorizeExtension,
  intentGuard,
  runtimeExtension,
  type AppScopeResolver,
} from "./engine.ts";
import {
  readGenerationIdentity,
  readHeldSubmission,
  readSubmissionEntry,
  readSubmissionRequestId,
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
import { defaultModel, meteredBinding, withDefaultModel } from "./model.ts";
import {
  guardOperationRequests,
  operationContextExtension,
  runAppScope,
} from "./operation-context.ts";
import {
  OperationScopes,
  parseEntryAppScope,
  parseEntryOperationId,
  type EngineEvidence,
  type OperationEnvelope,
  type OperationScopeSql,
} from "./operation-scope.ts";
import {
  companyOutputSkills,
  operationalSkills,
  runtimeHealthSkills,
  runtimeProfile,
} from "./profile.ts";
import { ThreadPreview } from "./preview.ts";
import { rawSessionFrame, sessionEventResponse } from "./session-stream.ts";
import type { ToolAuthority } from "./tool-policy.ts";
import { BOARD_MESSAGE_KIND } from "./contract.ts";
import { projectedThreadEntries, readAllRecords } from "./transcript.ts";

/**
 * The last model the platform gateway reported serving for this conversation.
 * Absent until the first call resolves the platform default.
 */
const RESOLVED_MODEL_KEY = "tenant_resolved_model";

/**
 * What one engine batch can change for the board's thread stream.
 *
 * The engine's batches only say that something changed, so the stream decides
 * what to re-project from its durable records. Most batches are partial text:
 * `message_start`, `message_update` (text, thinking or tool-argument deltas),
 * retries, deferred polls, tool progress, usage and agent-document changes.
 * Those change neither the header's turn facts nor the committed transcript, so
 * a token must never cost a full transcript read per reader. The commits below
 * do change one of them.
 *
 * `snapshot` is the engine's overflow replacement, so it can replace both. The
 * entry-producing events — `message_end`, `entry_appended`, and a
 * `tool_execution_end` that carries the result entry — reconcile the
 * transcript, and so do the ends of a run and of a turn: a connection settles
 * whatever its cursor has not caught up with at the boundary of the work that
 * produced it, even when the entry's own event was not observable. `run_start`,
 * `run_end`, `submission` and `inbox_update` change which operations are
 * running or queued, and a failed generation can park a turn the run did not
 * end; a queued input has no transcript entry until it is placed.
 */
function engineBatchChanges(events: readonly AgentEvent[]): {
  readonly master: boolean;
  readonly entries: boolean;
} {
  let master = false;
  let entries = false;
  for (const event of events) {
    switch (event.type) {
      case "snapshot":
        master = true;
        entries = true;
        break;
      case "run_start":
      case "submission":
      case "inbox_update":
        master = true;
        break;
      case "run_end":
        // The run's inputs settled: its header facts changed and the entries
        // committed with that settlement are the final ones for the run.
        master = true;
        entries = true;
        break;
      case "turn_end":
        entries = true;
        break;
      case "message_end":
      case "entry_appended":
        entries = true;
        break;
      case "tool_execution_end":
        if (event.entry !== undefined) entries = true;
        break;
      case "task_failed":
        if (event.kind === "pi.generation") master = true;
        break;
      default:
        break;
    }
  }
  return { master, entries };
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
    const ai = createAI({
      binding: meteredBinding(
        options,
        () => this.generationIdentity(),
        (served) => this.recordResolvedModel(served),
        options.maxImagesPerRequest,
        this.contextBudgetProofs,
      ) as unknown as Ai,
    });
    this.models.setProvider(withDefaultModel(ai.provider, model));
    this.registry.install(intentGuard(options.toolRefusal));
    this.registry.install(runtimeProfile);
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
    for (const group of options.tools) {
      this.registry.install(
        runtimeExtension(group, authority, this.toolAppScope, acceptsImages),
      );
    }
    this.harness = new PiHarness({
      harness: async ({ storage, context, onWake }) => {
        // The bundled skills are engine-owned, so they are wrapped here with
        // the same refusal and live authorization an application tool gets.
        this.registry.install(
          authorizeExtension(
            await skills([operationalSkills, runtimeHealthSkills, companyOutputSkills]),
            authority,
          ),
        );
        return Harness.open(
          storage,
          { models: this.models, registry: this.registry, onWake },
          context,
        );
      },
      defaults: { model },
    });
    Lifecycle.install(this).use(this.harness);
  }

  /**
   * SSE for this conversation: the engine's snapshot, then one batch per
   * committed change. The stream lives in memory, so a client that reconnects
   * gets a fresh snapshot rather than a cursor.
   */
  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (request.method !== "GET")
      return new Response("not found", { status: 404 });

    // The board's own thread stream: one `entry` frame per stored message,
    // `id:` the entry's `seq` so the client's `?after=` resume skips what it
    // already has. The engine's batches say "something changed", not "which
    // entry"; the transcript is the authority, so a batch that can have
    // committed or replaced entries re-reads it and emits whatever is past
    // `last`. A partial-text batch reads nothing. The `master` frame is
    // likewise re-sent when a batch changes the turn or queue facts, before any
    // of that batch's preview text.
    if (url.pathname === "/thread-stream") {
      // Raw thinking is an owner view; anything but an explicit owner reader
      // (set by the trusted HTTP adapter) gets none.
      const thinking = url.searchParams.get("reader") === "owner";
      const requested = Number(url.searchParams.get("after") ?? "0");
      let last =
        Number.isSafeInteger(requested) && requested > 0 ? requested : 0;
      // Attach the watch before reading anything else. The engine captures its
      // snapshot and its registration for later commits atomically, so a run
      // another participant starts while this connection is being set up is
      // either already in the state read below or delivered as a batch: there
      // is no window between a status read and the subscription in which a run
      // starts unseen. A run that starts later is announced by the fresh
      // `master` frame its batch sends before any of that run's preview text.
      const stream = await this.harness.session().events();
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
      // A later refresh is skipped when it would repeat this exact payload.
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
      const preview = new ThreadPreview();
      return sessionEventResponse(stream, {
        snapshot: (send, sendPreview) => {
          // The board flips its link to `live` on a `master` frame and on
          // nothing else, so a stream that never sends one reads
          // "reconnecting" forever — the stale banner then never clears.
          // It reports this object's real turn state. A host projection
          // composes under those runtime fields and cannot replace them.
          if (!send(": connected\n\n") || !send(masterFrame)) return;
          for (const entry of snapshotEntries) if (!emit(entry, send)) break;
          // The watch snapshot is the authoritative in-flight state, so a
          // reconnect (or an overflow replacement) starts from the engine's
          // current partial instead of a stale one. It follows the committed
          // entries and has no `id:`, so it cannot move the durable cursor.
          preview.applySnapshot(stream.snapshot);
          sendPreview(preview.text);
        },
        events: async (events, send, sendPreview) => {
          const changes = engineBatchChanges(events);
          // A turn or queue transition is announced before any transient text
          // of the same batch, so a viewer that connected while the
          // conversation was idle never renders preview bytes under an idle
          // header. The turn facts are re-read from the engine rather than
          // inferred from the event, so a frame reports only what the durable
          // records prove; a repeated payload is not sent again, which keeps a
          // slow reader from accumulating identical header frames. A failing
          // read is not swallowed: it rejects the listener, which ends the
          // watch and the stream, so the browser clears its preview and
          // reconnects instead of keeping stale turn controls.
          if (changes.master) {
            const frame = await this.composedMasterFrame(pinned);
            if (frame !== lastMasterFrame) {
              lastMasterFrame = frame;
              if (!send(frame)) return;
            }
          }
          const before = preview.text;
          const batch = preview.apply(events);
          let shown = before;
          if (batch.current !== null) {
            if (batch.current !== before) {
              sendPreview(batch.current);
              shown = batch.current;
            }
          } else if (
            before !== null &&
            batch.latest !== null &&
            batch.latest !== before
          ) {
            // The final chunk arrived in the commit that completes the
            // message: show it once, before the committed entry below.
            sendPreview(batch.latest);
            shown = batch.latest;
          }
          // Only a batch that committed or replaced entries re-reads the
          // transcript; partial text is not history and must not cost a full
          // read per chunk per reader.
          if (changes.entries)
            for (const entry of await this.transcript(thinking))
              if (!emit(entry, send)) break;
          // Entries are reconciled first; only then does transient text clear.
          if (batch.current === null && shown !== null) sendPreview(null);
        },
      }, { overflow: "after-cursor" });
    }

    if (url.pathname !== "/events")
      return new Response("not found", { status: 404 });
    const stream = await this.harness.session().events();
    return sessionEventResponse(stream, {
      snapshot: (send) => {
        send(rawSessionFrame({ type: "snapshot", snapshot: stream.snapshot }));
      },
      events: async (events, send) => {
        // A bounded response ends once its queue passes the mark; stop
        // producing rather than stringifying events no one will receive.
        for (const event of events) if (!send(rawSessionFrame(event))) break;
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
   */
  async ask(
    prompt: string,
    operationId?: string,
    app?: AppScope,
  ): Promise<AskResult> {
    await this.requireIdentity();
    const envelope = await this.admitOperation(prompt, operationId, app);
    const result = await this.harness.prompt(envelope.text, {
      operationId: envelope.operationId,
      entry: this.inputEntry(envelope),
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
  }

  /**
   * Submit without waiting: the board's send is queued, not a blocking turn.
   *
   * `app` is the host-verified scope of this turn; omit it for Home. The
   * operation id is bound to the text and scope it was first admitted with, so
   * a retried send with the same id replays that operation and one that changes
   * either is refused before the engine is asked.
   */
  async submit(
    text: string,
    operationId: string,
    app?: AppScope,
  ): Promise<boolean> {
    await this.requireIdentity();
    const envelope = await this.admitOperation(text, operationId, app);
    const receipt = await this.harness.submit(envelope.text, {
      operationId: envelope.operationId,
      entry: this.inputEntry(envelope),
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
    return this.operationScopes.admit(
      app === undefined
        ? { operationId: id, text }
        : { operationId: id, text, app },
      await this.engineEvidence(id),
    );
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
  private inputEntry(envelope: OperationEnvelope): {
    kind: string;
    data: JsonObject;
  } {
    const data: JsonObject = { messageId: envelope.operationId };
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
  } | null> {
    try {
      const context = this.options.threadContext
        ? this.validatedThreadContext(await this.options.threadContext())
        : null;
      return { context, frame: await this.composedMasterFrame(context) };
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
  ): Promise<string> {
    const status = await this.state();
    return `event: master\ndata: ${JSON.stringify(
      masterState(
        status.model,
        status,
        context?.conversationId ?? null,
        context?.metadata,
      ),
    )}\n\n`;
  }

  /**
   * The operation-scoped identity of the model request running right now. It
   * comes from the engine's own running generation checkpoint (task id +
   * transcript cutoff), so provider retries and a crash recovery reuse the
   * same funding key while a tool round gets a new one. No running generation,
   * or more than one, is a refusal — never a guessed key.
   */
  private generationIdentity(): GenerationIdentity | null {
    // The Workers SQL storage exposes the same `exec(...).toArray()` surface;
    // the adapter's narrower type keeps its own casts out of the session.
    return readGenerationIdentity(this.ctx.storage.sql as unknown as TaskSql);
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
    return { ...operationStatus(pending), model };
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
