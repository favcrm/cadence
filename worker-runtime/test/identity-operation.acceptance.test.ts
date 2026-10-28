import { Database } from "bun:sqlite";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "bun:test";
import type {
  Api,
  AssistantMessage,
  Message,
  Model,
  Provider,
} from "@earendil-works/pi-ai";
import { createModels } from "@earendil-works/pi-ai/models";
import { createAssistantMessageEventStream } from "@earendil-works/pi-ai/utils/event-stream";
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import {
  createRegistry,
  Harness,
  MemoryStorage,
} from "@earendil-works/pi-durable";
import { assistantIdentityText } from "../src/assistant-identity.ts";
import type { AssistantIdentityContext, RuntimeTool } from "../src/contract.ts";
import { runtimeExtension } from "../src/engine.ts";
import {
  guardModelRequests,
  identityContextExtension,
} from "../src/identity-context.ts";
import {
  IdentitySnapshots,
  type IdentitySnapshotSql,
} from "../src/identity-snapshots.ts";

/**
 * The private runtime's identity behavior over the real engine.
 *
 * The SDK opens a real `Harness` over `MemoryStorage` with an offline scripted
 * provider: the generation task, tool round, handover, retry, new prompt and
 * model-call guard are the actual engine path; only the assistant reply is
 * scripted. This is the seam the runtime's identity hook, guard and snapshot
 * store run at, not a reimplementation of them.
 */

const AVATAR_LOOKS = [
  "builder",
  "guard",
  "bookkeeper",
  "content",
  "designer",
  "reception",
  "operations",
  "support",
  "tester",
  "robot",
  "custom",
] as const;

const IDENTITY_HEADING = "## Your private assistant profile";

const MODEL: Model<Api> = {
  id: "scripted",
  name: "Scripted",
  api: "openai-completions",
  provider: "scripted",
  baseUrl: "https://scripted.invalid",
  input: ["text"],
  reasoning: false,
  contextWindow: 128_000,
  maxTokens: 8_192,
  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
};

const USAGE: AssistantMessage["usage"] = {
  input: 0,
  output: 0,
  cacheRead: 0,
  cacheWrite: 0,
  totalTokens: 0,
  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
};

function assistantReply(
  content: AssistantMessage["content"],
  stopReason: AssistantMessage["stopReason"],
  errorMessage?: string,
): AssistantMessage {
  return {
    role: "assistant",
    content,
    api: "openai-completions",
    provider: "scripted",
    model: "scripted",
    usage: USAGE,
    stopReason,
    timestamp: Date.now(),
    ...(errorMessage === undefined ? {} : { errorMessage }),
  };
}

function messageText(message: Message): string {
  if (typeof message.content === "string") return message.content;
  return message.content
    .flatMap((part) => (part.type === "text" ? [part.text] : []))
    .join("");
}

function identitySystemOf(messages: readonly Message[]): string {
  return messages
    .filter(
      (message) =>
        message.role === "system" &&
        (messageText(message).includes(IDENTITY_HEADING) ||
          messageText(message).includes("## Company agent identity")),
    )
    .map(messageText)
    .join("\n");
}

function lastUserText(messages: readonly Message[]): string {
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index]!;
    if (message.role === "user") return messageText(message);
  }
  return "";
}

const messageOf = (error: unknown): string =>
  error instanceof Error ? error.message : String(error);

type Observation = {
  user: string;
  identitySystem: string;
  hasToolResult: boolean;
  messages: readonly Message[];
};

/**
 * A provider-shaped, offline `Models` instance: what the harness's real
 * generation task calls through `Models.streamSimple`. The scripted reply only
 * chooses the assistant message, never the request or tool execution path.
 */
function scriptedModels(
  reply: (request: {
    user: string;
    hasToolResult: boolean;
    call: number;
  }) => AssistantMessage,
) {
  const observations: Observation[] = [];
  const respond = (context: { messages: readonly Message[] }) => {
    const observation: Observation = {
      user: lastUserText(context.messages),
      identitySystem: identitySystemOf(context.messages),
      hasToolResult: context.messages.some(
        (message) => message.role === "toolResult",
      ),
      messages: context.messages,
    };
    observations.push(observation);
    const stream = createAssistantMessageEventStream();
    const message = reply({
      user: observation.user,
      hasToolResult: observation.hasToolResult,
      call: observations.length,
    });
    queueMicrotask(() => stream.end(message));
    return stream;
  };
  const provider: Provider = {
    id: "scripted",
    name: "Scripted",
    auth: {
      apiKey: {
        name: "Scripted",
        resolve: async () => ({ auth: { apiKey: "scripted" } }),
      },
    },
    getModels: () => [MODEL],
    stream: (_model, context) => respond(context),
    streamSimple: (_model, context) => respond(context),
  };
  const models = createModels();
  models.setProvider(provider);
  return { models, observations };
}

