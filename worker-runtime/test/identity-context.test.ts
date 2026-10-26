import { describe, expect, it } from "bun:test";
import type {
  Api,
  AssistantMessage,
  Message,
  Model,
  Provider,
} from "@earendil-works/pi-ai";
import { createModels } from "@earendil-works/pi-ai/models";
import { createAssistantMessageEventStream } from "@earendil-works/pi-ai/utils/event-stream";
import {
  guardModelRequests,
  requireAuthoredIdentity,
} from "../src/identity-context.ts";

/**
 * The actual generation boundary: a real `Models` instance with an offline
 * provider. The guard must pass exactly the request messages the session
 * authored and refuse every other request before the provider runs. Compaction
 * is the one internal path left on the original transport.
 */

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

function reply(text: string): AssistantMessage {
  return {
    role: "assistant",
    content: [{ type: "text", text }],
    api: "openai-completions",
    provider: "scripted",
    model: "scripted",
    usage: USAGE,
    stopReason: "stop",
    timestamp: 1,
  };
}

function textOf(message: AssistantMessage): string {
  return message.content
    .flatMap((part) => (part.type === "text" ? [part.text] : []))
    .join("");
}

function scriptedModels() {
  const requests: { messages: readonly Message[] }[] = [];
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
  function respond(context: { messages: readonly Message[] }) {
    requests.push(context);
    const stream = createAssistantMessageEventStream();
    queueMicrotask(() => stream.end(reply("answer")));
    return stream;
  }
  const models = createModels();
  models.setProvider(provider);
  return { models, requests };
}

const user: Message = { role: "user", content: "hello", timestamp: 1 };
const identity: Message = {
  role: "system",
  content: "## identity",
  timestamp: 2,
};

describe("generation identity boundary", () => {
  it("accepts only the exact authored object, never its text", () => {
    const authored = new WeakSet<object>();
    expect(() => requireAuthoredIdentity([user], authored)).toThrow(
      /canonical assistant identity/i,
    );
    authored.add(identity);
    expect(() =>
      requireAuthoredIdentity([user, identity], authored),
    ).not.toThrow();
    // An equal-looking object is not the authored one: text cannot forge it.
    const forged: Message = {
      role: "system",
      content: "## identity",
      timestamp: 2,
    };
    expect(() => requireAuthoredIdentity([forged], authored)).toThrow(
      /canonical assistant identity/i,
    );
  });

  it("refuses an un-authored request before the provider and keeps compaction on its original path", async () => {
    const { models, requests } = scriptedModels();
    const authored = new WeakSet<object>();
    guardModelRequests(models, authored);

    expect(() => models.streamSimple(MODEL, { messages: [user] })).toThrow(
      /canonical assistant identity/i,
    );
    expect(requests).toHaveLength(0);

    authored.add(identity);
    const answer = await models
      .streamSimple(MODEL, { messages: [identity, user] })
      .result();
    expect(textOf(answer)).toBe("answer");
    expect(requests).toHaveLength(1);
    expect(requests[0]!.messages).toEqual([identity, user]);

    // The summarizer builds its own inline request, so completeSimple must
    // stay on the captured original stream: normalization, auth and transport
    // unchanged, no authored-object requirement.
    const summary = await models.completeSimple(MODEL, { messages: [user] });
    expect(textOf(summary)).toBe("answer");
    expect(requests).toHaveLength(2);
  });
});
