import { ChatChannel } from "../src/features/app-shell/chat/chatChannel";
import { planFrames } from "../src/features/app-shell/chat/frames";
import { matchDirective } from "../src/features/app-shell/chat/directive";
import { parseAppChat } from "../src/features/app-shell/chat/contract";

/** CAD-1110, Tier 2 frame isolation (contract I14-I17), the supporting
 *  checks beside the acceptance test: the chat frame shares CAD-1006's
 *  receive side, so these cover only what chat adds. */
function check(value: unknown, label: string): void { if (!value) throw new Error(label); }
const nonce = "a".repeat(64);
const receipt = { mount: `/api/app-screen/${"b".repeat(64)}`, bridge_nonce: nonce, generation: 2, tag: "post-preview" };
const init = { v: 1, op: "init", tag: "post-preview", bridge_nonce: nonce, generation: 2 };
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
function mount(data: Record<string, string | number | boolean> = { text: "Hi" }) {
  const source = {} as Window;
  const port = new Port();
  let removed = 0, failed = 0, ready = 0;
  const channel = new ChatChannel(source, receipt, { kind: "cadence_post_preview", data },
    () => ++removed, () => ++failed, () => ++ready);
  channel.receive({ source, origin: "null", data: init, ports: [port as unknown as MessagePort] });
  return { channel, port, counts: () => ({ removed, failed, ready }) };
}

// The child must opt in to chat-directive.v1; anything else closes with nothing sent.
for (const ready of [{ v: 1, op: "ready" }, { v: 1, op: "ready", accepts: ["publish-intents.v1"] }]) {
  const m = mount();
  m.port.receive(ready);
  check(m.port.closed && m.port.sent.length === 0 && m.counts().failed === 1, `no opt-in, no push: ${JSON.stringify(ready)}`);
}
// An opted-in child gets exactly the directive: no projection, no id, no cap.
const m = mount();
check(m.port.sent.length === 0, "nothing before ready");
m.port.receive({ v: 1, op: "ready", accepts: ["chat-directive.v1"] });
const first = m.port.sent[0] as Record<string, unknown>;
check(m.port.sent.length === 1 && JSON.stringify(first) === JSON.stringify({ v: 1, op: "directive", tag: "post-preview", kind: "cadence_post_preview", data: { text: "Hi" } }), "the only push is the directive");
check(!JSON.stringify(first).includes("install_id") && !JSON.stringify(first).includes(receipt.mount.slice(-64)), "no id and no frame capability in the push");
m.channel.update({ kind: "cadence_post_preview", data: { text: "Second" } });
check(m.port.sent.length === 2 && !m.port.closed, "a newer directive re-pushes into the live frame");
// The closed child vocabulary still binds: anything but ready/state closes.
m.port.receive({ v: 1, op: "rpc", actor: "operator" });
check(m.port.closed && m.counts().failed === 1, "a chat frame has no child RPC");
m.channel.update({ kind: "cadence_post_preview", data: { text: "late" } });
check(m.port.sent.length === 2, "a closed frame is never pushed to");
// Over 4 KiB closes instead of sending.
const big = mount({ text: "x".repeat(512), a: "x".repeat(512), b: "x".repeat(512), c: "x".repeat(512), d: "x".repeat(512), e: "x".repeat(512), f: "x".repeat(512), g: "x".repeat(512), h: "x".repeat(512) });
big.port.receive({ v: 1, op: "ready", accepts: ["chat-directive.v1"] });
check(big.port.closed && big.port.sent.length === 0, "an oversized push closes the frame");

// One live frame per tag, at most three per pane, a failed tag falls back to text.
const descriptor = parseAppChat({
  contract: "app-chat/v1", app: "demo",
  directives: ["a", "b", "c", "d"].map((t) => ({ match: `cadence_${t}`, render: `screen:${t}` })),
});
const row = (key: string, tag: string) => ({ key, directive: matchDirective(JSON.stringify({ [`cadence_${tag}`]: { text: key } }), descriptor) });
const rows = [row("e1", "a"), row("e2", "b"), row("e3", "c"), row("e4", "d"), row("e5", "d")];
const plan = planFrames(rows, new Set());
check(plan.get("e1")?.state === "closed", "the least recently updated tag is closed past three");
check(plan.get("e2")?.state === "live" && plan.get("e3")?.state === "live", "three tags stay live");
check(plan.get("e4")?.state === "live" && plan.get("e5")?.state === "updated", "a repeat tag pushes into its one frame");
check(!planFrames(rows, new Set(["d"])).has("e4"), "a failed tag renders as plain text");
console.log("chat frame checks passed");
