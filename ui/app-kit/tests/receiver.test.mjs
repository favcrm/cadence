import test, { afterEach } from "node:test";
import assert from "node:assert/strict";
import {
  MAX_ASSET_BYTES,
  MAX_STATE_BYTES,
  ScreenClosedError,
  ScreenRefusalError,
  ScreenTimeoutError,
  createScreenReceiver,
  readBoot,
  validateBoot,
} from "../src/index.mjs";

const boot = {
  tag: "tag-1",
  bridge_nonce: "a".repeat(64),
  generation: 1,
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

test("boot validation pins origin, nonce, tag and numeric generation", () => {
  assert.equal(validateBoot(boot).parent_origin, "https://board.example");
  assert.equal(validateBoot({ ...boot, generation: 0 }).generation, 0);
  assert.equal(
    validateBoot({ ...boot, parent_origin: "http://cadence-1234.localhost:1234" }).parent_origin,
    "http://cadence-1234.localhost:1234",
  );
  assert.throws(() => validateBoot({ ...boot, parent_origin: "http://127.0.0.1:3000" }), /loopback/);
  assert.throws(() => validateBoot({ ...boot, parent_origin: "http://board.example" }), /loopback/);
  assert.throws(() => validateBoot({ ...boot, parent_origin: "https://board.example/path" }), /origin/);
  assert.throws(() => validateBoot({ ...boot, bridge_nonce: "a".repeat(63) }), /64 lowercase hex/);
  assert.throws(() => validateBoot({ ...boot, generation: "1" }), /safe integer/);
  assert.throws(() => validateBoot({ ...boot, generation: -1 }), /safe integer/);
  assert.throws(() => validateBoot({ ...boot, tag: "Tag-1" }), /lowercase/);
  assert.throws(() => validateBoot({ ...boot, tag: "a.b" }), /lowercase/);
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

test("presentation opt-in is advertised and dispatched; the default is outlet", async () => {
  const modes = [];
  const { receiver, inbox, host } = connect({
    accepts: ["screen.v2", "screen-actions.v1", "screen-presentation.v1"],
    onPresentation: (mode) => modes.push(mode),
  });
  const ready = await waitFor(() => inbox.find((m) => m.op === "ready"), "ready");
  assert.deepEqual(ready.accepts, ["screen.v2", "screen-actions.v1", "screen-presentation.v1"]);
  assert.equal(receiver.presentation(), "outlet");
  host.postMessage({ v: 1, op: "presentation", mode: "drawer" });
  await waitFor(() => receiver.presentation() === "drawer", "the drawer surface");
  assert.deepEqual(modes, ["drawer"]);
});

test("presentation without the opt-in closes the port", async () => {
  const { receiver, inbox, host, errors } = connect({ accepts: ["screen.v2"] });
  await waitFor(() => inbox.find((m) => m.op === "ready"), "ready");
  host.postMessage({ v: 1, op: "presentation", mode: "drawer" });
  await waitFor(() => errors.length === 1, "the teardown");
  assert.match(errors[0], /opt-in/);
  assert.equal(receiver.presentation(), "outlet");
});

test("drawer helpers send host UI verbs and validate the surface", async () => {
  const { receiver, inbox, host, pushes } = connect({
    accepts: ["screen.v2", "screen-actions.v1", "screen-presentation.v1"],
  });
  await waitFor(() => inbox.find((m) => m.op === "ready"), "ready");
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the push");
  host.postMessage({ v: 1, op: "presentation", mode: "drawer" });
  await waitFor(() => receiver.presentation() === "drawer", "the presentation");
  const opening = receiver.openDrawer("settings");
  const open = await waitFor(() => inbox.find((m) => m.op === "call" && m.verb === "screen.drawer.open"), "the drawer open");
  assert.deepEqual(open.args, { surface: "settings" });
  host.postMessage({ v: 2, op: "reply", id: open.id, ok: true, data: { mode: "drawer", surface: "settings" } });
  assert.deepEqual(await opening, { mode: "drawer", surface: "settings" });
  const closing = receiver.closeDrawer();
  const close = await waitFor(() => inbox.find((m) => m.op === "call" && m.verb === "screen.drawer.close"), "the drawer close");
  assert.deepEqual(close.args, {});
  assert.notEqual(close.id, open.id);
  host.postMessage({ v: 2, op: "reply", id: close.id, ok: true, data: { mode: "outlet" } });
  assert.deepEqual(await closing, { mode: "outlet" });
  await assert.rejects(receiver.openDrawer("not a surface!"), /surface/);
});

test("openDrawer passes an optional size and refuses an unknown one", async () => {
  const { receiver, inbox, host, pushes } = connect({
    accepts: ["screen.v2", "screen-actions.v1", "screen-presentation.v1"],
  });
  await waitFor(() => inbox.find((m) => m.op === "ready"), "ready");
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the push");
  host.postMessage({ v: 1, op: "presentation", mode: "drawer" });
  await waitFor(() => receiver.presentation() === "drawer", "the presentation");
  const opening = receiver.openDrawer("draft", { size: "wide" });
  const open = await waitFor(() => inbox.find((m) => m.op === "call" && m.verb === "screen.drawer.open"), "the drawer open");
  assert.deepEqual(open.args, { surface: "draft", size: "wide" });
  host.postMessage({ v: 2, op: "reply", id: open.id, ok: true, data: { mode: "drawer", surface: "draft" } });
  assert.deepEqual(await opening, { mode: "drawer", surface: "draft" });
  const narrow = receiver.openDrawer("draft", { size: "narrow" });
  const second = await waitFor(() => inbox.find((m) => m.op === "call" && m.verb === "screen.drawer.open" && m.id !== open.id), "the narrow open");
  assert.deepEqual(second.args, { surface: "draft", size: "narrow" });
  host.postMessage({ v: 2, op: "reply", id: second.id, ok: true, data: { mode: "drawer", surface: "draft" } });
  await narrow;
  const sent = inbox.length;
  await assert.rejects(receiver.openDrawer("draft", { size: "huge" }), /size/);
  assert.equal(inbox.length, sent, "an unknown size is never sent");
});

test("drawer helpers refuse without the presentation opt-in", async () => {
  const { receiver, host, pushes } = connect({ accepts: ["screen.v2"] });
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the push");
  await assert.rejects(receiver.openDrawer("settings"), /opt into/);
  await assert.rejects(receiver.closeDrawer(), /opt into/);
});

test("opted-in drawer helpers wait for an actual host presentation", async () => {
  const { receiver, inbox, host, pushes } = connect({
    accepts: ["screen.v2", "screen-presentation.v1"],
  });
  await waitFor(() => inbox.find((m) => m.op === "ready"), "ready");
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the push");
  await assert.rejects(receiver.openDrawer("settings"), ScreenClosedError);
  await assert.rejects(receiver.closeDrawer(), ScreenClosedError);
  assert.equal(inbox.filter((m) => m.op === "call").length, 0);
  host.postMessage({ v: 1, op: "presentation", mode: "drawer" });
  await waitFor(() => receiver.presentation() === "drawer", "the presentation");
  const opening = receiver.openDrawer("settings");
  const open = await waitFor(
    () => inbox.find((m) => m.op === "call" && m.verb === "screen.drawer.open"),
    "the drawer open",
  );
  host.postMessage({ v: 2, op: "reply", id: open.id, ok: true, data: { mode: "drawer", surface: "settings" } });
  await opening;
});

test("opted-in malformed presentations close the port without dispatching", async () => {
  const cases = [
    { v: 2, op: "presentation", mode: "drawer" },
    { v: 1, op: "presentation", mode: "other" },
    { v: 1, op: "presentation" },
    { v: 1, op: "presentation", mode: "drawer", extra: true },
  ];
  for (const message of cases) {
    const modes = [];
    const { receiver, inbox, host, errors } = connect({
      accepts: ["screen.v2", "screen-presentation.v1"],
      onPresentation: (mode) => modes.push(mode),
    });
    await waitFor(() => inbox.find((m) => m.op === "ready"), "ready");
    host.postMessage(message);
    await waitFor(() => errors.length === 1, "the teardown");
    assert.equal(receiver.presentation(), "outlet");
    assert.deepEqual(modes, []);
  }
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
  const { host, inbox, pushes, receiver } = connect();
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
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
  const { host, inbox, pushes, receiver } = connect({ accepts: ["screen.v2", "screen-actions.v1"] });
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
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
  const { host, pushes, receiver } = connect({ timeoutMs: 20 });
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
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
  const { host, pushes, receiver } = connect({ timeoutMs: 1000 });
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  const pending = receiver.call("x");
  receiver.teardown();
  await assert.rejects(pending, /closed/);
});

test("the action channel is closed until a screen.v2 PUSH", async () => {
  const { receiver } = connect();
  await assert.rejects(receiver.call("read.run", {}), (error) => {
    assert.ok(error instanceof ScreenClosedError);
    assert.equal(error.code, "gone");
    assert.match(error.message, /screen\.v2 PUSH/);
    return true;
  });
  await assert.rejects(receiver.asset("media:1"), (error) => error instanceof ScreenClosedError);
});

test("a v1-only receiver refuses the v2 action channel", async () => {
  const { host, pushes, receiver } = connect({ accepts: ["screen.v1"] });
  host.postMessage(push({ v: 1 }));
  await waitFor(() => pushes.length === 1, "the v1 push");
  await assert.rejects(receiver.call("read.run", {}), (error) => error.code === "gone");
});

test("request shapes are bounded at send time", async () => {
  const { host, inbox, pushes, receiver } = connect({ accepts: ["screen.v2", "screen-actions.v1"] });
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  await assert.rejects(receiver.call("read.run", "not-an-object"), /plain object/);
  await assert.rejects(receiver.call("read.run", { blob: "x".repeat(16 * 1024) }), /16 KiB/);
  await assert.rejects(receiver.slot("publish.cancel", {}, { x: 0, y: 0, w: 12 }), /anchor/);
  await assert.rejects(receiver.slot("publish.cancel", {}, { x: -1, y: 0, w: 12, h: 4 }), /anchor/);
  await assert.rejects(receiver.tool("Social.Publish", {}, "req-1"), /alias/);
  await assert.rejects(receiver.tool("social.publish", {}, "bad id"), /request_id/);
  assert.equal(inbox.filter((m) => m.op === "call" || m.op === "slot" || m.op === "tool").length, 0);
});

test("a slot resolves the host's event, not just its state", async () => {
  const { host, inbox, pushes, receiver } = connect();
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  const pending = receiver.slot("publish.cancel", { run_id: "r1" }, "footer");
  const request = await waitFor(() => inbox.find((m) => m.op === "slot"), "the slot request");
  assert.equal(request.anchor, "footer");
  host.postMessage({
    v: 2,
    op: "slot-state",
    id: request.id,
    state: "done",
    receipt: { run_id: "r1", request_id: "req-1", state: "done" },
  });
  assert.deepEqual(await pending, {
    state: "done",
    receipt: { run_id: "r1", request_id: "req-1", state: "done" },
  });
});

test("the tool channel requires the screen-actions.v1 negotiation", async () => {
  const { receiver } = connect();
  await assert.rejects(receiver.tool("social.publish", {}, "req-1"), (error) => {
    assert.ok(error instanceof ScreenClosedError);
    assert.match(error.message, /screen-actions\.v1/);
    return true;
  });
});

test("a reply with the wrong version or request kind tears down", async () => {
  const { host, inbox, pushes, errors, receiver } = connect();
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  const pending = receiver.call("read.run", {});
  const rejected = assert.rejects(pending, ScreenClosedError);
  const request = await waitFor(() => inbox.find((m) => m.op === "call"), "the call");
  host.postMessage({ v: 2, op: "tool-reply", id: request.id, ok: true, data: {} });
  await waitFor(() => errors.length === 1, "the kind teardown");
  await rejected;

  const second = connect();
  second.host.postMessage(push());
  await waitFor(() => second.pushes.length === 1, "the second push");
  const waiting = second.receiver.call("read.run", {});
  const waitingRejected = assert.rejects(waiting, ScreenClosedError);
  const req = await waitFor(() => second.inbox.find((m) => m.op === "call"), "the second call");
  second.host.postMessage({ v: 1, op: "reply", id: req.id, ok: true, data: {} });
  await waitFor(() => second.errors.length === 1, "the version teardown");
  await waitingRejected;
});

test("a slot stays open through pending and resolves on done", async () => {
  const events = [];
  const { host, inbox, pushes, receiver } = connect({ onSlotState: (event) => events.push(event) });
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  const pending = receiver.slot("publish.cancel", {});
  const request = await waitFor(() => inbox.find((m) => m.op === "slot"), "the slot request");
  host.postMessage({ v: 2, op: "slot-state", id: request.id, state: "pending" });
  await tick();
  assert.equal(events.length, 1);
  assert.equal(events[0].state, "pending");
  assert.equal(await Promise.race([pending.then(() => "resolved"), tick().then(() => "open")]), "open");
  const receipt = { run_id: "run-" + "c".repeat(32), request_id: "req-1", state: "done" };
  host.postMessage({ v: 2, op: "slot-state", id: request.id, state: "done", receipt });
  assert.deepEqual(await pending, { state: "done", receipt });
});

test("an asset reply outside the image contract tears down", async () => {
  const { host, pushes, errors, receiver } = connect();
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  const pending = receiver.asset("media:1");
  const rejected = assert.rejects(pending, ScreenClosedError);
  await tick();
  host.postMessage({ v: 2, op: "asset", ref: "media:1", data_url: "https://evil.example/x.png" });
  await waitFor(() => errors.length === 1, "the asset teardown");
  await rejected;
});

test("a refused boot returns an inert receiver whose calls also report closed", async () => {
  const errors = [];
  const receiver = createScreenReceiver({
    boot: { ...boot, generation: "1" },
    parent: { postMessage() {} },
    onPush() {},
    onError: (reason) => errors.push(reason),
  });
  assert.equal(receiver.boot, null);
  assert.equal(errors.length, 1);
  assert.equal(receiver.presentation(), "outlet");
  await assert.rejects(receiver.openDrawer("settings"), ScreenClosedError);
  await assert.rejects(receiver.closeDrawer(), ScreenClosedError);
  await assert.rejects(receiver.call("read.run", {}), ScreenClosedError);
  await assert.rejects(receiver.asset("media:1"), ScreenClosedError);
  assert.equal(receiver.state("x"), false);
});

test("a v1-only receiver sends the bare ready envelope", async () => {
  const { inbox } = connect({ accepts: ["screen.v1"] });
  await tick();
  assert.deepEqual(inbox[0], { v: 1, op: "ready" });
});

test("a second concurrent tool call is refused locally", async () => {
  const { host, inbox, pushes, receiver } = connect({ accepts: ["screen.v2", "screen-actions.v1"], timeoutMs: 1000 });
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  const first = receiver.tool("social.publish", {}, "req-1");
  const request = await waitFor(() => inbox.find((m) => m.op === "tool"), "the tool request");
  await assert.rejects(receiver.tool("social.publish", {}, "req-2"), (error) => {
    assert.ok(error instanceof ScreenRefusalError);
    assert.equal(error.code, "conflict");
    return true;
  });
  assert.equal(inbox.filter((m) => m.op === "tool").length, 1);
  host.postMessage({ v: 2, op: "tool-reply", id: request.id, ok: true, data: { accepted: true } });
  assert.deepEqual(await first, { accepted: true });
  const third = receiver.tool("social.publish", {}, "req-3");
  await waitFor(() => inbox.filter((m) => m.op === "tool").length === 2, "the third tool request");
  const next = inbox.filter((m) => m.op === "tool").at(-1);
  host.postMessage({ v: 2, op: "tool-reply", id: next.id, ok: true, data: {} });
  await third;
});

test("a malformed call refusal tears down and rejects its own request", async () => {
  const { host, inbox, pushes, errors, receiver } = connect();
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  const pending = receiver.call("read.run", {});
  const rejected = assert.rejects(pending, ScreenClosedError);
  const request = await waitFor(() => inbox.find((m) => m.op === "call"), "the call");
  host.postMessage({ v: 2, op: "reply", id: request.id, ok: false, refusal: { code: "BAD CODE" } });
  await waitFor(() => errors.length === 1, "the malformed teardown");
  await rejected;
});

test("an oversized asset data URL tears down", async () => {
  const { host, pushes, errors, receiver } = connect();
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  const pending = receiver.asset("media:1");
  const rejected = assert.rejects(pending, ScreenClosedError);
  await tick();
  host.postMessage({
    v: 2,
    op: "asset",
    ref: "media:1",
    data_url: "data:image/png;base64," + "A".repeat(MAX_ASSET_BYTES),
  });
  await waitFor(() => errors.length === 1, "the bound teardown");
  await rejected;
});

test("a timed-out tool keeps the channel busy until the host replies", async () => {
  const { host, inbox, pushes, receiver } = connect({ accepts: ["screen.v2", "screen-actions.v1"], timeoutMs: 20 });
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  const first = receiver.tool("social.publish", {}, "req-1");
  const request = await waitFor(() => inbox.find((m) => m.op === "tool"), "the first tool request");
  await assert.rejects(first, ScreenTimeoutError);
  await assert.rejects(receiver.tool("social.publish", {}, "req-2"), (error) => {
    assert.ok(error instanceof ScreenRefusalError);
    assert.equal(error.code, "conflict");
    return true;
  });
  assert.equal(inbox.filter((m) => m.op === "tool").length, 1);
  host.postMessage({ v: 2, op: "tool-reply", id: request.id, ok: true, data: { accepted: true } });
  await tick();
  const third = receiver.tool("social.publish", {}, "req-3");
  const next = await waitFor(() => inbox.filter((m) => m.op === "tool").at(-1) !== request ? inbox.filter((m) => m.op === "tool").at(-1) : null, "the third tool request");
  host.postMessage({ v: 2, op: "tool-reply", id: next.id, ok: true, data: {} });
  await third;
});

test("a replayed tool reply cannot release another tool's gate", async () => {
  const { host, inbox, pushes, receiver } = connect({ accepts: ["screen.v2", "screen-actions.v1"], timeoutMs: 1000 });
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  const first = receiver.tool("social.publish", {}, "req-1");
  const firstRequest = await waitFor(() => inbox.find((m) => m.op === "tool"), "the first tool request");
  host.postMessage({ v: 2, op: "tool-reply", id: firstRequest.id, ok: true, data: {} });
  await first;

  const second = receiver.tool("social.publish", {}, "req-2");
  const secondRequest = await waitFor(
    () => inbox.filter((m) => m.op === "tool").find((m) => m.id !== firstRequest.id),
    "the second tool request",
  );
  host.postMessage({ v: 2, op: "tool-reply", id: firstRequest.id, ok: true, data: {} });
  await tick();
  await assert.rejects(receiver.tool("social.publish", {}, "req-3"), (error) => error.code === "conflict");
  assert.equal(inbox.filter((m) => m.op === "tool").length, 2);
  host.postMessage({ v: 2, op: "tool-reply", id: secondRequest.id, ok: true, data: {} });
  await second;
});

test("a wrong-kind late reply tears down", async () => {
  const { host, inbox, pushes, errors, receiver } = connect({ timeoutMs: 20 });
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  const pending = receiver.call("read.run", {});
  await assert.rejects(pending, ScreenTimeoutError);
  const request = await waitFor(() => inbox.find((m) => m.op === "call"), "the call");
  host.postMessage({ v: 2, op: "tool-reply", id: request.id, ok: true, data: {} });
  await waitFor(() => errors.length === 1, "the wrong-kind teardown");
});

test("teardown after a timed-out tool reports closed, not busy", async () => {
  const { host, pushes, receiver } = connect({ accepts: ["screen.v2", "screen-actions.v1"], timeoutMs: 20 });
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  await assert.rejects(receiver.tool("social.publish", {}, "req-1"), ScreenTimeoutError);
  receiver.teardown();
  await assert.rejects(receiver.tool("social.publish", {}, "req-2"), ScreenClosedError);
});

test("a malformed matching late reply tears down instead of releasing the gate", async () => {
  const { host, inbox, pushes, errors, receiver } = connect({ accepts: ["screen.v2", "screen-actions.v1"], timeoutMs: 20 });
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  await assert.rejects(receiver.tool("social.publish", {}, "req-1"), ScreenTimeoutError);
  const request = await waitFor(() => inbox.find((m) => m.op === "tool"), "the tool request");
  host.postMessage({ v: 2, op: "tool-reply", id: request.id });
  await waitFor(() => errors.length === 1, "the malformed late teardown");
  await assert.rejects(receiver.tool("social.publish", {}, "req-2"), ScreenClosedError);
});

test("a tool request carries generation_scope when given", async () => {
  const { host, inbox, pushes, receiver } = connect({ accepts: ["screen.v2", "screen-actions.v1"] });
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  const scope = { operation: "caption", draft_id: "draft_1", revision: 3 };
  const pending = receiver.tool("caption.generate", { idea: "x" }, "req-1", scope);
  const request = await waitFor(() => inbox.find((m) => m.op === "tool"), "the scoped tool request");
  assert.deepEqual(request.generation_scope, scope);
  host.postMessage({ v: 2, op: "tool-reply", id: request.id, ok: true, data: { ok: true } });
  await pending;
});

test("an invalid generation_scope is refused before posting", async () => {
  const { host, inbox, pushes, receiver } = connect({ accepts: ["screen.v2", "screen-actions.v1"] });
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  for (const scope of [
    { operation: "video", draft_id: "draft_1", revision: 1 },
    { operation: "caption", draft_id: "draft_1", revision: 0 },
    { operation: "caption", draft_id: "draft 1", revision: 1 },
    { operation: "caption", draft_id: "draft_1", revision: 1, extra: true },
  ]) {
    await assert.rejects(receiver.tool("caption.generate", {}, "req-1", scope), /generation_scope/);
  }
  assert.equal(inbox.filter((m) => m.op === "tool").length, 0);
});

// CAD-1921: a host that cannot serve a ref never answers it. `asset(ref, {timeoutMs})`
// rejects, frees the ref for a re-request, and tolerates the host's late answer.
const PNG = "data:image/png;base64,iVBORw0KGgo=";
test("an asset request with a timeout rejects, and the same ref can be requested again", async () => {
  const { host, inbox, pushes, errors, receiver } = connect();
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  await assert.rejects(receiver.asset("media:1", { timeoutMs: 20 }), (error) => {
    assert.ok(error instanceof ScreenTimeoutError);
    assert.equal(error.code, "timeout");
    return true;
  });
  const again = receiver.asset("media:1", { timeoutMs: 1000 });
  await waitFor(() => inbox.filter((m) => m.op === "asset").length === 2, "the re-request");
  host.postMessage({ v: 2, op: "asset", ref: "media:1", data_url: PNG });
  assert.equal(await again, PNG);
  assert.deepEqual(errors, []);
});

test("a late asset reply after a timeout is dropped once, not a teardown", async () => {
  const { host, pushes, errors, receiver } = connect();
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  await assert.rejects(receiver.asset("media:1", { timeoutMs: 20 }), ScreenTimeoutError);
  host.postMessage({ v: 2, op: "asset", ref: "media:1", data_url: PNG });
  await tick();
  await tick();
  assert.deepEqual(errors, [], "the late answer is dropped");
  // Only that one late answer: a second unsolicited reply still tears down.
  host.postMessage({ v: 2, op: "asset", ref: "media:1", data_url: PNG });
  await waitFor(() => errors.length === 1, "the unsolicited teardown");
});

test("a late reply that crosses a re-request answers the re-request", async () => {
  const { host, pushes, errors, receiver } = connect();
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  await assert.rejects(receiver.asset("media:1", { timeoutMs: 20 }), ScreenTimeoutError);
  const again = receiver.asset("media:1", { timeoutMs: 1000 });
  host.postMessage({ v: 2, op: "asset", ref: "media:1", data_url: PNG });
  assert.equal(await again, PNG);
  // The host may still answer the re-request itself: dropped, not a teardown.
  host.postMessage({ v: 2, op: "asset", ref: "media:1", data_url: PNG });
  await tick();
  await tick();
  assert.deepEqual(errors, []);
});

test("an asset request without a timeout waits as before; a bad timeout is refused", async () => {
  const { host, pushes, receiver } = connect({ timeoutMs: 20 });
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  const open = receiver.asset("media:1");
  await new Promise((resolve) => setTimeout(resolve, 60));
  assert.equal(await Promise.race([open.then(() => "settled", () => "settled"), tick().then(() => "open")]), "open");
  for (const bad of [0, -1, 1.5, "20", Number.MAX_SAFE_INTEGER + 1]) {
    await assert.rejects(receiver.asset("media:2", { timeoutMs: bad }), TypeError);
  }
});

test("late replies for every ref the host can still hold are dropped, oldest first", async () => {
  const { host, pushes, errors, receiver } = connect();
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  const refs = Array.from({ length: 256 }, (_, i) => `media:${i}`);
  await Promise.all(refs.map((ref) => assert.rejects(receiver.asset(ref, { timeoutMs: 20 }), ScreenTimeoutError)));
  for (const ref of refs) host.postMessage({ v: 2, op: "asset", ref, data_url: PNG });
  await tick();
  await tick();
  assert.deepEqual(errors, [], "the host answers in queue order; none tears down");
});

test("a call reply's asset_refs admit those refs; a reply without them (an older host) admits none", async () => {
  const { host, inbox, pushes, errors, receiver } = connect();
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  const ref = "draft-image:sdr-1:social.draft";
  const old = receiver.call("social.drafts.show", { draft_id: "sdr-1" });
  const first = await waitFor(() => inbox.find((m) => m.op === "call"), "the first call");
  host.postMessage({ v: 2, op: "reply", id: first.id, ok: true, data: { draft_id: "sdr-1", asset_id: "ast-1" } });
  assert.deepEqual(await old, { draft_id: "sdr-1", asset_id: "ast-1" });
  assert.equal(receiver.assetAdmitted(ref), false, "an older host never says so");
  const told = receiver.call("social.drafts.show", { draft_id: "sdr-1" });
  const second = await waitFor(() => inbox.filter((m) => m.op === "call")[1], "the second call");
  host.postMessage({ v: 2, op: "reply", id: second.id, ok: true, data: { draft_id: "sdr-1", asset_id: "ast-1" }, asset_refs: [ref, 7, ""] });
  assert.deepEqual(await told, { draft_id: "sdr-1", asset_id: "ast-1" }, "the app still gets only data");
  assert.equal(receiver.assetAdmitted(ref), true);
  assert.deepEqual(errors, []);
});
