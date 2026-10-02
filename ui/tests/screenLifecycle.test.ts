import { parseChild, parseInit, type ScreenPush } from "../src/features/workspace-apps/screen/screenProtocol";
import { parseMount, ScreenChannel } from "../src/features/workspace-apps/screen/screenLifecycle";

function check(value: unknown, label: string): void { if (!value) throw new Error(label); }
const nonce = "a".repeat(64);
const receipt = { mount: `/api/app-screen/${"b".repeat(64)}`, bridge_nonce: nonce, generation: 2, tag: "main" };
const init = { v: 1, op: "init", tag: "main", bridge_nonce: nonce, generation: 2 };
const push: ScreenPush = { v: 1, op: "screen", install_id: "i", digest: "d", tag: "main", context_id: "c",
  installation: { install_id: "i", digest: "d", title: "t", name: "n", version: "v", summary: "" },
  contexts: [{ id: "c", label: "context" }], runs: [], outbox: [], now: 1 };
check(parseMount(receipt, "main"), "valid distinct capability/nonce");
check(!parseMount({ ...receipt, mount: `/api/app-screen/${nonce}` }, "main"), "capability is not bridge nonce");
check(!parseMount({ ...receipt, mount: "https://other.example/token" }, "main"), "foreign frame URL refused");
check(!parseInit({ ...init, operator: true }), "forged init field refused");
check(!parseChild({ v: 1, op: "ready", actor: "operator" }), "forged port field refused");
check(!parseChild({ v: 1, op: "state", data: "界".repeat(12000) }), "UTF8 state bound");
check(parseChild({ v: 1, op: "state", data: "x".repeat(32768) }), "exact byte bound accepted");

class Port {
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onmessageerror: (() => void) | null = null;
  sent: unknown[] = [];
  closed = false;
  start() {}
  close() { this.closed = true; }
  postMessage(value: unknown) { this.sent.push(value); }
  receive(value: unknown) { this.onmessage?.({ data: value }); }
}
function mount() {
  const source = {} as Window;
  const port = new Port();
  let removed = 0, failed = 0, ready = 0;
  const channel = new ScreenChannel(source, receipt, push, () => ++removed, () => ++failed, () => ++ready);
  const event = { source, origin: "null", data: init, ports: [port as unknown as MessagePort] };
  return { channel, port, event, counts: () => ({ removed, failed, ready }) };
}
const good = mount();
good.channel.receive(good.event);
check(good.port.sent.length === 0, "no private push before ready");
good.port.receive({ v: 1, op: "ready" });
check(good.port.sent.length === 1 && good.counts().ready === 1, "ready then one push");
good.channel.update({ ...push, context_id: "other" });
check(good.port.closed && good.counts().removed === 1, "context changes close port and remove frame");
good.port.receive({ v: 1, op: "ready" });
check(good.port.sent.length === 1, "late message cannot revive retired mount");
const foreign = mount(); foreign.channel.receive({ ...foreign.event, source: {} as Window });
check(!foreign.port.closed && foreign.counts().ready === 0, "unrelated listeners own foreign ports, no data disclosed");
const foreignOrigin = mount(); foreignOrigin.channel.receive({ ...foreignOrigin.event, origin: "https://foreign.example" });
check(foreignOrigin.port.closed && foreignOrigin.counts().failed === 1, "owned frame with foreign origin fails closed");
const wrong = mount(); wrong.channel.receive({ ...wrong.event, data: { ...init, generation: 3 } });
check(wrong.port.closed && wrong.counts().failed === 1, "wrong generation fails closed");
const twice = mount(); twice.channel.receive(twice.event); twice.channel.receive(twice.event);
check(twice.port.closed && twice.counts().failed === 1, "second init closes mount");
const navigate = mount(); navigate.channel.receive(navigate.event); navigate.channel.load(); navigate.channel.load();
check(navigate.port.closed && navigate.counts().removed === 1, "self navigation tears down");
const forged = mount(); forged.channel.receive(forged.event); forged.port.receive({ v: 1, op: "rpc", actor: "operator" });
check(forged.port.closed && forged.counts().failed === 1, "no child RPC authority");

// CAD-1025: publish-intents.v1 is opt-in at `ready`; the first PUSH already
// carries the latest projection, and a forged opt-in closes the mount.
const intents = { status: "ok" as const, withheld: 0, rows: [{ intent_id: "i", install_id: "i", context_id: "c", run_id: "r",
  effect_id: "e", state: "queued" as const, channel: "instagram" as const, destination_id: "d", due_epoch: 1, timezone: "UTC" }] };
