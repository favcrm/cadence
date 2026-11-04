import { Database } from "bun:sqlite";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it, mock } from "bun:test";
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
import type {
  AssistantIdentityContext,
  RuntimeTool,
} from "../src/contract.ts";
import { runtimeExtension } from "../src/engine.ts";
import {
  guardModelRequests,
  identityContextExtension,
} from "../src/identity-context.ts";
import { IdentitySnapshots } from "../src/identity-snapshots.ts";
import { OperationScopes } from "../src/operation-scope.ts";

/**
 * CAD-2159 acceptance check. Authored independently of the implementer.
 * Copy this file unedited. Do not weaken a refusal or replace the guard.
 *
 * Two checks, both on the runtime's own paths:
 *
 * 1. `HostedSession.submit`/`ask` render the host's committed context through
 *    the runtime's `requireIdentity`, so a malformed or oversized
 *    `agent_profile` is refused at admission: nothing reaches the engine, no
 *    operation id is reserved and no turn runs. The declared 8,000 UTF-16 code
 *    unit limit is pinned in both directions, so a guard that refuses every
 *    profile cannot pass. The at-limit case uses 4,000 astral characters, which
 *    are 8,000 UTF-16 code units but only 4,000 code points; a guard that
 *    counted code points would admit the 8,001-unit case and fail here. A
 *    sparse skill array is refused too: `Array.prototype.some` skips holes, so
 *    a hole must be rejected explicitly or a profile with an undeclared skill
 *    slot would be admitted.
 *
 * 2. Over the real engine (`Harness` + `identityContextExtension` +
 *    `guardModelRequests` + `IdentitySnapshots`), a valid profile's full 8,000
 *    code units of instructions, declared skills and revision reach the model;
 *    a revision committed by a tool mid-operation does not change the running
 *    operation's snapshot, and the next operation reads the new revision.
 */

// `hosted-session.ts` imports its Durable Object base from the platform
// module, which `bun test` cannot resolve. The stub carries no behaviour this
// check relies on; the runtime's own methods under test are the real ones.
mock.module("cloudflare:workers", () => ({
  DurableObject: class {
    constructor(
      readonly ctx: unknown,
      readonly env: unknown,
    ) {}
  },
}));

const MALFORMED = /committed assistant identity context is malformed/i;

const IDENTITY_HEADING = "## Agent profile";

/** The host's context as supplied for one admission; malformed on purpose. */
function profileContext(profile: unknown): AssistantIdentityContext {
  return {
    scope: "agent_profile",
    profile,
  } as unknown as AssistantIdentityContext;
}

/** The narrow SQL surface the session, its snapshot store and its guard read. */
type SessionSql = {
  exec<T = Record<string, unknown>>(
    query: string,
    ...bindings: (string | number | null)[]
  ): { toArray(): T[] };
};

