import { parseChild, type ReplyMessage } from "../src/features/workspace-apps/screen/screenProtocol";
import { SlotController, slotBox, SLOT_GUARD_MS, type SlotView } from "../src/features/workspace-apps/screen/screenSlot";
import { ScreenChannel, type ScreenActions } from "../src/features/workspace-apps/screen/screenLifecycle";

function check(value: unknown, label: string): void { if (!value) throw new Error(label); }

// Wire: closed verbs, exact fields, bounded anchors and args.
check(parseChild({ v: 2, op: "call", id: "c1", verb: "read.run", args: { run_id: "r" } }), "call parses");
check(parseChild({ v: 2, op: "slot", id: "s1", verb: "run.start", args: {}, anchor: { x: 1, y: 2, w: 100, h: 40 } }), "slot parses");
check(parseChild({ v: 2, op: "slot", id: "s1", verb: "run.start", args: {}, anchor: "footer" }), "footer anchor parses");
check(!parseChild({ v: 2, op: "call", id: "c1", verb: "run.start", args: {} }), "a spend verb is never a call");
check(!parseChild({ v: 2, op: "slot", id: "s1", verb: "read.run", args: {}, anchor: "footer" }), "a call verb is never a slot");
check(!parseChild({ v: 2, op: "call", id: "c1", verb: "publish.start", args: {} }), "unknown verb refused");
check(!parseChild({ v: 2, op: "slot", id: "s1", verb: "publish.start", args: {}, anchor: "footer" }), "publish verbs wait for HP4");
check(!parseChild({ v: 2, op: "call", id: "c1", verb: "read.run", args: {}, actor: "operator" }), "forged field refused");
check(!parseChild({ v: 2, op: "slot", id: "s1", verb: "run.start", args: {}, anchor: { x: -1, y: 0, w: 1, h: 1 } }), "negative anchor refused");
check(!parseChild({ v: 2, op: "slot", id: "s1", verb: "run.start", args: {}, anchor: { x: 0, y: 0, w: 0, h: 1 } }), "empty anchor refused");
check(!parseChild({ v: 2, op: "slot", id: "s1", verb: "run.start", args: {}, anchor: { x: NaN, y: 0, w: 1, h: 1 } }), "NaN anchor refused");
check(!parseChild({ v: 2, op: "call", id: "c1", verb: "read.run", args: { big: "x".repeat(17 * 1024) } }), "args bounded");
check(!parseChild({ v: 2, op: "call", id: "bad id!", verb: "read.run", args: {} }), "id grammar");

// Geometry: clamped inside the frame.
const box = slotBox({ x: 900, y: -5, w: 300, h: 10 }, 400, 300);
check(box.x === 100 && box.y === 0 && box.w === 300 && box.h === 32, "anchor is clamped to the frame and to a usable size");
check(slotBox("footer", 400, 300).footer, "footer bar");

