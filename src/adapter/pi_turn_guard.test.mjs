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
  for (let i = 0; i < 100; i += 1) {
    messages.push({ role: "assistant", content: [{ type: "text", text: `call ${i}` }] });
    messages.push({ role: "toolResult", content: [{ type: "text", text: `r${i}` }] });
  }
  messages.push(user("newest"));
  const out = project(messages);
  assert.ok(out.length <= KEEP);
  assert.notEqual(out[1]?.role, "toolResult", "a retained result keeps its call in front");
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
});
