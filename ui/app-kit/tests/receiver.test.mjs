import test, { afterEach } from "node:test";
import assert from "node:assert/strict";
import {
  DRAWER_CONTEXT_BYTES,
  HOST_DRAWER_VERBS,
  MAX_ASSET_BYTES,
  MAX_AUDIO_TRANSCRIPT_CHARS,
  MAX_REPLY_DATA_BYTES,
  MAX_STATE_BYTES,
  SCREEN_AUDIO_V1,
  ScreenBridgeError,
  ScreenClosedError,
  ScreenRefusalError,
  ScreenTimeoutError,
  createScreenBridge,
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

test("surfaces opt-in is advertised and the drawer surface is dispatched", async () => {
  const seen = [];
  const { inbox, host } = connect({ accepts: ["screen.v2", "screen-surfaces.v1"], onSurface: (...args) => seen.push(args) });
  await tick();
  assert.deepEqual(inbox[0].accepts, ["screen.v2", "screen-surfaces.v1"]);
  const { receiver } = connect({ accepts: ["screen.v2", "screen-surfaces.v1"] });
  assert.equal(receiver.surfaceRole(), "primary");
  assert.equal(receiver.surface(), undefined);
  host.postMessage({ v: 1, op: "surface", role: "drawer", modal: false, surface: "settings" });
  await tick();
  assert.deepEqual(seen.pop(), ["drawer", "settings", false]);
});

test("every surface shape the host sends is admitted", async () => {
  const shapes = [
    { v: 1, op: "surface", role: "primary", modal: false },
    { v: 1, op: "surface", role: "drawer", modal: false, surface: "settings" },
    { v: 1, op: "surface", role: "primary" },
  ];
  for (const shape of shapes) {
    const seen = [];
    const { host, receiver } = connect({ accepts: ["screen.v2", "screen-surfaces.v1"], onSurface: (...a) => seen.push(a) });
    host.postMessage(shape);
    await tick();
    assert.equal(seen.length, 1, `host shape ${JSON.stringify(shape)} is admitted`);
    assert.equal(receiver.surfaceRole(), shape.role);
    assert.equal(receiver.surface(), shape.surface);
  }
  const bad = connect({ accepts: ["screen.v2", "screen-surfaces.v1"] });
  bad.host.postMessage({ v: 1, op: "surface", role: "primary", modal: "no" });
  await tick();
  assert.equal(bad.receiver.accepted(), false, "a non-boolean modal still refuses");
});

test("a drawer surface opens no further drawer", async () => {
  const { receiver, host } = connect({ accepts: ["screen.v2", "screen-surfaces.v1"] });
  host.postMessage({ v: 1, op: "surface", role: "drawer", modal: false, surface: "post" });
  await tick();
  assert.equal(receiver.surfaceRole(), "drawer");
  await assert.rejects(receiver.openDrawer("settings"), /no further drawer/);
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

test("screen-locale.v1 is advertised on any screen.v2 mount without requiring records", async () => {
  const { inbox: standalone } = connect({ accepts: ["screen.v2", "screen-surfaces.v1", "screen-locale.v1"] });
  const { inbox: fiveCap } = connect({
    accepts: ["screen.v2", "screen-actions.v1", "screen-media.v1", "screen-surfaces.v1", "screen-locale.v1"],
  });
  await tick();
  assert.deepEqual(standalone[0], {
    v: 1,
    op: "ready",
    accepts: ["screen.v2", "screen-surfaces.v1", "screen-locale.v1"],
  });
  assert.deepEqual(fiveCap[0], {
    v: 1,
    op: "ready",
    accepts: ["screen.v2", "screen-actions.v1", "screen-media.v1", "screen-surfaces.v1", "screen-locale.v1"],
  });
});

test("capabilities require screen.v2, reject legacy crm tokens, and fit the six-capability bound", async () => {
  const base = { boot, parent: { postMessage() {} }, onPush() {}, timeoutMs: 100 };
  assert.throws(
    () => createScreenReceiver({ ...base, accepts: ["screen.v1", "screen-records.v1"] }),
    /screen-records\.v1 needs screen\.v2/,
  );
  assert.throws(
    () => createScreenReceiver({ ...base, accepts: ["screen.v1", "screen-locale.v1"] }),
    /screen-locale\.v1 needs screen\.v2/,
  );
  assert.throws(
    () => createScreenReceiver({ ...base, accepts: ["screen.v1", "screen-audio.v1"] }),
    /screen-audio\.v1 needs screen\.v2/,
  );
  assert.throws(
    () => createScreenReceiver({ ...base, accepts: ["screen.v1", "screen-media.v1"] }),
    /screen-media\.v1 needs screen\.v2/,
  );
  assert.throws(
    () => createScreenReceiver({ ...base, accepts: ["screen.v2", "crm-locale.v1"] }),
    /accepts must name/,
  );
  const { inbox: sixCap } = connect({
    accepts: [
      "screen.v2",
      "screen-actions.v1",
      "screen-records.v1",
      "screen-surfaces.v1",
      "screen-locale.v1",
      "screen-audio.v1",
    ],
  });
  await tick();
  assert.equal(sixCap[0].accepts.length, 6);
  assert.throws(
    () =>
      createScreenReceiver({
        ...base,
        accepts: [
          "screen.v2",
          "screen-actions.v1",
          "screen-media.v1",
          "screen-presentation.v1",
          "screen-surfaces.v1",
          "screen-locale.v1",
          "screen-audio.v1",
        ],
      }),
    /at most six ready capabilities/,
  );
});

test("maxArgsBytes admits a records CSV-sized call", async () => {
  const { host, inbox, pushes, receiver } = connect({
    accepts: ["screen.v2", "screen-records.v1"],
    maxArgsBytes: 327680,
  });
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the first push");
  const csvText = "x".repeat(200 * 1024);
  const reply = receiver.call("records.csv.preview", { csv_text: csvText });
  const request = await waitFor(
    () => inbox.find((m) => m.op === "call" && m.verb === "records.csv.preview"),
    "the CSV call",
  );
  assert.equal(request.args.csv_text.length, csvText.length);
  host.postMessage({ v: 2, op: "reply", id: request.id, ok: true, data: { rows: 1 } });
  assert.deepEqual(await reply, { rows: 1 });
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
  assert.equal(receiver.surfaceRole(), "primary");
  assert.equal(receiver.surface(), undefined);
  assert.equal(receiver.ownLeaveState(), null);
  await assert.rejects(receiver.openDrawer("settings"), ScreenClosedError);
  await assert.rejects(receiver.closeDrawer(), ScreenClosedError);
  await assert.rejects(receiver.drawerContext(), ScreenClosedError);
  await assert.rejects(receiver.call("read.run", {}), ScreenClosedError);
  await assert.rejects(receiver.audioCapture(), ScreenClosedError);
  await assert.rejects(receiver.asset("media:1"), ScreenClosedError);
  assert.equal(receiver.state("x"), false);

  // Expected tag pin and top-level window guard also fail closed into inert().
  assert.throws(() => validateBoot(boot, "other-tag"), /tag mismatch/);
  const tagMismatch = [];
  const wrongTag = createScreenReceiver({
    tag: "other-tag",
    boot,
    parent: { postMessage() {} },
    onPush() {},
    onError: (reason) => tagMismatch.push(reason),
  });
  assert.equal(wrongTag.boot, null);
  assert.match(tagMismatch[0], /tag mismatch/);

  const topWindow = { postMessage() {} };
  const topErrors = [];
  const prevParent = globalThis.parent;
  globalThis.parent = topWindow;
  try {
    const selfFramed = createScreenReceiver({
      boot,
      window: topWindow,
      onPush() {},
      onError: (reason) => topErrors.push(reason),
    });
    assert.equal(selfFramed.boot, null);
    assert.ok(topErrors.some((msg) => /child frame/.test(msg)));
  } finally {
    globalThis.parent = prevParent;
  }
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

test("openDrawer carries bounded plain-JSON context on surfaces and drawerContext unwraps it on drawer mounts", async () => {
  const { receiver, inbox, host, pushes } = connect({ accepts: ["screen.v2", "screen-surfaces.v1"] });
  host.postMessage({ v: 1, op: "surface", role: "primary", modal: false });
  host.postMessage(push());
  await waitFor(() => pushes.length === 1, "the primary push");
  await assert.rejects(receiver.drawerContext(), /only a drawer surface/);
  await assert.rejects(receiver.openDrawer("detail", { context: new Date() }), TypeError);
  await assert.rejects(receiver.openDrawer("detail", { context: { pad: "x".repeat(DRAWER_CONTEXT_BYTES) } }), TypeError);

  const opening = receiver.openDrawer("detail", { size: "wide", context: { record_id: "rec_1", tab: 2 } });
  const open = await waitFor(() => inbox.find((m) => m.op === "call" && m.verb === "screen.drawer.open"), "drawer open");
  assert.deepEqual(open.args, { surface: "detail", size: "wide", context: { record_id: "rec_1", tab: 2 } });
  host.postMessage({ v: 2, op: "reply", id: open.id, ok: true, data: { mode: "drawer", surface: "detail" } });
  await opening;

  const pres = connect({ accepts: ["screen.v2", "screen-presentation.v1"] });
  pres.host.postMessage(push());
  await waitFor(() => pres.pushes.length === 1, "the presentation push");
  pres.host.postMessage({ v: 1, op: "presentation", mode: "outlet" });
  await tick();
  await assert.rejects(pres.receiver.openDrawer("detail", { context: { a: 1 } }), ScreenClosedError);

  const drawer = connect({ accepts: ["screen.v2", "screen-surfaces.v1"] });
  drawer.host.postMessage({ v: 1, op: "surface", role: "drawer", modal: false, surface: "detail" });
  drawer.host.postMessage(push());
  await waitFor(() => drawer.pushes.length === 1, "the drawer push");
  const reading = drawer.receiver.drawerContext();
  const ctxCall = await waitFor(() => drawer.inbox.find((m) => m.op === "call" && m.verb === "screen.drawer.context"), "ctx call");
  assert.deepEqual(ctxCall.args, {});
  drawer.host.postMessage({ v: 2, op: "reply", id: ctxCall.id, ok: true, data: { context: { record_id: "rec_1" } } });
  assert.deepEqual(await reading, { record_id: "rec_1" });

  const badRead = drawer.receiver.drawerContext();
  const badCall = await waitFor(() => drawer.inbox.filter((m) => m.op === "call" && m.verb === "screen.drawer.context")[1], "bad ctx call");
  drawer.host.postMessage({ v: 2, op: "reply", id: badCall.id, ok: true, data: "not-an-object" });
  await assert.rejects(badRead, ScreenClosedError);
});

test("ownLeaveState publishes v2 leave booleans and symbol-fences overlapping handles", async () => {
  let syncAcquired = false;
  let receiverRef = null;
  const { host, inbox, receiver } = connect({
    onPush() {
      const handle = receiverRef.ownLeaveState();
      syncAcquired = handle !== null;
      handle?.release();
    },
  });
  receiverRef = receiver;
  assert.equal(receiver.ownLeaveState(), null);
  host.postMessage(push());
  await waitFor(() => syncAcquired, "synchronous ownLeaveState inside onPush");

  const first = receiver.ownLeaveState();
  assert.equal(first.publish(true), true);
  assert.equal(first.publish("yes"), false);
  await waitFor(() => inbox.some((m) => m.v === 2 && m.op === "leave" && m.unsaved === true), "dirty leave");

  const second = receiver.ownLeaveState();
  assert.equal(second.publish(true), true);
  await waitFor(() => inbox.filter((m) => m.op === "leave" && m.unsaved === true).length === 2, "second dirty leave");
  const leavesBeforeStaleRelease = inbox.filter((m) => m.op === "leave").length;
  first.release();
  assert.equal(first.publish(false), false);
  await tick();
  assert.equal(inbox.filter((m) => m.op === "leave").length, leavesBeforeStaleRelease);

  second.release();
  await waitFor(() => inbox.at(-1)?.op === "leave" && inbox.at(-1)?.unsaved === false, "active release");

  const v1 = connect({ accepts: ["screen.v1"] });
  v1.host.postMessage(push({ v: 1 }));
  await waitFor(() => v1.pushes.length === 1, "v1 push");
  assert.equal(v1.receiver.ownLeaveState(), null);
});

function fullPush(overrides = {}) {
  const install_id = overrides.install_id ?? "install-1";
  const digest = overrides.digest ?? "sha256:" + "b".repeat(64);
  return {
    v: 2, op: "screen", tag: "notes", install_id, digest, context_id: "",
    installation: { install_id, name: "notes", title: "Meeting Notes", version: "0.4.0", digest, summary: "Notes app" },
    contexts: [{ id: "ctx-1", label: "Team" }], runs: [], outbox: [],
    actions: ["records.list", "records.create", "screen.route.patch", ...HOST_DRAWER_VERBS],
    now: 1700000000, presentation_locale: "zh-TW",
    route: { section: "open", view: "list", record_id: "rec_1", segment_id: null },
    ...overrides,
  };
}

function startBridge(bridge, bootOverride = { ...boot, tag: bridge.tag }) {
  let host;
  const inbox = [];
  const parent = {
    postMessage(_msg, _origin, transfer) {
      host = transfer?.[0];
      if (host) host.onmessage = (event) => inbox.push(event.data);
    },
  };
  bridge.start({ boot: bootOverride, parent });
  live.push({ teardown: () => bridge.reset() });
  return { host, inbox };
}

test("createScreenBridge transitions to a new error snapshot when initial push or boot is refused", async () => {
  const bridge = createScreenBridge({
    tag: "notes", appName: "notes", records: true, surfaces: true, locale: true, calls: { "records.list": ["query"] },
  });
  const states = [];
  bridge.subscribe(() => states.push(bridge.getState()));
  const initial = bridge.getState();
  assert.equal(initial.status, "loading");

  const { host } = startBridge(bridge);
  host.postMessage(fullPush({ actions: ["records.list", "BAD VERB!"] }));
  await waitFor(() => bridge.getState().status === "error", "error transition");
  const failed = bridge.getState();
  assert.notEqual(failed, initial);
  assert.equal(failed.snapshot, null);
  assert.ok(failed.error instanceof ScreenBridgeError);
  assert.equal(failed.error.status, 502);
  assert.equal(states.at(-1), failed);

  bridge.reset();
  startBridge(bridge, { ...boot, tag: "wrong-tag" });
  assert.equal(bridge.getState().status, "error");
  assert.equal(bridge.getState().error.status, 502);
});

test("createScreenBridge separates host drawer verbs from app capabilities and parses locale/route/observations", async () => {
  const bridge = createScreenBridge({
    tag: "notes", appName: "notes", records: true, surfaces: true, locale: true,
    calls: { "records.list": ["query", "limit"], "records.create": ["record_id", "profile"], "screen.route.patch": ["section", "view", "record_id"] },
    observations: ["records.list"],
    route: { sections: ["all", "open"], defaultSection: "all" },
  });
  const { host, inbox } = startBridge(bridge);
  await tick();
  assert.deepEqual(inbox[0], {
    v: 1, op: "ready", accepts: ["screen.v2", "screen-surfaces.v1", "screen-records.v1", "screen-locale.v1"],
  });
  host.postMessage({ v: 1, op: "surface", role: "primary", modal: false });
  host.postMessage(fullPush({ read_observations: [{ operation: "records.list", args: { limit: 20 }, data: { items: [] } }] }));
  await waitFor(() => bridge.getState().status === "ready", "ready transition");

  const snap = bridge.getSnapshot();
  assert.equal(snap.presentationLocale, "zh-TW");
  assert.deepEqual(snap.route, { section: "open", view: "list", record_id: "rec_1", segment_id: null });
  assert.equal(snap.observations.length, 1);
  assert.deepEqual(snap.actions, ["records.list", "records.create", "screen.route.patch"]);
  assert.equal(bridge.can("records.list"), true);
  assert.equal(bridge.can("screen.drawer.open"), false);
  await assert.rejects(bridge.call(snap.scope, "screen.drawer.open", { surface: "detail" }), (err) => err.status === 403);
});

test("createScreenBridge enforces arg allowlists, per-verb byte limits, concurrency, reply bounds, and write/read retirement", async () => {
  let writes = 0;
  const bridge = createScreenBridge({
    tag: "notes", records: true, surfaces: true,
    calls: { "records.list": ["query"], "records.create": ["record_id", "profile"], "screen.route.patch": ["section", "view", "record_id"] },
    slots: { "records.export": ["format"] },
    argBytes: { "records.create": 32768 },
    singleFlight: ["records."],
    writeVerbs: ["records.create", "screen.route.patch"],
    route: { sections: ["all", "open"] },
    maxInFlight: 2,
  });
  bridge.onWrite(() => { writes += 1; });
  const { host, inbox } = startBridge(bridge);
  host.postMessage(fullPush({ actions: ["records.list", "records.create", "records.export", "screen.route.patch"] }));
  await waitFor(() => bridge.getState().status === "ready", "ready transition");
  const { scope } = bridge.getSnapshot();

  await assert.rejects(bridge.call(scope, "records.list", { bogus: 1 }), (err) => err.status === 400);
  await assert.rejects(bridge.call(scope, "records.list", { query: "x".repeat(17 * 1024) }), (err) => err.status === 400);

  const createCall = bridge.call(scope, "records.create", {
    record_id: "rec_1", profile: { body: "x".repeat(20 * 1024) }, extraUndefined: undefined,
  });
  await assert.rejects(bridge.call(scope, "records.list", { query: "a" }), (err) => err.status === 429);
  const createMsg = await waitFor(() => inbox.find((m) => m.op === "call" && m.verb === "records.create"), "create call");
  assert.deepEqual(Object.keys(createMsg.args), ["record_id", "profile"]);
  host.postMessage({ v: 2, op: "reply", id: createMsg.id, ok: true, data: { record_id: "rec_1" } });
  assert.deepEqual(await createCall, { record_id: "rec_1" });
  assert.equal(writes, 1);

  const writeForbidden = bridge.call(scope, "records.create", { record_id: "rec_2" });
  const wfMsg = await waitFor(() => inbox.filter((m) => m.op === "call" && m.verb === "records.create")[1], "forbidden write");
  host.postMessage({ v: 2, op: "reply", id: wfMsg.id, ok: false, refusal: { code: "forbidden", text: "viewer only" } });
  await assert.rejects(writeForbidden, (err) => err.status === 403);
  assert.equal(bridge.getState().status, "ready");

  const slotDone = bridge.slot(scope, "records.export", { format: "csv" });
  const slotMsg = await waitFor(() => inbox.find((m) => m.op === "slot"), "slot message");
  host.postMessage({ v: 2, op: "slot-state", id: slotMsg.id, state: "done", data: { exported: 3 } });
  assert.deepEqual(await slotDone, { exported: 3 });

  const callsBeforePatch = inbox.filter((m) => m.verb === "screen.route.patch").length;
  bridge.patchRoute({ record_id: "INVALID ID!" });
  await tick();
  assert.equal(inbox.filter((m) => m.verb === "screen.route.patch").length, callsBeforePatch);
  bridge.patchRoute({ section: "open", record_id: "rec_1" });
  const patchMsg = await waitFor(() => inbox.find((m) => m.op === "call" && m.verb === "screen.route.patch"), "route patch");
  host.postMessage({ v: 2, op: "reply", id: patchMsg.id, ok: true, data: {} });
  await tick();
  assert.equal(writes, 1);

  const leave = bridge.ownLeaveState();
  assert.equal(leave.publish(true), true);
  await waitFor(() => inbox.some((m) => m.op === "leave" && m.unsaved === true), "bridge leave");

  const poisoned = bridge.call(scope, "records.list", { query: "ok" });
  const listMsg = await waitFor(() => inbox.find((m) => m.op === "call" && m.verb === "records.list"), "list call");
  host.postMessage({ v: 2, op: "reply", id: listMsg.id, ok: true, data: { items: [{ nested: { action_token: "leaked" } }] } });
  await assert.rejects(poisoned, (err) => err.status === 502);
  assert.equal(bridge.getState().status, "error");
  assert.equal(leave.publish(false), false);

  bridge.reset();
  const second = startBridge(bridge);
  second.host.postMessage(fullPush({ actions: ["records.list"] }));
  await waitFor(() => bridge.getState().status === "ready", "second ready");
  const huge = bridge.call(bridge.getSnapshot().scope, "records.list", {});
  const hugeMsg = await waitFor(() => second.inbox.find((m) => m.op === "call"), "huge call");
  second.host.postMessage({ v: 2, op: "reply", id: hugeMsg.id, ok: true, data: { blob: "x".repeat(MAX_REPLY_DATA_BYTES) } });
  await assert.rejects(huge, (err) => err.status === 502);
  assert.equal(bridge.getState().status, "error");
});

test("createScreenBridge supports actions/media tool, asset, and state channels", async () => {
  const bridge = createScreenBridge({ tag: "studio", actions: true, media: true, locale: true, calls: { "social.drafts.list": [] } });
  const { host, inbox } = startBridge(bridge);
  host.postMessage(fullPush({
    tag: "studio",
    installation: { install_id: "install-1", name: "studio", title: "Studio", version: "1.0.0", digest: "sha256:" + "b".repeat(64), summary: "Studio" },
    actions: ["social.drafts.list", "social.publish", ...HOST_DRAWER_VERBS],
  }));
  await waitFor(() => bridge.getState().status === "ready", "studio ready");
  const { scope, actions } = bridge.getSnapshot();
  assert.deepEqual(actions, ["social.drafts.list", "social.publish"]);

  const toolPending = bridge.tool(scope, "social.publish", { text: "hi" }, "req-1");
  const toolMsg = await waitFor(() => inbox.find((m) => m.op === "tool"), "bridge tool");
  host.postMessage({ v: 2, op: "tool-reply", id: toolMsg.id, ok: true, data: { posted: true } });
  assert.deepEqual(await toolPending, { posted: true });

  const assetPending = bridge.asset(scope, "media:1", { timeoutMs: 500 });
  await waitFor(() => inbox.some((m) => m.op === "asset" && m.ref === "media:1"), "bridge asset");
  host.postMessage({ v: 2, op: "asset", ref: "media:1", data_url: PNG });
  assert.equal(await assetPending, PNG);

  assert.equal(bridge.state("resume-snapshot"), true);
  await waitFor(() => inbox.some((m) => m.op === "state" && m.data === "resume-snapshot"), "bridge state");
});

test("receiver.audioCapture negotiates screen-audio.v1, waits without default timeout, enforces single-flight, and validates replies", async () => {
  const unopted = connect();
  unopted.host.postMessage(push());
  await waitFor(() => unopted.pushes.length === 1, "unopted push");
  await assert.rejects(unopted.receiver.audioCapture(), (err) => err instanceof ScreenClosedError && /screen-audio\.v1/.test(err.message));

  const { host, inbox, pushes, errors, receiver } = connect({
    accepts: ["screen.v2", SCREEN_AUDIO_V1],
    timeoutMs: 20,
  });
  await tick();
  assert.deepEqual(inbox[0], { v: 1, op: "ready", accepts: ["screen.v2", "screen-audio.v1"] });
  await assert.rejects(receiver.audioCapture(), ScreenClosedError);

  host.postMessage(push({ audio_capture: 1 }));
  await waitFor(() => pushes.length === 1, "audio push");
  await assert.rejects(receiver.audioCapture({ mode: "stream" }), TypeError);
  await assert.rejects(receiver.audioCapture({ extra: true }), TypeError);
  await assert.rejects(receiver.audioCapture({ timeoutMs: 0 }), TypeError);

  // Interactive audio modal stays open past receiver timeoutMs (20ms) when timeoutMs is omitted.
  const recording = receiver.audioCapture();
  const req1 = await waitFor(() => inbox.find((m) => m.op === "audio"), "record request");
  assert.deepEqual(req1, { v: 2, op: "audio", id: req1.id, mode: "record" });
  await new Promise((resolve) => setTimeout(resolve, 55));
  assert.equal(await Promise.race([recording.then(() => "settled", () => "settled"), tick().then(() => "open")]), "open");

  // Single-flight refuses a concurrent capture without tearing down the port.
  await assert.rejects(receiver.audioCapture({ mode: "upload" }), (err) => err instanceof ScreenRefusalError && err.code === "conflict");
  assert.equal(inbox.filter((m) => m.op === "audio").length, 1);

  const capturePayload = {
    transcript: "Reviewed Q4 launch checklist.",
    duration_seconds: 185,
    held_at: "2026-11-13T03:00:00.000Z",
    source: "recording",
  };
  host.postMessage({ v: 2, op: "audio-reply", id: req1.id, ok: true, data: capturePayload });
  const result = await recording;
  assert.deepEqual(result, capturePayload);
  assert.equal(Object.isFrozen(result), true);
  assert.deepEqual(errors, []);

  // Malformed audio-reply data tears down the port.
  const broken = receiver.audioCapture({ mode: "upload" });
  const brokenRejected = assert.rejects(broken, ScreenClosedError);
  const req2 = await waitFor(() => inbox.filter((m) => m.op === "audio")[1], "upload request");
  assert.equal(req2.mode, "upload");
  host.postMessage({
    v: 2,
    op: "audio-reply",
    id: req2.id,
    ok: true,
    data: { transcript: "x".repeat(MAX_AUDIO_TRANSCRIPT_CHARS + 1), duration_seconds: 10, held_at: "2026-11-13T03:00:00Z", source: "upload" },
  });
  await waitFor(() => errors.length === 1, "malformed audio-reply teardown");
  await brokenRejected;
});

test("createScreenBridge projects audioCapture, preserves state on cancel/refusal, and retires on unauthorized or malformed replies", async () => {
  const bridge = createScreenBridge({
    tag: "notes",
    appName: "notes",
    records: true,
    surfaces: true,
    locale: true,
    audio: true,
    calls: { "records.list": ["query"] },
  });
  const { host, inbox } = startBridge(bridge);
  await tick();
  assert.deepEqual(inbox[0], {
    v: 1,
    op: "ready",
    accepts: ["screen.v2", "screen-surfaces.v1", "screen-records.v1", "screen-locale.v1", "screen-audio.v1"],
  });

  // Without audio_capture: 1, canAudioCapture() is false and audioCapture refuses locally with 403.
  host.postMessage(fullPush({ actions: ["records.list"] }));
  await waitFor(() => bridge.getState().status === "ready", "ready without audio_capture");
  assert.equal(bridge.canAudioCapture(), false);
  await assert.rejects(
    bridge.audioCapture(bridge.getSnapshot().scope, { mode: "record" }),
    (err) => err.status === 403 && err.code === "forbidden",
  );
  assert.equal(bridge.getState().status, "ready");

  // With audio_capture: 1, record and upload resolve and cancelled/owner_required keep the bridge ready.
  host.postMessage(fullPush({ actions: ["records.list"], audio_capture: 1 }));
  await waitFor(() => bridge.getState().pushes === 2, "push with audio_capture");
  assert.equal(bridge.canAudioCapture(), true);
  const { scope } = bridge.getSnapshot();

  const cancelled = bridge.audioCapture(scope, { mode: "record" });
  const cancelMsg = await waitFor(() => inbox.find((m) => m.op === "audio"), "cancel audio op");
  host.postMessage({ v: 2, op: "audio-reply", id: cancelMsg.id, ok: false, refusal: { code: "cancelled", text: "Audio capture was cancelled." } });
  await assert.rejects(cancelled, (err) => err.status === 400 && err.code === "cancelled");
  assert.equal(bridge.getState().status, "ready", "cancelling the audio modal leaves the screen bridge ready");

  const ownerRefused = bridge.audioCapture(scope, { mode: "upload" });
  const ownerMsg = await waitFor(() => inbox.filter((m) => m.op === "audio")[1], "owner_required audio op");
  host.postMessage({ v: 2, op: "audio-reply", id: ownerMsg.id, ok: false, refusal: { code: "owner_required", text: "Workspace owner required." } });
  await assert.rejects(ownerRefused, (err) => err.status === 403 && err.code === "owner_required");
  assert.equal(bridge.getState().status, "ready");

  const uploaded = bridge.audioCapture(scope, { mode: "upload" });
  const uploadMsg = await waitFor(() => inbox.filter((m) => m.op === "audio")[2], "upload audio op");
  const uploadData = {
    transcript: "Uploaded voice memo transcript.",
    duration_seconds: 42,
    held_at: "2026-11-13T02:30:00.000Z",
    source: "upload",
  };
  host.postMessage({ v: 2, op: "audio-reply", id: uploadMsg.id, ok: true, data: uploadData });
  assert.deepEqual(await uploaded, uploadData);
  assert.equal(bridge.getState().status, "ready");

  // Unauthorized retires the bridge.
  const unauth = bridge.audioCapture(scope, { mode: "record" });
  const unauthMsg = await waitFor(() => inbox.filter((m) => m.op === "audio")[3], "unauthorized audio op");
  host.postMessage({ v: 2, op: "audio-reply", id: unauthMsg.id, ok: false, refusal: { code: "unauthorized", text: "Session expired." } });
  await assert.rejects(unauth, (err) => err.status === 401);
  assert.equal(bridge.getState().status, "error");

  // Forged audio_capture marker (not 1) is rejected on push.
  bridge.reset();
  const second = startBridge(bridge);
  second.host.postMessage(fullPush({ actions: ["records.list"], audio_capture: 2 }));
  await waitFor(() => bridge.getState().status === "error", "forged audio_capture error");
  assert.equal(bridge.getState().error.status, 502);
});