// Controller: the guard, one live slot, trusted press, one run.
function controller() {
  let now = 1000;
  const sent: ReplyMessage[] = [];
  let view: SlotView | null = null;
  let runs = 0;
  const c = new SlotController(m => sent.push(m), v => { view = v; },
    async (_verb, args) => ({ label: String(args.label ?? "Go"), run: async () => { runs++; return { ok: true as const, data: {} }; } }),
    () => now);
  return { c, sent, view: () => view, runs: () => runs, at: (t: number) => { now = t; }, now: () => now };
}
const req = (id: string, label: string, x = 0) => ({ v: 2 as const, op: "slot" as const, id, verb: "run.start" as const, args: { label }, anchor: { x, y: 0, w: 100, h: 40 } });
const tick = () => new Promise(resolve => setTimeout(resolve, 0));
(async () => {
  const a = controller();
  await a.c.request(req("s1", "Draft"));
  check(a.view()?.label === "Draft", "slot appears with the host's label");
  const token = a.view()!.token;
  a.at(1000 + SLOT_GUARD_MS - 1);
  check(!a.c.tap(token, true, a.now()), "a tap inside the guard is ignored");
  a.at(1000 + SLOT_GUARD_MS);
  check(!a.c.tap(token, false, a.now()), "an untrusted press is ignored");
  check(!a.c.tap("forged", true, a.now()), "a forged token is ignored");
  check(!a.c.tap(token, true, 1000 + SLOT_GUARD_MS - 10), "a press that began inside the guard is ignored");
  check(a.runs() === 0, "nothing ran yet");
  check(a.c.tap(token, true, a.now()), "a trusted press after the guard fires");
  check(!a.c.tap(token, true, a.now()), "a second tap while it runs does nothing");
  await tick();
  check(a.runs() === 1 && a.view() === null, "ran exactly once and the slot is gone");
  check(a.sent.map(m => m.op === "slot-state" ? m.state : "").join() === "pending,done", "frame sees state only");
  check(!a.c.tap(token, true, a.now() + 1000) && a.runs() === 1, "a replayed token after use does nothing");

  // Moving the slot restarts the guard.
  const m = controller();
  await m.c.request(req("s1", "Draft", 0));
  const t = m.view()!.token;
  m.at(2000);
  await m.c.request(req("s1", "Draft", 50));
  m.at(2000 + SLOT_GUARD_MS - 1);
  check(!m.c.tap(t, true, 1000 + SLOT_GUARD_MS * 3), "a slot that moved within the guard does not fire");
  m.at(2000 + SLOT_GUARD_MS);
  check(m.c.tap(t, true, 2000 + SLOT_GUARD_MS), "it fires once the guard has passed since the move");

  // Different args retire the slot; one live slot only.
  const s = controller();
  await s.c.request(req("s1", "Draft"));
  const first = s.view()!.token;
  await s.c.request(req("s1", "Other"));
  check(s.view()!.token !== first && s.view()!.label === "Other", "same id, different args is a new slot");
  check(s.sent.some(x => x.op === "slot-state" && x.state === "refused" && x.refusal?.code === "superseded"), "the old slot is retired");
  s.at(5000);
  check(!s.c.tap(first, true, 5000), "the retired slot's token is dead");
  s.c.close();
  check(s.view() === null, "closing retires the slot");

  // A refused plan never draws a button.
  const r = new SlotController(() => undefined, () => undefined, async () => ({ code: "nope", text: "No." }), () => 0);
  await r.request(req("s1", "x"));
  check(r.view() === null, "a refused plan draws nothing");

  // Channel: only a v2 child with an action host may act; anything else closes.
  const nonce = "a".repeat(64);
  const receipt = { mount: `/api/app-screen/${"b".repeat(64)}`, bridge_nonce: nonce, generation: 2, tag: "main" };
  const init = { v: 1, op: "init", tag: "main", bridge_nonce: nonce, generation: 2 };
  const push = { v: 1, op: "screen", install_id: "i", digest: "d", tag: "main", context_id: "c",
    installation: { install_id: "i", digest: "d", title: "t", name: "n", version: "v", summary: "" },
    contexts: [{ id: "c", label: "x" }], runs: [], outbox: [], publish_intents: { status: "ok", withheld: 0, rows: [] }, now: 1 } as never;
  class Port { onmessage: ((e: { data: unknown }) => void) | null = null; onmessageerror: (() => void) | null = null;
    sent: unknown[] = []; closed = false; start() {} close() { this.closed = true; }
    postMessage(v: unknown) { this.sent.push(v); } receive(v: unknown) { this.onmessage?.({ data: v }); } }
  const mountChannel = (actions?: ScreenActions) => {
    const source = {} as Window; const port = new Port();
    const channel = new ScreenChannel(source, receipt, push, () => undefined, () => undefined, () => undefined, undefined, actions);
    channel.receive({ source, origin: "null", data: init, ports: [port as unknown as MessagePort] });
    return { channel, port };
  };
  const slotMsg = { v: 2, op: "slot", id: "s1", verb: "run.start", args: {}, anchor: "footer" };
  const callMsg = { v: 2, op: "call", id: "c1", verb: "read.run", args: { run_id: "r" } };
  const actions: ScreenActions = { call: async () => ({ ok: true, data: { hi: 1 } }), planner: async () => ({ code: "x", text: "y" }), onSlot: () => undefined, onLink: () => undefined };
  const v1 = mountChannel(actions); v1.port.receive({ v: 1, op: "ready" }); v1.port.receive(callMsg);
  check(v1.port.closed, "a v1 child cannot call");
  const none = mountChannel(); none.port.receive({ v: 1, op: "ready", accepts: ["screen.v2"] }); none.port.receive(slotMsg);
  check(none.port.closed, "a frame with no action host (non-operator) cannot ask for a slot");
  const early = mountChannel(actions); early.port.receive(callMsg);
  check(early.port.closed, "no call before ready");
  const ok = mountChannel(actions); ok.port.receive({ v: 1, op: "ready", accepts: ["screen.v2"] }); ok.port.receive(callMsg);
  await tick();
  check(!ok.port.closed && JSON.stringify(ok.port.sent.at(-1)) === JSON.stringify({ v: 2, op: "reply", id: "c1", ok: true, data: { hi: 1 } }), "a v2 child gets its reply");
  const unknown = mountChannel(actions); unknown.port.receive({ v: 1, op: "ready", accepts: ["screen.v2"] });
  unknown.port.receive({ v: 2, op: "call", id: "c2", verb: "run.start", args: {} });
  check(unknown.port.closed, "an unknown call verb closes the port");
  console.log("screenSlot ok");
})().catch(error => setTimeout(() => { throw error; }));