const extended: ScreenPush = { ...push, runs: [{ id: "r", state: "running", title: "t", snapshot_digest: "s", workflow: { title: "t" }, context_id: "c" }],
  outbox: [{ effect_id: "e", state: "done", title: "t", run_id: "r", context_id: "c" }],
  publish_intents: { status: "loading", withheld: 0, rows: [] } };
check(parseChild({ v: 1, op: "ready", accepts: ["publish-intents.v1"] }), "intent opt-in accepted");
for (const accepts of [[], ["publish-intents.v2"], ["publish-intents.v1", "publish-intents.v1"], "publish-intents.v1", ["publish-intents.v1", "grants"]])
  check(!parseChild({ v: 1, op: "ready", accepts }), `forged opt-in refused: ${JSON.stringify(accepts)}`);
const opted = mount(); opted.channel.update(extended); opted.channel.receive(opted.event);
opted.channel.update({ ...extended, publish_intents: intents });
check(opted.port.sent.length === 0, "no push before ready even after updates");
opted.port.receive({ v: 1, op: "ready", accepts: ["publish-intents.v1"] });
const first = opted.port.sent[0] as ScreenPush;
check(opted.port.sent.length === 1 && first.publish_intents?.rows[0]?.intent_id === "i", "first push carries the latest intents");
const bare = mount(); bare.channel.update(extended); bare.channel.receive(bare.event); bare.port.receive({ v: 1, op: "ready" });
const legacy = bare.port.sent[0] as Record<string, unknown> & ScreenPush;
check(!("publish_intents" in legacy) && !("context_id" in legacy.runs[0]) && !("run_id" in legacy.outbox[0]),
  "a child that did not opt in receives the exact v1 shape");
bare.channel.update({ ...extended, publish_intents: intents });
check(!("publish_intents" in (bare.port.sent[1] as object)), "later updates keep the negotiated shape");
const forgedOptIn = mount(); forgedOptIn.channel.receive(forgedOptIn.event);
forgedOptIn.port.receive({ v: 1, op: "ready", accepts: ["publish-intents.v1", "credentials"] });
check(forgedOptIn.port.closed && forgedOptIn.port.sent.length === 0, "forged opt-in closes the mount with nothing sent");
const unextended = mount(); unextended.channel.receive(unextended.event); unextended.port.receive({ v: 1, op: "ready", accepts: ["publish-intents.v1"] });
check(unextended.port.closed && unextended.port.sent.length === 0, "an opted-in child never gets a push without publish_intents");
const switched = mount(); switched.channel.update(extended); switched.channel.receive(switched.event); switched.port.receive({ v: 1, op: "ready", accepts: ["publish-intents.v1"] });
switched.channel.update({ ...extended, context_id: "other", publish_intents: intents });
check(switched.port.closed && switched.port.sent.length === 1, "intents for a switched context never reach the old mount");
const overCap = { ...extended, publish_intents: { status: "ok" as const, withheld: 0,
  rows: Array.from({ length: 100 }, (_, k) => ({ ...intents.rows[0], intent_id: `i${k}`, destination_id: "d".repeat(1400) })) } };
const heavyOpt = mount(); heavyOpt.channel.update(overCap); heavyOpt.channel.receive(heavyOpt.event);
heavyOpt.port.receive({ v: 1, op: "ready", accepts: ["publish-intents.v1"] });
check(!heavyOpt.port.closed && (heavyOpt.port.sent[0] as ScreenPush).publish_intents?.status === "unavailable",
  "an opted-in push over the cap degrades to unavailable intents");
const heavyBare = mount(); heavyBare.channel.update(overCap); heavyBare.channel.receive(heavyBare.event);
heavyBare.port.receive({ v: 1, op: "ready" });
check(!heavyBare.port.closed && !("publish_intents" in (heavyBare.port.sent[0] as object)), "a legacy child is sized on its own shape");
const badLink = mount(); badLink.channel.update({ ...extended, outbox: [{ ...extended.outbox[0], run_id: "" }] });
badLink.channel.receive(badLink.event); badLink.port.receive({ v: 1, op: "ready", accepts: ["publish-intents.v1"] });
check(badLink.port.closed && badLink.port.sent.length === 0 && badLink.counts().failed === 1 && badLink.counts().ready === 0,
  "out-of-range linkage closes an opted-in mount with nothing sent and no ready");
console.log("screen lifecycle adversarial checks pass");