/** `node:sqlite`-shaped adapter over Bun's real SQLite for this check only. */
function sqlStore(database: Database): SessionSql {
  return {
    exec<T = Record<string, unknown>>(
      query: string,
      ...bindings: (string | number | null)[]
    ) {
      if (/^\s*(SELECT|PRAGMA|WITH)/i.test(query)) {
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

function sessionDatabase(): Database {
  const directory = mkdtempSync(join(tmpdir(), "cadence-agent-profile-"));
  directories.push(directory);
  return new Database(join(directory, "session.sqlite"));
}

type Submission = { readonly text: string; readonly operationId: string };

type EngineStub = {
  submit(
    text: string,
    options: {
      operationId: string;
      entry?: { kind: string; data: Record<string, unknown> };
    },
  ): Promise<{ accepted: boolean; operationId: string }>;
  prompt(
    text: string,
    options: {
      operationId: string;
      entry?: { kind: string; data: Record<string, unknown> };
    },
  ): Promise<{
    status: "done" | "unanswered";
    text: string | undefined;
    operationId: string;
  }>;
};

/**
 * The real `HostedSession` prototype over the session's own SQLite and an
 * engine stub, the same seam the app-scope acceptance uses. `options.identity`
 * is exactly what the constructor stores and `requireIdentity` reads; the
 * stub only records what reached the engine.
 */
async function sessionWithIdentity(
  identity: () => Promise<AssistantIdentityContext>,
  engine: EngineStub,
  operationScopes: OperationScopes,
  sql: SessionSql,
) {
  // Static import would be hoisted above the `cloudflare:workers` mock and
  // fail to resolve; the platform module does not exist under `bun test`.
  const { HostedSession } = await import("../src/hosted-session.ts");
  return Object.assign(Object.create(HostedSession.prototype), {
    options: { identity },
    operationScopes,
    ctx: { storage: { sql } },
    harness: { pending: async () => [], ...engine },
  }) as unknown as {
    submit(text: string, operationId: string): Promise<boolean>;
    ask(
      prompt: string,
      operationId?: string,
    ): Promise<{ status: "done" | "unanswered"; operationId?: string }>;
  };
}

describe("agent profile identity refusal", () => {
  it("refuses a malformed or oversized agent profile before admitting the operation", async () => {
    const database = sessionDatabase();
    const submissions: Submission[] = [];
    const prompts: string[] = [];
    let current: unknown = null;
    const session = await sessionWithIdentity(
      async () => current as AssistantIdentityContext,
      {
        submit: async (text, options) => {
          submissions.push({ text, operationId: options.operationId });
          return { accepted: true, operationId: options.operationId };
        },
        prompt: async (text, options) => {
          prompts.push(text);
          return {
            status: "done",
            text: "answer",
            operationId: options.operationId,
          };
        },
      },
      new OperationScopes(sqlStore(database)),
      sqlStore(database),
    );
    try {
      const base = {
        name: "Ledger",
        role: "bookkeeper",
        instructions: "Reconcile invoices.\nFlag discrepancies.",
        skills: ["invoices", "reconciliation"],
        revision: 3,
      };

      // Exactly the declared limit is admitted. 4,000 astral characters are
      // 8,000 UTF-16 code units, so the limit is counted the way the host
      // stores it, not in code points.
      const atLimit = "🚀".repeat(4000);
      expect(
        atLimit.length,
        "4,000 astral characters are 8,000 UTF-16 code units",
      ).toBe(8000);
      current = profileContext({ ...base, instructions: atLimit });
      expect(await session.submit("at the limit", "op-limit")).toBe(true);

      // A profile with no declared skills and ordinary allowed whitespace is
      // valid context too; an over-strict guard must not pass this check by
      // refusing a well-formed profile.
      current = profileContext({
        name: "Ledger",
        role: "bookkeeper",
        instructions: "line one\r\nline two\tend",
        skills: [],
        revision: 1,
      });
      expect(await session.submit("allowed whitespace", "op-tabs")).toBe(true);

      expect(submissions.map((entry) => entry.operationId)).toEqual([
        "op-limit",
        "op-tabs",
      ]);

      let refusals = 0;
      const refuse = async (
        profile: unknown,
        why: string,
        method: "submit" | "ask" = "submit",
      ) => {
        current = profileContext(profile);
        refusals += 1;
        const operationId = `op-refused-${refusals}`;
        const before = submissions.length + prompts.length;
        const failure = await (
          method === "submit"
            ? session.submit("must not run", operationId)
            : session.ask("must not run", operationId)
        ).then(
          () => null,
          (error: unknown) => error,
        );
        expect(failure, why).toBeInstanceOf(Error);
        expect(
          failure instanceof Error ? failure.message : String(failure),
          why,
        ).toMatch(MALFORMED);
        expect(
          submissions.length + prompts.length,
          `${why}: nothing reaches the engine`,
        ).toBe(before);
      };

      // The skill list is an array of declared names: a hole in it is not a
      // skill, and `Array.prototype.some` would skip it.
      const holesOnly = new Array<string>(3);
      const oneHole = ["invoices", "reconciliation"];
      oneHole.length = 3;

      await refuse(undefined, "a profile is required");
      await refuse(null, "a null profile is not a profile");
      await refuse([], "an array is not a profile");
      await refuse({ ...base, name: "" }, "an empty name");
      await refuse({ ...base, name: "   " }, "a blank name");
      await refuse({ ...base, name: "a".repeat(121) }, "a name over 120");
      await refuse(
        { ...base, name: "Led\u0000ger" },
        "a control character in the name",
      );
      await refuse({ ...base, role: "" }, "an empty role");
      await refuse({ ...base, role: "  " }, "a blank role");
      await refuse({ ...base, role: "a".repeat(81) }, "a role over 80");
      await refuse(
        { ...base, role: "book\u001fkeeper" },
        "a control character in the role",
      );
      await refuse({ ...base, instructions: "" }, "empty instructions");
      await refuse(
        { ...base, instructions: "  \n\t  " },
        "blank instructions",
      );
      await refuse(
        { ...base, instructions: "ok\u0000" },
        "a control character in the instructions",
      );
      await refuse(
        { ...base, instructions: 7 },
        "instructions that are not text",
      );
      await refuse(
        { ...base, instructions: `${atLimit}a` },
        "one UTF-16 code unit over the instruction limit",
      );
      await refuse(
        { ...base, instructions: "a".repeat(8001) },
        "an oversized instruction string refused on ask",
        "ask",
      );
      await refuse({ ...base, revision: 0 }, "revision 0");
      await refuse({ ...base, revision: 1.5 }, "a fractional revision");
      await refuse({ ...base, revision: "3" }, "a string revision");
      await refuse(
        { ...base, revision: Number.MAX_SAFE_INTEGER + 1 },
        "an unsafe revision",
      );
      await refuse({ ...base, revision: Number.NaN }, "a NaN revision");
      await refuse(
        {
          ...base,
          skills: Array.from({ length: 33 }, (_, index) => `skill-${index}`),
        },
        "more than 32 declared skills",
      );
      await refuse(
        { ...base, skills: "invoices" },
        "skills that are not a list",
      );
      await refuse({ ...base, skills: [""] }, "an empty skill");
      await refuse({ ...base, skills: ["  "] }, "a blank skill");
      await refuse({ ...base, skills: [7] }, "a skill that is not text");
      await refuse(
        { ...base, skills: [undefined] },
        "an undefined skill",
      );
      await refuse(
        { ...base, skills: ["a".repeat(81)] },
        "a skill over 80",
      );
      await refuse(
        { ...base, skills: ["ok\u0000"] },
        "a control character in a skill",
      );
      await refuse(
        { ...base, skills: holesOnly },
        "a skill list that is all holes",
      );
      await refuse(
        { ...base, skills: oneHole },
        "a skill list with one hole",
      );

      // A refused admission reserves nothing: only the two admitted
      // operations have a binding, and the engine saw only those two.
      const rows = database
        .prepare(
          "SELECT operation_id FROM tenant_operation_scopes ORDER BY operation_id",
        )
        .all() as Array<{ operation_id: string }>;
      expect(rows.map((row) => row.operation_id)).toEqual([
        "op-limit",
        "op-tabs",
      ]);
      expect(prompts).toEqual([]);
    } finally {
      database.close();
    }
  });
});

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
  };
}

function messageText(message: Message): string {
  if (typeof message.content === "string") return message.content;
  return message.content
    .flatMap((part) => (part.type === "text" ? [part.text] : []))
    .join("");
}

function lastUserText(messages: readonly Message[]): string {
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index]!;
    if (message.role === "user") return messageText(message);
  }
  return "";
}

