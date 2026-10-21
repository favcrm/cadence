import test, { afterEach } from "node:test";
import assert from "node:assert/strict";
import {
  MAX_STATE_BYTES,
  ScreenRefusalError,
  ScreenTimeoutError,
  createScreenReceiver,
  readBoot,
  validateBoot,
} from "../src/index.mjs";

const boot = {
  tag: "tag-1",
  bridge_nonce: "a".repeat(64),
  generation: "gen-1",
  parent_origin: "https://board.example",
};
const tick = () => new Promise((resolve) => setImmediate(resolve));
const live = [];
afterEach(() => {
  while (live.length) live.pop().teardown();
});
async function waitFor(find, label) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const value = find();
    if (value) return value;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  throw new Error(`timed out waiting for ${label}`);
}

function connect(options = {}) {
  let host;
  const init = [];
  const pushes = [];
  const errors = [];
  const parent = {
    postMessage(message, origin, transfer) {
      init.push({ message, origin, transfer });
      host = transfer?.[0];
    },
  };
  const receiver = createScreenReceiver({
    boot,
    parent,
    timeoutMs: 100,
    onPush: (push) => pushes.push(push),
    onError: (reason) => errors.push(reason),
    ...options,
  });
  const inbox = [];
  if (host) host.onmessage = (event) => inbox.push(event.data);
  live.push(receiver);
  return { receiver, init, pushes, errors, inbox, host };
}

const push = (overrides = {}) => ({
  v: 2,
  op: "screen",
  tag: boot.tag,
  install_id: "install-1",
  digest: "sha256:" + "b".repeat(64),
  context_id: "ctx-1",
  now: 1,
  ...overrides,
});

test("boot validation pins origin, nonce and keys", () => {
  assert.equal(validateBoot(boot).parent_origin, "https://board.example");
  assert.equal(validateBoot({ ...boot, parent_origin: "http://localhost:5173" }).parent_origin, "http://localhost:5173");
  assert.throws(() => validateBoot({ ...boot, parent_origin: "http://board.example" }), /https or loopback/);
  assert.throws(() => validateBoot({ ...boot, parent_origin: "https://board.example/path" }), /origin/);
  assert.throws(() => validateBoot({ ...boot, bridge_nonce: "a".repeat(63) }), /64 lowercase hex/);
  assert.throws(() => validateBoot({ ...boot, extra: true }), /exactly/);
});

test("readBoot reads the #cadence-screen-boot element", () => {
  const documentRef = { getElementById: (id) => (id === "cadence-screen-boot" ? { textContent: JSON.stringify(boot) } : null) };
  assert.equal(readBoot(documentRef).tag, "tag-1");
  assert.throws(() => readBoot({ getElementById: () => null }), /no #cadence-screen-boot/);
});

test("init posts to the exact origin with one transferred port, then ready", async () => {
  const { init, inbox } = connect();
  assert.equal(init.length, 1);
  assert.equal(init[0].origin, boot.parent_origin);
  assert.equal(init[0].transfer.length, 1);
  assert.deepEqual(init[0].message, {
    v: 1,
    op: "init",
    tag: boot.tag,
    bridge_nonce: boot.bridge_nonce,
    generation: boot.generation,
  });
  await tick();
  assert.deepEqual(inbox[0], { v: 1, op: "ready", accepts: ["screen.v2"] });
});

test("a PUSH reaches onPush and locks the identity", async () => {
  const { host, pushes, receiver } = connect();
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the push");
  assert.equal(pushes.length, 1);
  assert.equal(receiver.accepted(), true);
});

test("a PUSH with a foreign tag tears down and never reaches onPush", async () => {
  const { host, pushes, errors, receiver } = connect();
  host.postMessage(push({ tag: "other" }));
  await waitFor(() => errors.length === 1, "the teardown");
  assert.equal(pushes.length, 0);
  assert.equal(receiver.accepted(), false);
  assert.match(errors[0], /tag mismatch/);
  await assert.rejects(receiver.call("x"), /closed/);
});

test("an identity change on an accepted port tears down", async () => {
  const { host, pushes, errors } = connect();
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  host.postMessage(push({ install_id: "other" }));
  await waitFor(() => errors.length === 1, "the identity teardown");
  assert.match(errors[0], /identity\/scope mismatch/);
});

test("call resolves on a reply and maps a refusal to its status", async () => {
  const { host, inbox, receiver } = connect();
  const ok = receiver.call("social.drafts.discard", { draft_id: "d1" });
  const request = await waitFor(() => inbox.find((m) => m.op === "call"), "the call");
  assert.deepEqual(
    { v: request.v, verb: request.verb, args: request.args },
    { v: 2, verb: "social.drafts.discard", args: { draft_id: "d1" } },
  );
  host.postMessage({ v: 2, op: "reply", id: request.id, ok: true, data: { discarded: true } });
  assert.deepEqual(await ok, { discarded: true });

  const refused = receiver.call("x");
  const second = await waitFor(() => inbox.filter((m) => m.op === "call").at(-1) === request ? null : inbox.filter((m) => m.op === "call").at(-1), "the second call");
  host.postMessage({ v: 2, op: "reply", id: second.id, ok: false, refusal: { code: "conflict", text: "busy" } });
  await assert.rejects(refused, (error) => {
    assert.ok(error instanceof ScreenRefusalError);
    assert.equal(error.status, 409);
    assert.equal(error.code, "conflict");
    return true;
  });
});

test("a tool request carries alias, input and request_id without a verb", async () => {
  const { host, inbox, receiver } = connect();
  const pending = receiver.tool("social.publish", { draft: 1 }, "req-1");
  const request = await waitFor(() => inbox.find((m) => m.op === "tool"), "the tool request");
  assert.deepEqual(
    { alias: request.alias, input: request.input, request_id: request.request_id, verb: request.verb },
    { alias: "social.publish", input: { draft: 1 }, request_id: "req-1", verb: undefined },
  );
  host.postMessage({ v: 2, op: "tool-reply", id: request.id, ok: true, data: { accepted: true } });
  assert.deepEqual(await pending, { accepted: true });
});

test("a missing reply times out and rejects", async () => {
  const { receiver } = connect({ timeoutMs: 20 });
  const pending = receiver.call("slow");
  await assert.rejects(pending, (error) => error instanceof ScreenTimeoutError && error.status === 504);
});

test("state is bounded and only sent after a port exists", async () => {
  const { inbox, receiver } = connect();
  assert.equal(receiver.state("x".repeat(MAX_STATE_BYTES)), true);
  assert.equal(receiver.state("x".repeat(MAX_STATE_BYTES + 1)), false);
  assert.equal(receiver.state(42), false);
  await waitFor(() => inbox.some((m) => m.op === "state"), "the state post");
});

test("an unsolicited reply tears the port down", async () => {
  const { host, errors } = connect();
  host.postMessage({ v: 2, op: "reply", id: "never-sent", ok: true });
  await waitFor(() => errors.length === 1, "the teardown");
  assert.match(errors[0], /never sent/);
});

test("an unknown op tears the port down", async () => {
  const { host, errors } = connect();
  host.postMessage({ v: 2, op: "pull" });
  await waitFor(() => errors.length === 1, "the teardown");
  assert.match(errors[0], /unknown op/);
});

test("teardown rejects pending work", async () => {
  const { receiver } = connect({ timeoutMs: 1000 });
  const pending = receiver.call("x");
  receiver.teardown();
  await assert.rejects(pending, /closed/);
});
