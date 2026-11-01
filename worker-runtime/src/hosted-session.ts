import { DurableObject } from "cloudflare:workers";
import { createModels } from "@earendil-works/pi-ai/models";
import { createRegistry, Harness } from "@earendil-works/pi-durable";
import { PiHarness, skills } from "agents/harness/pi";
import { Lifecycle } from "agents/lifecycle";
import { createAI } from "agents/models/pi-ai";
import type {
  AskResult,
  GenerationIdentity,
  HostedSessionOptions,
  HostedState,
  PendingOperation,
  ThreadContext,
  ThreadEntry,
} from "./contract.ts";
import { assistantIdentityText } from "./assistant-identity.ts";
import { authorizeExtension, intentGuard, runtimeExtension } from "./engine.ts";
import { readGenerationIdentity, type TaskSql } from "./identity.ts";
import {
  guardModelRequests,
  identityContextExtension,
} from "./identity-context.ts";
import {
  IdentitySnapshots,
  type IdentitySnapshotSql,
} from "./identity-snapshots.ts";
import { masterState } from "./master.ts";
import { defaultModel, meteredBinding, withDefaultModel } from "./model.ts";
import {
  operationalSkills,
  runtimeHealthSkills,
  runtimeProfile,
} from "./profile.ts";
import { sessionEventResponse } from "./session-stream.ts";
import type { ToolAuthority } from "./tool-policy.ts";
import { authorizeTools } from "./tool-policy.ts";
import { BOARD_MESSAGE_KIND } from "./contract.ts";
import { threadEntries } from "./transcript.ts";

/**
 * The last model the platform gateway reported serving for this conversation.
 * Absent until the first call resolves the platform default.
 */
const RESOLVED_MODEL_KEY = "tenant_resolved_model";

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
    const acceptsImages = options.acceptsToolImages === true;
    const model = defaultModel(options.modelAlias, acceptsImages);
    const authority: ToolAuthority = {
      toolRefusal: (name) => options.toolRefusal(name),
      authorizeTool: (name) => options.authorizeTool(name),
    };
    const ai = createAI({
      binding: meteredBinding(
        options,
        () => this.generationIdentity(),
        (served) => this.recordResolvedModel(served),
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
    for (const group of options.tools) {
      this.registry.install(
        runtimeExtension(authorizeTools(group, authority), acceptsImages),
      );
    }
    this.harness = new PiHarness({
      harness: async ({ storage, context, onWake }) => {
        // The bundled skills are engine-owned, so they are wrapped here with
        // the same refusal and live authorization an application tool gets.
        this.registry.install(
          authorizeExtension(
            await skills([operationalSkills, runtimeHealthSkills]),
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
    // entry"; the transcript is the authority, so a batch re-reads it and
    // emits whatever is past `last`.
    if (url.pathname === "/thread-stream") {
      // Raw thinking is an owner view; anything but an explicit owner reader
      // (set by the trusted HTTP adapter) gets none.
      const thinking = url.searchParams.get("reader") === "owner";
      const requested = Number(url.searchParams.get("after") ?? "0");
      let last =
        Number.isSafeInteger(requested) && requested > 0 ? requested : 0;
      // Read the projection now, at this connection's snapshot. A reconnect
      // reads the host's store again, so a connection never pins an older
      // revision/avatar/name over a newer committed edit; an unavailable or
      // malformed projection fails the connection rather than serving a
      // partial header.
      let masterFrame: string;
      try {
        const master = this.options.threadContext
          ? this.validatedThreadContext(await this.options.threadContext())
          : null;
        const model = await this.modelLabel();
        // Serialize the actual composed payload once, before opening a watch
        // or returning SSE 200. Metadata may contain non-JSON values, while
        // reserved fields it cannot override must retain runtime precedence.
        masterFrame = `event: master\ndata: ${JSON.stringify(
          masterState(
            model,
            { running: false, queued: 0 },
            master?.conversationId ?? null,
            master?.metadata,
          ),
        )}\n\n`;
      } catch {
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
      // Capture the watch before reading its snapshot so commits between the
      // initial read and subscription are buffered, not silently missed.
      const stream = await this.harness.session().events();
      function emit(entry: ThreadEntry, send: (frame: string) => void): void {
        if (entry.seq <= last) return;
        send(
          `id: ${entry.seq}\nevent: entry\ndata: ${JSON.stringify(entry)}\n\n`,
        );
        last = entry.seq;
      }
      return sessionEventResponse(stream, {
        snapshot: (send) => {
          send(": connected\n\n");
          // The board flips its link to `live` on a `master` frame and on
          // nothing else, so a stream that never sends one reads
          // "reconnecting" forever — the stale banner then never clears.
          // This object is the master: it answered, and it holds no live
          // native turn, so the honest snapshot is idle. A host projection
          // composes under those runtime fields and cannot replace them.
          send(masterFrame);
          for (const entry of threadEntries(stream.snapshot.entries, thinking))
            emit(entry, send);
        },
        events: async (_events, send) => {
          for (const entry of await this.transcript(thinking))
            emit(entry, send);
        },
      });
    }

    if (url.pathname !== "/events")
      return new Response("not found", { status: 404 });
    const stream = await this.harness.session().events();
    return sessionEventResponse(stream, {
      snapshot: (send) => {
        send(
          `data: ${JSON.stringify({ type: "snapshot", snapshot: stream.snapshot })}\n\n`,
        );
      },
      events: async (events, send) => {
        for (const event of events) send(`data: ${JSON.stringify(event)}\n\n`);
      },
    });
  }

  /** Ask the session and wait for the answer. */
  async ask(prompt: string, operationId?: string): Promise<AskResult> {
    await this.requireIdentity();
    const id = operationId ?? crypto.randomUUID();
    const result = await this.harness.prompt(prompt, {
      operationId: id,
      entry: { kind: BOARD_MESSAGE_KIND, data: { messageId: id } },
    });
    return {
      status: result.status,
      text: result.text,
      operationId: result.operationId,
    };
  }

  /** Map the engine's committed records, including durable input metadata, for the board. */
  async transcript(includeThinking: boolean): Promise<ThreadEntry[]> {
    return threadEntries(
      await this.harness.session().messages(),
      includeThinking,
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
   * Start a fresh context for this conversation. The durable transcript before
   * this point stops being active; the board then has a clean thread even when
   * a stale session is wedged and a normal reload cannot recover it. The caller
   * is the owner-only reset route; this is deliberately not an automatic retry.
   */
  async resetConversation(): Promise<void> {
    await this.harness.session().reset();
  }

  /** Submit without waiting: the board's send is queued, not a blocking turn. */
  async submit(text: string, operationId: string): Promise<boolean> {
    await this.requireIdentity();
    const receipt = await this.harness.submit(text, {
      operationId,
      entry: { kind: BOARD_MESSAGE_KIND, data: { messageId: operationId } },
    });
    return receipt.accepted;
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
    return {
      running: pending.some((operation) => operation.status === "running"),
      queued: pending.filter((operation) => operation.status === "queued")
        .length,
      model,
    };
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