/** `node:sqlite`-shaped adapter over Bun's real SQLite for these tests only. */
function sqlStore(database: Database): IdentitySnapshotSql {
  return {
    exec<T = Record<string, unknown>>(
      query: string,
      ...bindings: (string | number | null)[]
    ) {
      if (/^\s*(SELECT|PRAGMA)/i.test(query)) {
        const rows = database.prepare(query).all(...bindings) as unknown as T[];
        return { toArray: () => rows };
      }
      database.prepare(query).run(...bindings);
      return { toArray: () => [] as T[] };
    },
  };
}

const directories: string[] = [];
afterEach(() => {
  for (const directory of directories)
    rmSync(directory, { recursive: true, force: true });
  directories.length = 0;
});

function sessionDatabase(): { database: Database; path: string } {
  const directory = mkdtempSync(join(tmpdir(), "cadence-identity-"));
  directories.push(directory);
  const path = join(directory, "session.sqlite");
  return { database: new Database(path), path };
}

type Profile = {
  name: string | null;
  tone: string | null;
  userName: string | null;
  preferences: string | null;
  look: string;
  revision: number;
};

/** The host's stand-in canonical profile: a tool mutates it, the load reads it. */
function profileHost() {
  const profile: Profile = {
    name: null,
    tone: null,
    userName: null,
    preferences: "concise",
    look: "builder",
    revision: 0,
  };
  const context = async (): Promise<AssistantIdentityContext> => ({
    scope: "personal",
    profile: {
      identity: {
        name: profile.name,
        tone: profile.tone,
        look: profile.look,
        revision: profile.revision,
      },
      bootstrap_state: "pending",
      user_name: profile.userName,
      preferences: profile.preferences,
    },
    avatarLooks: AVATAR_LOOKS,
  });
  const tool: RuntimeTool<{ name: string; tone?: string }> = {
    name: "profile_update",
    description: "Record the user's agreed assistant profile.",
    parameters: {
      type: "object",
      properties: {
        name: { type: "string" },
        tone: { type: "string" },
      },
      required: ["name"],
      additionalProperties: false,
    },
    replay: "unsafe",
    async execute(input) {
      profile.name = input.name;
      profile.tone = input.tone ?? profile.tone;
      profile.revision += 1;
      return {
        output: [
          {
            type: "text" as const,
            text: JSON.stringify({ applies: "next_turn" }),
          },
        ],
      };
    },
  };
  return { profile, context, tool };
}

