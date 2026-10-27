// CAD-1526: the turn guard must bound `messages` below the gateway's
// published maximum (100), or every later turn of a long session fails with
// `400 Field "messages" exceeds the published maximum`.
//
// The guard reads its VERSION from the pi package, so this test stages the
// real guard beside a stub package and imports it from there.
// PI_STUB_VERSION selects the runtime branch: the deployed pi (0.87.1) takes
// the ungated path, 1.0.0 takes the guidance-filtering path.
import { after, test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const VERSION = process.env.PI_STUB_VERSION ?? "0.87.1";
// Mirrors MESSAGES_KEEP in the guard: the published maximum less a margin.
const KEEP = 100 - 10;

const root = mkdtempSync(join(tmpdir(), "cadence-pi-guard-"));
after(() => rmSync(root, { recursive: true, force: true }));

const handlers = new Map();
const commands = new Map();
const pi = {
  on: (name, fn) => {
    if (!handlers.has(name)) handlers.set(name, []);
    handlers.get(name).push(fn);
  },
  registerCommand: (name, definition) => commands.set(name, definition),
  appendEntry: () => {},
  sendMessage: () => {},
};

const pkg = join(root, "node_modules", "@earendil-works", "pi-coding-agent");
mkdirSync(pkg, { recursive: true });
writeFileSync(
  join(pkg, "package.json"),
  JSON.stringify({ name: "@earendil-works/pi-coding-agent", version: "0.0.0", type: "module", main: "index.js" }),
);
writeFileSync(join(pkg, "index.js"), `export const VERSION = ${JSON.stringify(VERSION)};\n`);

const staged = join(root, "pi_turn_guard.mjs");
writeFileSync(staged, readFileSync(join(dirname(fileURLToPath(import.meta.url)), "pi_turn_guard.ts")));
const { default: install } = await import(pathToFileURL(staged).href);
install(pi);

const project = messages => handlers.get("context").at(-1)({ messages }).messages;
const user = value => ({ role: "user", content: [{ type: "text", text: value }] });
const system = () => ({ role: "system", content: [{ type: "text", text: "system" }] });
const round = (i, results) => [
  {
    role: "assistant",
    content: [{ type: "text", text: `call ${i}` }],
    toolCalls: Array.from({ length: results }, (_, r) => ({ id: `c${i}-${r}`, name: "bash" })),
  },
  ...Array.from({ length: results }, (_, r) => ({
    role: "toolResult", toolCallId: `c${i}-${r}`, content: [{ type: "text", text: `r${i}.${r}` }],
  })),
];

// The gateway's published request shape: at most 100 `messages` per request
// (AgenticOS `packages/contracts/src/inference.ts`), and every tool call
// answered by its own result — a round cannot be sent in part. A request that
// breaks either is refused the way the live gateway refuses it, so a request
// that is merely shorter is not mistaken for an answered turn.
const MAX = 100;
const gateway = messages => {
  if (!Array.isArray(messages) || messages.length === 0) {
    return { status: 400, message: 'Field "messages" must not be empty.' };
  }
  if (messages.length > MAX) {
    return { status: 400, message: 'Field "messages" exceeds the published maximum.' };
  }
  let pending = [];
  const unanswered = () => (pending.length === 0 ? null : {
    status: 400,
    message: `Field "messages" leaves ${pending.length} tool call(s) unanswered.`,
  });
  for (const message of messages) {
    if (message?.role === "assistant") {
      const settled = unanswered();
      if (settled) return settled;
      pending = (message.toolCalls ?? []).map(call => call.id);
    } else if (message?.role === "toolResult") {
      const at = pending.indexOf(message.toolCallId);
      if (at < 0) {
        return { status: 400, message: `Field "messages" carries a result for an unknown call ${message.toolCallId}.` };
      }
      pending.splice(at, 1);
    } else if (["user", "system", "developer"].includes(message?.role)) {
      const settled = unanswered();
      if (settled) return settled;
    }
  }
  return unanswered() ?? { status: 200, answer: "(answered)" };
};

test("the guard registers a context hook", () => {
  assert.equal(typeof handlers.get("context")?.at(-1), "function");
});

test("a short history passes through unchanged", () => {
  const messages = Array.from({ length: 40 }, (_, i) => user(`m${i}`));
  assert.deepEqual(project(messages), messages);
});

test("a long history is bounded below the published maximum", () => {
  const messages = [system()];
  for (let i = 0; i < 149; i += 1) messages.push(user(`turn ${i}`));
  assert.ok(messages.length > 100, "fixture must exceed the published maximum");
  const out = project(messages);
  assert.ok(out.length <= KEEP, `bounded to ${out.length}`);
  assert.equal(out[0].role, "system", "the system prompt survives");
  assert.deepEqual(out.at(-1), messages.at(-1), "the newest message opens the turn");
});

test("the window never opens on an orphaned tool result", () => {
  const messages = [system()];
  for (let i = 0; i < 100; i += 1) messages.push(...round(i, 1));
  messages.push(user("newest"));
  const out = project(messages);
  assert.ok(out.length <= KEEP);
  assert.notEqual(out[1]?.role, "toolResult", "a retained result keeps its call in front");
  assert.deepEqual(out.at(-1), messages.at(-1));
  assert.deepEqual(gateway(out), { status: 200, answer: "(answered)" });
});

test("a thread past the failure point is answered, not refused", () => {
  // CAD-1526 acceptance: drive a thread past the published maximum and assert
  // an answer, not the 400 every later turn of that session returned.
  const messages = [system(), user("the objective"), ...round(0, 1)];
  for (let i = 1; i <= 60; i += 1) messages.push(...round(i, 1));
  assert.equal(gateway(messages).status, 400, "the fixture must cross the published maximum");
  assert.deepEqual(gateway(project(messages)), { status: 200, answer: "(answered)" });
});

test("a whole tool round keeps the assistant call that produced it", () => {
  // One assistant message carrying a round of calls: the run of results
  // reaches the newest message, whose call is the first message of the round.
  const messages = [system(), user("the objective"), ...round(0, 95)];
  assert.ok(messages.length > KEEP, "the fixture must exceed the kept window");
  const out = project(messages);
  assert.equal(out[2]?.role, "assistant", "the round keeps its call");
  assert.ok(out.length <= MAX, `bounded to ${out.length}`);
  assert.deepEqual(gateway(out), { status: 200, answer: "(answered)" });
});

test("the active objective survives a long tool run", () => {
  // The turn's instruction is the newest user message, not the bootstrap: a
  // long tool run must not window it out while it is still being worked on.
  const messages = [system(), user("bootstrap"), { role: "assistant", content: [{ type: "text", text: "ok" }] }];
  messages.push(user("the active objective"));
  for (let i = 0; i < 50; i += 1) messages.push(...round(i, 1));
  assert.ok(messages.length > MAX, "the fixture must cross the published maximum");
  const out = project(messages);
  const texts = out.map(message => message.content?.[0]?.text);
  assert.ok(texts.includes("the active objective"), "the objective is kept");
  assert.ok(texts.includes("bootstrap"), "the bootstrap is kept");
  assert.ok(out.length <= MAX, `bounded to ${out.length}`);
  assert.deepEqual(gateway(out), { status: 200, answer: "(answered)" });
});

test("a tool round larger than the maximum is left out whole, never in part", () => {
  // One assistant message carrying more calls than the window can hold: a round
  // cannot be sent in part, so it is left out rather than crossing the
  // published maximum or leaving some of its calls unanswered.
  const messages = [user("the objective"), ...round(0, 99)];
  assert.ok(messages.length > MAX, "the fixture must cross the published maximum");
  const out = project(messages);
  assert.ok(out.length <= MAX, `bounded to ${out.length}`);
  assert.equal(out.filter(message => message.role === "toolResult").length, 0, "no result arrives without its round");
  assert.deepEqual(gateway(out), { status: 200, answer: "(answered)" });
});

test("the objective survives the window", () => {
  const messages = [system(), user("the objective")];
  for (let i = 0; i < 200; i += 1) messages.push(user(`turn ${i}`));
  const out = project(messages);
  assert.ok(out.length <= KEEP);
  assert.equal(out[1]?.content[0]?.text, "the objective", "the instruction survives");
  assert.deepEqual(out.at(-1), messages.at(-1));
});

test("a blank text block still becomes the stable placeholder", () => {
  const out = project([{ role: "user", content: [{ type: "text", text: "   " }] }]);
  assert.deepEqual(out[0].content, [{ type: "text", text: "(no output)" }]);
});

test("guidance filtering follows the runtime branch", async () => {
  if (VERSION !== "1.0.0") {
    // The deployed pi does not take the gated guidance path. The count bound
    // above still applies there, which is what CAD-1526 needs.
    assert.equal(handlers.has("agent_start"), false);
    return;
  }
  await commands.get("cadence-bind-turn").handler(
    JSON.stringify({ request: "r1", turn: "t1" }),
    { mode: "rpc", isIdle: () => true },
  );
  handlers.get("agent_start").forEach(handler => handler());
  const guidance = turn => ({
    role: "custom",
    customType: "cadence-turn-guidance",
    details: { turn },
    content: [{ type: "text", text: `g${turn}` }],
  });
  const out = project([user("a"), guidance("t0"), guidance("t1")]);
  const kept = out.filter(message => message.customType === "cadence-turn-guidance");
  assert.equal(kept.length, 1, "stale guidance is dropped");
  assert.equal(kept[0].details.turn, "t1", "the current turn's guidance survives");

  // The current turn's guidance is pinned rather than left to the window: a
  // turn with more messages than the window cannot drop the constraints set
  // for work that is still running.
  const long = [user("a"), guidance("t0"), guidance("t1")];
  for (let i = 0; i < 120; i += 1) long.push(user(`turn ${i}`));
  const pinned = project(long).filter(message => message.customType === "cadence-turn-guidance");
  assert.equal(pinned.length, 1, "only the current turn's guidance is kept");
  assert.equal(pinned[0].details.turn, "t1", "the current turn's guidance is pinned");
});