/** The identity system message this operation's request carried. */
function identitySystemOf(messages: readonly Message[]): string {
  return messages
    .filter(
      (message) =>
        message.role === "system" &&
        messageText(message).includes(IDENTITY_HEADING),
    )
    .map(messageText)
    .join("\n");
}

type Observation = {
  user: string;
  identitySystem: string;
  hasToolResult: boolean;
};

/**
 * A provider-shaped, offline `Models` instance: the harness's real generation
 * task calls through `Models.streamSimple`, and only the assistant reply is
 * scripted.
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

describe("agent profile operation snapshot", () => {
  it("keeps the running operation's full profile snapshot and gives the next operation the new revision", async () => {
    const database = sessionDatabase();
    const snapshots = new IdentitySnapshots(sqlStore(database));
    // Exactly 8,000 UTF-16 code units, with astral characters at the end.
    const instructions = `${"a".repeat(7990)}🚀🚀🚀🚀🚀`;
    expect(instructions.length).toBe(8000);
    const skills = ["invoices", "reconciliation", "ledger"];
    let revision = 1;
    let loads = 0;
    const load = async () => {
      loads += 1;
      return assistantIdentityText({
        scope: "agent_profile",
        profile: {
          name: "Ledger",
          role: "bookkeeper",
          instructions,
          skills,
          revision,
        },
      });
    };
    const bump: RuntimeTool<{ revision: number }> = {
      name: "profile_bump",
      description: "Commit a new profile revision.",
      parameters: {
        type: "object",
        properties: { revision: { type: "number" } },
        required: ["revision"],
        additionalProperties: false,
      },
      replay: "unsafe",
      async execute(input) {
        revision = input.revision;
        return {
          output: [{ type: "text" as const, text: JSON.stringify({ revision }) }],
        };
      },
    };
    const authored = new WeakSet<object>();
    const registry = createRegistry();
    registry.install(identityContextExtension(load, authored, snapshots));
    registry.install(runtimeExtension({ name: "profile-tools", tools: [bump] }));
    const scripted = scriptedModels(({ user, hasToolResult }) => {
      if (user.includes("BUMP") && !hasToolResult)
        return assistantReply(
          [
            {
              type: "toolCall",
              id: "bump",
              name: "profile_bump",
              arguments: { revision: 2 },
            },
          ],
          "toolUse",
        );
      return assistantReply([{ type: "text", text: "done" }], "stop");
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
        { type: "input", content: "BUMP" },
        BACKGROUND_CONTEXT,
      );
      expect((await first.wait(BACKGROUND_CONTEXT)).status).toBe("done");
      expect(scripted.observations).toHaveLength(2);
      const beforeTool = scripted.observations[0]!;
      const afterTool = scripted.observations[1]!;

      // The full declared context reaches the model: all 8,000 code units,
      // the declared skills and the revision, with no capability grant.
      expect(beforeTool.identitySystem).toContain(IDENTITY_HEADING);
      expect(beforeTool.identitySystem).toContain(instructions);
      expect(beforeTool.identitySystem).toContain(
        `declared skills: ${JSON.stringify(skills)}`,
      );
      expect(beforeTool.identitySystem).toContain("revision: 1");
      expect(beforeTool.identitySystem).toContain(
        "not caller identity, permissions or tool grants",
      );

      // The tool committed revision 2 during the operation; the same
      // operation's second generation task still sees its snapshot.
      expect(revision).toBe(2);
      expect(afterTool.hasToolResult).toBe(true);
      expect(afterTool.identitySystem).toBe(beforeTool.identitySystem);
      expect(afterTool.identitySystem).not.toContain("revision: 2");
      expect(loads, "one canonical load per operation").toBe(1);

      const second = await conversation.submit(
        { type: "input", content: "SECOND" },
        BACKGROUND_CONTEXT,
      );
      expect((await second.wait(BACKGROUND_CONTEXT)).status).toBe("done");
      expect(scripted.observations).toHaveLength(3);
      const next = scripted.observations[2]!;
      expect(next.identitySystem).toContain("revision: 2");
      expect(next.identitySystem).toContain(instructions);
      expect(next.identitySystem).toContain(
        `declared skills: ${JSON.stringify(skills)}`,
      );
      expect(next.identitySystem).not.toBe(beforeTool.identitySystem);
      expect(loads, "the next operation loads the committed profile").toBe(2);

      // One insert-once snapshot per operation root, holding the full text.
      const rows = database
        .prepare(
          "SELECT run_id, context FROM tenant_identity_snapshots ORDER BY rowid",
        )
        .all() as Array<{ run_id: string; context: string }>;
      expect(rows).toHaveLength(2);
      expect(rows[0]!.context).toContain("revision: 1");
      expect(rows[0]!.context).toContain(instructions);
      expect(rows[1]!.context).toContain("revision: 2");
      expect(beforeTool.identitySystem).toContain(rows[0]!.context);
      expect(afterTool.identitySystem).toContain(rows[0]!.context);
      expect(next.identitySystem).toContain(rows[1]!.context);
      expect(rows[0]!.run_id).not.toBe(rows[1]!.run_id);
    } finally {
      await harness.close(BACKGROUND_CONTEXT);
      database.close();
    }
  });
});