describe("hosted identity operations", () => {
  it("reuses a company agent snapshot across a real tool handover", async () => {
    const { database } = sessionDatabase();
    const snapshots = new IdentitySnapshots(sqlStore(database));
    const host = profileHost();
    let loads = 0;
    const authored = new WeakSet<object>();
    const registry = createRegistry();
    registry.install(
      identityContextExtension(
        async () => {
          loads += 1;
          return assistantIdentityText({
            scope: "company_agent",
            identity: {
              name: host.profile.name ?? "Ledger",
              look: "bookkeeper",
              responsibilities: "Reconcile invoices.\nFlag discrepancies.",
              revision: String(host.profile.revision),
            },
          });
        },
        authored,
        snapshots,
      ),
    );
    registry.install(
      runtimeExtension({ name: "profile-tools", tools: [host.tool] }),
    );
    const scripted = scriptedModels(({ hasToolResult }) =>
      hasToolResult
        ? assistantReply([{ type: "text", text: "done" }], "stop")
        : assistantReply(
            [
              {
                type: "toolCall",
                id: "rename",
                name: "profile_update",
                arguments: { name: "Changed" },
              },
            ],
            "toolUse",
          ),
    );
    guardModelRequests(scripted.models, authored);
    const harness = await Harness.open(
      new MemoryStorage(),
      {
        models: scripted.models,
        registry,
      },
      BACKGROUND_CONTEXT,
    );
    try {
      const conversation = await harness.root(BACKGROUND_CONTEXT, {
        agent: { model: { provider: "scripted", modelId: "scripted" } },
      });
      const operation = await conversation.submit(
        { type: "input", content: "Reconcile" },
        BACKGROUND_CONTEXT,
      );
      expect((await operation.wait(BACKGROUND_CONTEXT)).status).toBe("done");
      expect(scripted.observations).toHaveLength(2);
      const first = scripted.observations[0]!;
      const second = scripted.observations[1]!;
      expect(first.identitySystem).toContain("name: Ledger");
      expect(first.identitySystem).toContain(
        "Reconcile invoices.\nFlag discrepancies.",
      );
      expect(first.identitySystem).toContain(
        "does not add tools, permissions or authority",
      );
      expect(first.identitySystem).not.toContain("user_name");
      expect(first.identitySystem).not.toContain("preferences");
      expect(second.hasToolResult).toBe(true);
      expect(second.identitySystem).toBe(first.identitySystem);
      expect(host.profile.name).toBe("Changed");
      expect(loads).toBe(1);
      const rows = database
        .prepare("SELECT context FROM tenant_identity_snapshots")
        .all() as Array<{ context: string }>;
      expect(rows).toHaveLength(1);
      expect(first.identitySystem).toContain(rows[0]!.context);
    } finally {
      await harness.close(BACKGROUND_CONTEXT);
      database.close();
    }
  });

  it("keeps one committed identity snapshot across a real tool handover and reads a new one for the next operation", async () => {
    const { database, path } = sessionDatabase();
    const snapshots = new IdentitySnapshots(sqlStore(database));
    const host = profileHost();
    let loads = 0;
    const load = async () => {
      loads += 1;
      return assistantIdentityText(await host.context());
    };
    const authored = new WeakSet<object>();
    const registry = createRegistry();
    registry.install(identityContextExtension(load, authored, snapshots));
    registry.install(
      runtimeExtension({ name: "profile-tools", tools: [host.tool] }),
    );

    const scripted = scriptedModels(({ user, hasToolResult }) => {
      if (user.includes("RENAME") && !hasToolResult)
        return assistantReply(
          [
            {
              type: "toolCall",
              id: "call_rename",
              name: "profile_update",
              arguments: { name: "Renamed", tone: "curious" },
            },
          ],
          "toolUse",
        );
      return assistantReply([{ type: "text", text: `answer:${user}` }], "stop");
    });
    guardModelRequests(scripted.models, authored);
    const harness = await Harness.open(
      new MemoryStorage(),
      {
        models: scripted.models,
        registry,
        settings: { retry: { baseDelayMs: 0 } },
      },
      BACKGROUND_CONTEXT,
    );
    try {
      const conversation = await harness.root(BACKGROUND_CONTEXT, {
        agent: { model: { provider: "scripted", modelId: "scripted" } },
      });
      const first = await conversation.submit(
        { type: "input", content: "RENAME" },
        BACKGROUND_CONTEXT,
      );
      expect((await first.wait(BACKGROUND_CONTEXT)).status).toBe("done");
      expect(scripted.observations).toHaveLength(2);
      const beforeTool = scripted.observations[0]!;
      const afterTool = scripted.observations[1]!;
      expect(beforeTool.hasToolResult).toBe(false);
      expect(afterTool.hasToolResult).toBe(true);
      expect(beforeTool.identitySystem).toContain("name: (unset)");
      expect(beforeTool.identitySystem).toContain("tone: (unset)");
      expect(beforeTool.identitySystem).toContain("look: builder");
      expect(beforeTool.identitySystem).toContain(
        "user_name (how you address this user): (unset)",
      );

      // The host committed the change during the tool round, but the second
      // generation task for the SAME operation still receives the snapshot.
      expect(afterTool.identitySystem).toBe(beforeTool.identitySystem);
      expect(afterTool.identitySystem).not.toContain("name: Renamed");
      expect(afterTool.identitySystem).not.toContain("Avery");
      expect(host.profile.name).toBe("Renamed");
      expect(host.profile.revision).toBe(1);
      expect(loads, "one canonical load per operation").toBe(1);

      const second = await conversation.submit(
        { type: "input", content: "SECOND" },
        BACKGROUND_CONTEXT,
      );
      expect((await second.wait(BACKGROUND_CONTEXT)).status).toBe("done");
      expect(scripted.observations).toHaveLength(3);
      const next = scripted.observations[2]!;
      expect(next.identitySystem).toContain("name: Renamed");
      expect(next.identitySystem).toContain("tone: curious");
      expect(next.identitySystem).toContain("look: builder");
      expect(next.identitySystem).not.toBe(beforeTool.identitySystem);
      expect(loads, "the new operation loads the committed profile").toBe(2);

      // Insert-once per operation root, with the preserved table and rows.
      const rows = database
        .prepare(
          "SELECT run_id, context FROM tenant_identity_snapshots ORDER BY rowid",
        )
        .all() as Array<{ run_id: string; context: string }>;
      expect(rows).toHaveLength(2);
      expect(rows[0]!.context).toContain("name: (unset)");
      expect(rows[1]!.context).toContain("name: Renamed");
      expect(beforeTool.identitySystem).toContain(rows[0]!.context);
      expect(afterTool.identitySystem).toContain(rows[0]!.context);
      expect(next.identitySystem).toContain(rows[1]!.context);
      expect(rows[0]!.run_id).not.toBe(rows[1]!.run_id);

      // A new store instance on a new SQLite connection returns the stored
      // snapshot; a stored snapshot never reloads the canonical profile.
      const reopened = new Database(path);
      try {
        const restored = await new IdentitySnapshots(
          sqlStore(reopened),
        ).readOrInsert(
          rows[0]!.run_id,
          () =>
            Promise.reject(
              new Error("a stored snapshot must not reload the profile"),
            ),
          Date.now(),
        );
        expect(restored).toBe(rows[0]!.context);
      } finally {
        reopened.close();
      }

      // Request-only context: the transcript has the real entries but no
      // identity system message or prompt copy.
      const entries = await conversation.entries(
        { order: "ascending" },
        100,
        undefined,
        BACKGROUND_CONTEXT,
      );
      expect(entries.items.length).toBeGreaterThan(0);
      expect(JSON.stringify(entries)).toContain("RENAME");
      expect(JSON.stringify(entries)).not.toContain(IDENTITY_HEADING);
      const leaked = entries.items.flatMap((entry) =>
        (entry.model ?? []).filter(
          (message) =>
            message.role === "system" &&
            messageText(message).includes(IDENTITY_HEADING),
        ),
      );
      expect(leaked).toEqual([]);
    } finally {
      await harness.close(BACKGROUND_CONTEXT);
      database.close();
    }
  });

  it("reuses the operation snapshot across a real provider retry", async () => {
    const { database } = sessionDatabase();
    const snapshots = new IdentitySnapshots(sqlStore(database));
    const host = profileHost();
    let loads = 0;
    const load = async () => {
      loads += 1;
      return assistantIdentityText(await host.context());
    };
    const authored = new WeakSet<object>();
    const registry = createRegistry();
    registry.install(identityContextExtension(load, authored, snapshots));
    const scripted = scriptedModels(({ call }) =>
      call === 1
        ? assistantReply(
            [{ type: "text", text: "" }],
            "error",
            "503 Service Unavailable",
          )
        : assistantReply([{ type: "text", text: "recovered" }], "stop"),
    );
    guardModelRequests(scripted.models, authored);
    const harness = await Harness.open(
      new MemoryStorage(),
      {
        models: scripted.models,
        registry,
        settings: { retry: { baseDelayMs: 0 } },
      },
      BACKGROUND_CONTEXT,
    );
    try {
      const conversation = await harness.root(BACKGROUND_CONTEXT, {
        agent: { model: { provider: "scripted", modelId: "scripted" } },
      });
      const submission = await conversation.submit(
        { type: "input", content: "RETRY" },
        BACKGROUND_CONTEXT,
      );
      expect((await submission.wait(BACKGROUND_CONTEXT)).status).toBe("done");
      expect(scripted.observations).toHaveLength(2);
      // The retried attempt is the same operation: same run root, same
      // authored snapshot, no second canonical load.
      expect(loads).toBe(1);
      expect(scripted.observations[1]!.identitySystem).toBe(
        scripted.observations[0]!.identitySystem,
      );
      expect(scripted.observations[1]!.identitySystem).toContain(
        IDENTITY_HEADING,
      );
      expect(
        database
          .prepare("SELECT COUNT(*) AS n FROM tenant_identity_snapshots")
          .get(),
      ).toEqual({ n: 1 });
    } finally {
      await harness.close(BACKGROUND_CONTEXT);
      database.close();
    }
  });

  it("does not call the provider without the authored identity object and keeps compaction completeSimple", async () => {
    // A canonical load that refuses (here: a malformed host context) fails the
    // operation before the provider and writes no snapshot.
    const { database } = sessionDatabase();
    const brokenSnapshots = new IdentitySnapshots(sqlStore(database));
    const brokenAuthored = new WeakSet<object>();
    const brokenRegistry = createRegistry();
    brokenRegistry.install(
      identityContextExtension(
        async () =>
          assistantIdentityText({
            scope: "personal",
            profile: {
              identity: {
                name: null,
                tone: null,
                look: "dragon",
                revision: 0,
              },
              bootstrap_state: "pending",
              user_name: null,
              preferences: null,
            },
            avatarLooks: AVATAR_LOOKS,
          }),
        brokenAuthored,
        brokenSnapshots,
      ),
    );
    const broken = scriptedModels(() =>
      assistantReply([{ type: "text", text: "must not run" }], "stop"),
    );
    guardModelRequests(broken.models, brokenAuthored);
    const reports: unknown[] = [];
    const brokenHarness = await Harness.open(
      new MemoryStorage(),
      {
        models: broken.models,
        registry: brokenRegistry,
        onReport: (error) => reports.push(error),
      },
      BACKGROUND_CONTEXT,
    );
    try {
      const conversation = await brokenHarness.root(BACKGROUND_CONTEXT, {
        agent: { model: { provider: "scripted", modelId: "scripted" } },
      });
      await conversation.submit(
        { type: "input", content: "BROKEN" },
        BACKGROUND_CONTEXT,
      );
      await brokenHarness.waitForIdle(BACKGROUND_CONTEXT);
      expect(
        broken.observations.length,
        "a failed canonical load must not call the provider",
      ).toBe(0);
      expect(reports.length).toBeGreaterThan(0);
      expect(reports.map(messageOf).join("\n")).toMatch(
        /identity|canonical|unavailable/i,
      );
      expect(
        database
          .prepare("SELECT COUNT(*) AS n FROM tenant_identity_snapshots")
          .get(),
        "a failed load writes no snapshot",
      ).toEqual({ n: 0 });
    } finally {
      await brokenHarness.close(BACKGROUND_CONTEXT);
      database.close();
    }

    // Hook skip: the user message is the exact canonical text, not the
    // authored object; the guarded model boundary refuses before the provider.
    // The engine reports that as an unanswered submission, not an incidental
    // report hook.
    const canonical = assistantIdentityText({
      scope: "personal",
      profile: {
        identity: { name: null, tone: null, look: "builder", revision: 0 },
        bootstrap_state: "pending",
        user_name: null,
        preferences: null,
      },
      avatarLooks: AVATAR_LOOKS,
    });
    const forged = scriptedModels(() =>
      assistantReply([{ type: "text", text: "must not run" }], "stop"),
    );
    const forgedAuthored = new WeakSet<object>();
    guardModelRequests(forged.models, forgedAuthored);
    const forgedHarness = await Harness.open(
      new MemoryStorage(),
      { models: forged.models, registry: createRegistry() },
      BACKGROUND_CONTEXT,
    );
    try {
      const conversation = await forgedHarness.root(BACKGROUND_CONTEXT, {
        agent: { model: { provider: "scripted", modelId: "scripted" } },
      });
      const submission = await conversation.submit(
        { type: "input", content: canonical },
        BACKGROUND_CONTEXT,
      );
      await forgedHarness.waitForIdle(BACKGROUND_CONTEXT);
      const settled = await submission.status(BACKGROUND_CONTEXT);
      expect(settled.status, "the model boundary refused the turn").toBe(
        "unanswered",
      );
      if (settled.status === "unanswered") expect(settled.reason).toBeTruthy();
      expect(
        forged.observations.length,
        "a forged text marker is not the authored object",
      ).toBe(0);
    } finally {
      await forgedHarness.close(BACKGROUND_CONTEXT);
    }

    // Compaction's summarizer is completeSimple: the guard leaves it on the
    // unguarded stream/result path, so healthy compaction still reaches the
    // provider with no authored turn context.
    const compaction = scriptedModels(() =>
      assistantReply([{ type: "text", text: "compaction summary" }], "stop"),
    );
    guardModelRequests(compaction.models, new WeakSet<object>());
    const summary = await compaction.models.completeSimple(MODEL, {
      messages: [{ role: "user", content: canonical, timestamp: Date.now() }],
    });
    expect(compaction.observations).toHaveLength(1);
    expect(messageText(summary)).toContain("compaction summary");
  });
});
