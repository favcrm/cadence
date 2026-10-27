/** CAD-818 Threads client: request paths, methods and bodies follow the backend contract (DRAFT v1). Fetch is stubbed; no daemon. */
import { ApiError } from "../src/lib/api";
import {
  changeSubscriber,
  createTopic,
  postToTopic,
  predatesThreads,
  readTopic,
  readWorkConversation,
  listWorkConversations,
  requestFromTopic,
} from "../src/features/threads/threadsClient";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

interface Call {
  url: string;
  method: string;
  body: unknown;
}
const calls: Call[] = [];
let reply: { status: number; body: unknown } = { status: 200, body: {} };
(globalThis as unknown as { fetch: unknown }).fetch = async (url: string, init: RequestInit = {}) => {
  calls.push({ url, method: init.method ?? "GET", body: init.body ? JSON.parse(String(init.body)) : undefined });
  return new Response(JSON.stringify(reply.body), { status: reply.status, headers: { "Content-Type": "application/json" } });
};

async function main() {
  // Reads: cursor mode defaults to the newest page; a seq cursor is `before` or `after`, never both.
  reply = { status: 200, body: { conversations: [] } };
  await listWorkConversations({ project: "cadence", agent: "pm", include_system: true });
  equal(calls.at(-1), { url: "/api/work-conversations?project=cadence&agent=pm&include_system=1", method: "GET", body: undefined }, "list query");
  await listWorkConversations();
  equal(calls.at(-1)?.url, "/api/work-conversations", "list with no filters has no query");

  reply = { status: 200, body: { conversation: {}, messages: [], cursor: 0, more_before: false } };
  await readWorkConversation("conv-abc");
  equal(calls.at(-1)?.url, "/api/work-conversations/conv-abc?tail=1&limit=50", "read defaults to the newest page");
  await readWorkConversation("conv-abc", { before: 40, limit: 20 });
  equal(calls.at(-1)?.url, "/api/work-conversations/conv-abc?before=40&limit=20", "read before a seq");
  await readTopic("topic-1", { after: 7 });
  equal(calls.at(-1)?.url, "/api/topics/topic-1?after=7&limit=50", "topic read after a seq");

  // Writes: the operator's board routes, JSON bodies; the client never sends a sender or operator field.
  reply = { status: 200, body: { topic: "topic-1", type: "project", source: "cadence", created: true } };
  const created = await createTopic({ type: "project", source: " cadence " });
  equal(calls.at(-1), { url: "/api/topics", method: "POST", body: { type: "project", source: "cadence" } }, "create trims source");
  equal(created.topic, "topic-1", "create returns the plain id");

  await changeSubscriber("topic-1", "subscribe", "worker-1");
  equal(calls.at(-1), { url: "/api/topics/topic-1/subscribers", method: "POST", body: { action: "subscribe", alias: "worker-1" } }, "subscribe by alias");
  await changeSubscriber("topic-1", "unsubscribe", "worker-1");
  equal(calls.at(-1)?.body, { action: "unsubscribe", alias: "worker-1" }, "unsubscribe by alias");

  reply = { status: 200, body: { topic: "topic-1", message: "post-1", duplicate: false, deliveries: [] } };
  await postToTopic("topic-1", "  hello  ", "ui-1");
  equal(calls.at(-1), { url: "/api/topics/topic-1/posts", method: "POST", body: { text: "hello", id: "ui-1" } }, "post carries the idempotence id");

  await requestFromTopic("topic-1", "worker-1", "check the build", "ui-2");
  equal(calls.at(-1), { url: "/api/topics/topic-1/requests", method: "POST", body: { target: "worker-1", text: "check the build", id: "ui-2" } }, "one-target ask");

  // Refusals never reach the network: empty input is rejected client-side.
  const before = calls.length;
  let refused = false;
  try {
    await postToTopic("topic-1", "   ", "ui-3");
  } catch (e) {
    refused = e instanceof ApiError && e.status === 400;
  }
  assert(refused, "an empty post is refused client-side");
  equal(calls.length, before, "an empty post sends nothing");

  // A daemon without these verbs answers 501; that is the only "predates" signal.
  reply = { status: 501, body: { error: "not implemented" } };
  let legacy = false;
  try {
    await listWorkConversations();
  } catch (e) {
    legacy = predatesThreads(e);
  }
  assert(legacy, "501 means the daemon predates Threads");
  reply = { status: 403, body: { error: "operator required" } };
  let forbidden = false;
  try {
    await postToTopic("topic-1", "x", "ui-4");
  } catch (e) {
    forbidden = e instanceof ApiError && e.status === 403 && !predatesThreads(e);
  }
  assert(forbidden, "a refused write surfaces its status, never as legacy");

  console.log("threads client checks passed");
}

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

void main();
