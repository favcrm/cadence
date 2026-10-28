// CAD-1324 reviewer-written refusal check (cc13-rev-spec-1324, from the
// ticket, not the implementation): a draft-image ref the host just returned
// is requestable only on the mount that heard it, for that draft and the
// tool alias the reply came through. A ref learned on another mount or
// install, for another draft, through another alias, or for a draft whose
// later reply shows no image, closes the frame and loads nothing.
import { ScreenChannel } from "../src/features/workspace-apps/screen/screenLifecycle";
import type { ScreenPush } from "../src/features/workspace-apps/screen/screenProtocol";

function check(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }
const nonce = "c".repeat(64);
const receipt = { mount: `/api/app-screen/${"d".repeat(64)}`, bridge_nonce: nonce, generation: 1, tag: "main" };
function pushFor(install: string): ScreenPush {
  return {
    v: 1, op: "screen", install_id: install, digest: "sha256:d1", tag: "main", context_id: "ctx1",
    installation: { install_id: install, digest: "sha256:d1", title: "t", name: "social-content", version: "v", summary: "" },
    contexts: [{ id: "ctx1", label: "context" }], runs: [], outbox: [], now: 1,
    defaults: { context_id: "ctx1", revision: 0, values: {} },
    readiness: { ok: true, blockers: [] },
    publish_intents: { status: "ok", withheld: 0, rows: [] },
    actions: ["open-link"],
  };
}
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

/** One real ScreenChannel mount. `replies` are the daemon's call results, in order. */
function mount(install: string, replies: unknown[]) {
  const source = {} as Window; const port = new Port(); const asked: string[] = [];
  const failed = { count: 0 };
  const channel = new ScreenChannel(source, receipt, pushFor(install), () => {}, () => { failed.count++; }, () => {},
    async ref => { asked.push(ref); return "data:image/jpeg;base64," + "A".repeat(64); },
    { call: async () => ({ ok: true, data: replies.shift() ?? null }),
      planner: (() => { throw new Error("no planner"); }) as never, onSlot: () => {}, onLink: () => {} });
  channel.receive({ source, origin: "null", data: { v: 1, op: "init", tag: "main", bridge_nonce: nonce, generation: 1 }, ports: [port as unknown as MessagePort] });
  port.receive({ v: 1, op: "ready", accepts: ["screen.v2", "screen-actions.v1"] });
  const show = async (id: string, alias = "social.draft") => {
    port.receive({ v: 2, op: "call", id, verb: "social.drafts.show", args: { alias, draft_id: "sdr-1" } });
    await tick();
    check(port.sent.some(m => (m as { op?: string; id?: string }).op === "reply" && (m as { id?: string }).id === id), `fixture: reply ${id} was sent`);
  };
  const ask = async (ref: string) => { port.receive({ v: 2, op: "asset", ref }); await tick(); };
  return { port, asked, failed, show, ask };
}
const withImage = { draft_id: "sdr-1", asset_id: "ast-1" };
const told = "draft-image:sdr-1:social.draft";

void (async () => {
  // Positive control: the mount that heard the reply is served without closing.
  const a = mount("inst1", [withImage]);
  await a.show("c1");
  await a.ask(told);
  check(!a.port.closed && a.failed.count === 0 && a.asked.length === 1 && a.asked[0] === told,
    "positive control: the just-told draft image is served on its own mount");

  // A different mount of the same install never heard the reply: refused.
  const otherMount = mount("inst1", []);
  await otherMount.ask(told);
  check(otherMount.port.closed && otherMount.failed.count === 1 && otherMount.asked.length === 0,
    "a ref learned on another mount is refused");

  // A different install that made its own (image-less) call: refused.
  const otherInstall = mount("inst2", [{ draft_id: "sdr-9" }]);
  await otherInstall.show("c1");
  await otherInstall.ask(told);
  check(otherInstall.port.closed && otherInstall.failed.count === 1 && otherInstall.asked.length === 0,
    "a ref learned on another install is refused");

  // Same mount, a different draft id than the reply named: refused.
  const otherDraft = mount("inst1", [withImage]);
  await otherDraft.show("c1");
  await otherDraft.ask("draft-image:sdr-2:social.draft");
  check(otherDraft.port.closed && otherDraft.failed.count === 1 && otherDraft.asked.length === 0,
    "a ref for a draft the reply did not name is refused");

  // Same mount and draft, a different tool alias than the reply came through: refused.
  const otherAlias = mount("inst1", [withImage]);
  await otherAlias.show("c1");
  await otherAlias.ask("draft-image:sdr-1:social.drafts");
  check(otherAlias.port.closed && otherAlias.failed.count === 1 && otherAlias.asked.length === 0,
    "a ref through another tool alias is refused");

  // The image was served, then a later reply shows the draft without an image: refused afterwards.
  const removed = mount("inst1", [withImage, { draft_id: "sdr-1" }]);
  await removed.show("c1");
  await removed.ask(told);
  check(!removed.port.closed && removed.asked.length === 1, "fixture: the image was served before removal");
  await removed.show("c2");
  await removed.ask(told);
  check(removed.port.closed && removed.failed.count === 1 && removed.asked.length === 1,
    "a draft whose later reply shows no image is refused afterwards");

  console.log("draft image refusal checks pass");
})().catch(error => setTimeout(() => { throw error; }));
