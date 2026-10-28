// CAD-1324: a draft image the host just told the app about (a call reply
// with {draft_id, asset_id}) is requestable at once; a ref never returned
// still closes the frame.
import { ScreenChannel } from "../src/features/workspace-apps/screen/screenLifecycle";
import type { ScreenPush } from "../src/features/workspace-apps/screen/screenProtocol";

function check(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }
const nonce = "a".repeat(64);
const receipt = { mount: `/api/app-screen/${"b".repeat(64)}`, bridge_nonce: nonce, generation: 1, tag: "main" };
const push: ScreenPush = {
  v: 1, op: "screen", install_id: "inst1", digest: "sha256:d1", tag: "main", context_id: "ctx1",
  installation: { install_id: "inst1", digest: "sha256:d1", title: "t", name: "social-content", version: "v", summary: "" },
  contexts: [{ id: "ctx1", label: "context" }], runs: [], outbox: [], now: 1,
  defaults: { context_id: "ctx1", revision: 0, values: {} },
  readiness: { ok: true, blockers: [] },
  publish_intents: { status: "ok", withheld: 0, rows: [] },
  actions: ["open-link"],
};
class Port {
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onmessageerror: (() => void) | null = null;
  sent: unknown[] = []; closed = false;
  start() {}
  close() { this.closed = true; }
  postMessage(value: unknown) { this.sent.push(value); }
  receive(value: unknown) { this.onmessage?.({ data: value }); }
}
const tick = () => new Promise(resolve => setTimeout(resolve, 0));

function mount() {
  const source = {} as Window; const port = new Port(); const asked: string[] = [];
  const channel = new ScreenChannel(source, receipt, push, () => {}, () => {}, () => {},
    async ref => { asked.push(ref); return "data:image/jpeg;base64," + "A".repeat(64); },
    { call: async () => ({ ok: true, data: { draft_id: "sdr-1", asset_id: "ast-1" } }),
      planner: (() => { throw new Error("no planner"); }) as never, onSlot: () => {}, onLink: () => {} });
  channel.receive({ source, origin: "null", data: { v: 1, op: "init", tag: "main", bridge_nonce: nonce, generation: 1 }, ports: [port as unknown as MessagePort] });
  port.receive({ v: 1, op: "ready", accepts: ["screen.v2", "screen-actions.v1"] });
  return { port, asked };
}

void (async () => {
  const told = mount();
  told.port.receive({ v: 2, op: "call", id: "c1", verb: "social.drafts.show", args: { alias: "social.draft", draft_id: "sdr-1" } });
  await tick();
  check(told.port.sent.some(m => (m as { op?: string }).op === "reply"), "fixture: the call reply was sent");
  told.port.receive({ v: 2, op: "asset", ref: "draft-image:sdr-1:social.draft" }); await tick();
  check(!told.port.closed && told.asked.length === 1, "a draft image the host just returned is served without closing");

  const never = mount();
  never.port.receive({ v: 2, op: "call", id: "c1", verb: "social.drafts.show", args: { alias: "social.draft", draft_id: "sdr-1" } });
  await tick();
  never.port.receive({ v: 2, op: "asset", ref: "draft-image:sdr-2:social.draft" }); await tick();
  check(never.port.closed && never.asked.length === 0, "a draft image ref never returned still closes the frame");
  console.log("draft image ref checks pass");
})().catch(error => setTimeout(() => { throw error; }));
