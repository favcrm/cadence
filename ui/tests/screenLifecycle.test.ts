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
console.log("screen lifecycle adversarial checks pass");
