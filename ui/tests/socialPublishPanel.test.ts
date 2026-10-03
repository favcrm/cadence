export {};
/** CAD-787 mounted panel QA with an injected stub client — no fetch, no
 *  provider, no live post. Proves the dpq_ grant input, the CAD-1027
 *  confirmation step and its one-approval-one-intent ids, now-vs-schedule,
 *  every dispatch state, cancel behaviour and empty/error states. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/app-installations/install-a" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "SVGElement", "navigator", "MutationObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history"]) {
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
}
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function(this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const PublishPanel = (require("../src/features/workspace-apps/PublishPanel") as typeof import("../src/features/workspace-apps/PublishPanel")).default;
function assert(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }
const host = document.createElement("div"); document.body.append(host);
const root = createRoot(host);
const flush = () => React.act(async () => { await new Promise(resolve => setTimeout(resolve, 0)); });
const text = () => host.textContent ?? "";
const button = (label: string) => Array.from(host.querySelectorAll("button")).find(value => value.textContent?.trim() === label) ?? null;
const candidate = { run_id: "run-a", effect_id: "effect-a", artifact_id: "artifact-a", bundle_digest: "bundle-digest", title: "Synthetic caption", caption: "Synthetic caption", image_digest: "i-digest", writer: "writer-a", reviewer: "reviewer-a" };
// CAD-1027: the run title and the reviewed artifact text differ on purpose —
// the confirmation must show the text freeze posts, never the title.
const reviewedText = "Reviewed caption body that will post.\nSecond line, verbatim.";
let artifactId = "artifact-a";
let releaseArtifact: (() => void) | null = null;
const loadArtifact = (id: string) => new Promise<any>(resolve => {
  const done = () => resolve({ id: artifactId === id ? id : artifactId, digest: "sha256:artifact-digest", media_type: "text/plain", size: reviewedText.length, text: reviewedText });
  releaseArtifact = done;
});
async function openConfirmation() {
  await React.act(async () => { button("Review post now")?.click(); });
  await flush();
  await React.act(async () => { releaseArtifact?.(); });
  await flush();
}
let refuseSchedule = false;
const cancelScopes: [string, string | null][] = [];
const reviewedHex = "ab".repeat(32);
const otherHex = "cd".repeat(32);
let importHex = otherHex;
const imports: any[] = [];
const receipts = async () => [{ id: "receipt-a", run_id: "run-a", slot: "image", asset: { media_type: "image/png", digest: `sha256:${reviewedHex}`, size: 1 } }];
let intents: any[] = [];
let failList = false;
const scheduled: any[] = [];
const cancelled: string[] = [];
const sent: string[] = [];
const sendScopes: [string, string | null][] = [];
const stub = {
  list: async () => { if (failList) throw new Error("Operator session expired"); return { intents }; },
  show: async (intentId: string) => ({ intent: intents.find(value => value.intent_id === intentId) }),
  schedule: async (body: any) => {
    if (refuseSchedule) throw new Error("approval_replay: this approval already authorized another social publish intent");
    assert(body.slot === "publication" && body.timezone === "Asia/Hong_Kong" && body.destination_id === "17841400008460056", "Panel schedules exact pilot destination with timezone");
    assert(!Object.hasOwn(body, "caption_digest"), "Panel never invents digests");
    assert(body.media_key === `dp1.ws.conn.${reviewedHex.slice(0, 32)}`, "An image draft schedules only with the verified media key");
    assert(/^apv-[0-9a-f]{32}$/.test(body.approval_id) && body.request_id === body.approval_id && body.grant_id === "dpq_synthetic_grant_ig", "Confirmation mints the approval id that is also the request id");
    const intent = { intent_id: `intent-${scheduled.length}`, install_id: "install-a", context_id: null, run_id: body.run_id, effect_id: body.effect_id, state: body.due_epoch <= Math.floor(Date.now() / 1000) + 5 ? "processing" : "queued", channel: body.toolkit, destination_id: body.destination_id, caption_digest: "c-digest", image_digest: "i-digest", frozen_digest: "binding-digest", idempotency_key: `key-${scheduled.length}`, due_epoch: body.due_epoch, timezone: body.timezone, grant_id: body.grant_id, approval_id: body.approval_id, writer: "writer-a", reviewer: "reviewer-a", permalink: null, receipt: null, refusal: null, upstream: null };
    scheduled.push(body); intents = [intent, ...intents];
    return { intent };
  },
  importMedia: async (body: any) => {
    assert(Object.keys(body).every(key => ["request_id", "install_id", "context_id", "run_id", "artifact_id", "bundle_digest", "slot", "toolkit", "destination_id"].includes(key)) && /^imp-[0-9a-f]{32}$/.test(body.request_id), "Import sends provenance and scope only");
    imports.push(body);
    return { media_key: `dp1.ws.conn.${importHex.slice(0, 32)}`, image_digest: importHex };
  },
  cancel: async (intentId: string, installId: string, contextId: string | null) => { cancelScopes.push([installId, contextId]); cancelled.push(intentId); intents = intents.map(value => value.intent_id === intentId ? { ...value, state: "cancelled" } : value); return { intent: intents.find(value => value.intent_id === intentId) }; },
  sendNow: async (intentId: string, installId: string, contextId: string | null) => { sendScopes.push([installId, contextId]); sent.push(intentId); intents = intents.map(value => value.intent_id === intentId ? { ...value, state: "posted" } : value); return { intent: intents.find(value => value.intent_id === intentId) }; },
};
let mounts = 0;
async function render(props: any) {
  await React.act(async () => { root.render(React.createElement(PublishPanel, { key: ++mounts, installId: "install-a", contextId: null, candidates: [], canWrite: true, client: stub, loadArtifact, imageReceipts: receipts, ...props })); });
  await flush();
}
async function typeGrant(value: string) {
  const input = host.querySelector("#publish-grant") as HTMLInputElement;
  await React.act(async () => {
    Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")!.set!.call(input, value);
    input.dispatchEvent(new win.Event("input", { bubbles: true }));
  });
  await flush();
}
async function main() {
  await render({});
  assert(text().includes("No publish grant") && text().includes("No approved drafts"), "Missing grant and drafts render honest empty states");
  assert(text().includes("1–10 uses") && text().includes("send_disabled"), "Grant-use range and disabled gate are always visible");
  assert(!button("Review post now"), "No send action without a candidate");
  await render({ candidates: [candidate] });
  assert(text().includes("No publish grant"), "Grant gate holds with a candidate present");
  assert(button("Review post now")?.disabled, "Review stays disabled until a grant binds");
  await typeGrant("grant-a");
  assert(text().includes("dpq_ followed by") && button("Review post now")?.disabled, "A non-dpq_ grant is refused in place");
  await typeGrant("dpq_synthetic_grant_ig");
  assert(text().includes("Grant bound: dpq_synthetic_grant_ig"), "A valid grant is shown back");
  // Candidate picker + approval gate.
  await React.act(async () => { host.querySelector('button[aria-label="Approved draft"]')?.dispatchEvent(new win.MouseEvent("click", { bubbles: true })); });
  await flush();
  const option = Array.from(document.querySelectorAll('[role="option"]')).find(value => value.textContent?.includes("Synthetic caption"));
  await React.act(async () => { option?.dispatchEvent(new win.MouseEvent("click", { bubbles: true })); });
  await flush();
  assert(text().includes("@sakeboyhk"), "Exact destination is shown before any decision");
  assert(!button("Confirm and post now"), "Nothing schedules without the confirmation step");
  assert(button("Review post now")?.disabled, "An image draft cannot review before its image is imported");
  await React.act(async () => { button("Import image")?.click(); });
  await flush(); await flush();
  assert(imports.length === 1 && text().includes("does not match the reviewed image") && button("Review post now")?.disabled && scheduled.length === 0, "A mismatched import digest is refused and nothing schedules");
  importHex = reviewedHex;
  await React.act(async () => { button("Import image")?.click(); });
  await flush(); await flush();
  assert(button("Image imported") && !button("Review post now")?.disabled, "A matching import unlocks review");
  await React.act(async () => { button("Review post now")?.click(); });
  await flush();
  assert(button("Confirm and post now")?.disabled && text().includes("Loading the reviewed caption"), "Confirm stays disabled until the reviewed artifact text loads");
  await React.act(async () => { releaseArtifact?.(); });
  await flush();
  const confirm = host.querySelector('[aria-label="Confirm publish"]')?.textContent ?? "";
  assert(confirm.includes(reviewedText) && confirm.includes("sha256:artifact-digest") && !confirm.includes("Synthetic caption"), "Confirmation shows the reviewed artifact text verbatim with its digest, not the run title");
  assert(!button("Confirm and post now")?.disabled, "A loaded matching artifact enables Confirm");
  assert(confirm.includes(reviewedHex) && confirm.includes("dp1.ws.conn.") && confirm.includes("Asia/Hong_Kong") && confirm.includes("17841400008460056") && confirm.includes("instagram") && /apv-[0-9a-f]{32}/.test(confirm), "Confirmation shows caption, image digest, due, timezone, destination and the approval id");
  await React.act(async () => { button("Confirm and post now")?.click(); button("Confirm and post now")?.click(); });
  await flush(); await flush();
  assert(scheduled.length >= 1 && scheduled.every(body => body.approval_id === scheduled[0].approval_id) && text().includes("intent-0"), "A double press reuses one approval id, so the daemon keeps one intent");
  const firstApproval = scheduled[0].approval_id;
  artifactId = "artifact-other";
  await openConfirmation();
  assert(button("Confirm and post now")?.disabled && text().includes("does not match this draft"), "An artifact that is not the draft's keeps Confirm disabled");
  artifactId = "artifact-a";
  await React.act(async () => { button("Back")?.click(); });
  await flush();
  await openConfirmation();
  await typeGrant("dpq_synthetic_grant_other");
  assert(!button("Confirm and post now"), "Editing after review voids the confirmation");
  await typeGrant("dpq_synthetic_grant_ig");
  await openConfirmation();
  await React.act(async () => { button("Confirm and post now")?.click(); });
  await flush(); await flush();
  assert(scheduled[scheduled.length - 1].approval_id !== firstApproval, "A new confirmation mints a new approval id");
  refuseSchedule = true;
  await openConfirmation();
  await React.act(async () => { button("Confirm and post now")?.click(); });
  await flush(); await flush();
  assert(text().includes("already authorized another post"), "A replay refusal renders operator copy, not the raw code");
  refuseSchedule = false;
  assert(text().includes("17841400008460056") && text().includes("binding-digest") && text().includes("never relay-vouched"), "Card echoes the destination id and binding digest; the handle stays a display constant");
  // Schedule path needs a due time.
  await React.act(async () => { host.querySelector('input[value="schedule"]')?.dispatchEvent(new win.MouseEvent("click", { bubbles: true })); });
  await flush();
  assert(button("Review schedule")?.disabled, "Schedule without a due time stays gated");
  // Cancel takes two explicit presses from queued only (cancel_closed past queued).
  assert(!button("Cancel before dispatch"), "Processing intent offers no cancel");
  // An unscoped panel (contextId null) lists another context's intent; its
  // cancel must carry that intent's own frozen install and context.
  intents = [{ ...intents[0], intent_id: "intent-q", state: "queued", install_id: "install-a", context_id: "ctx-other" }];
  await render({ candidates: [candidate], });
  assert(button("Cancel before dispatch"), "Queued intent offers cancel");
  await React.act(async () => { button("Cancel before dispatch")?.click(); });
  await flush();
  assert(button("Confirm cancel"), "Cancel needs an explicit second press");
  await React.act(async () => { button("Confirm cancel")?.click(); });
  await flush(); await flush();
  assert(cancelled.includes("intent-q") && text().includes("Nothing was sent"), "Cancel closes the intent without a send");
  assert(cancelScopes.at(-1)?.[0] === "install-a" && cancelScopes.at(-1)?.[1] === "ctx-other", "Cancel uses the intent's frozen install/context, not the panel's");
  // CAD-1041: Send now takes two explicit presses on a queued intent
  // only; processing/posted offer nothing, one click never sends.
  assert(!sent.includes("intent-q"), "Cancel alone never sends");
  intents = [{ ...intents[0], intent_id: "intent-s", state: "queued" }];
  await render({ candidates: [candidate] });
  assert(button("Send now"), "Queued intent offers Send now");
  await React.act(async () => { button("Send now")?.click(); });
  await flush();
  assert(button("Confirm send now"), "Send now needs an explicit second press");
  assert(!sent.includes("intent-s"), "Arming alone sends nothing");
  await React.act(async () => { button("Confirm send now")?.click(); });
  await flush(); await flush();
  assert(sent.includes("intent-s") && text().includes("posted"), "Confirmed send posts the named intent");
  assert(sendScopes.at(-1)?.[0] === "install-a" && sendScopes.at(-1)?.[1] === "ctx-other", "Send now uses the intent's frozen install/context, not the panel's");
  intents = [{ ...intents[0], intent_id: "intent-p", state: "processing" }];
  await render({ candidates: [candidate] });
  assert(!button("Send now"), "Processing intent offers no Send now");
  // Every state renders its operator copy.
  intents = [
    { ...intents[0], intent_id: "i-posted", state: "posted", permalink: "https://www.instagram.com/p/fixture000/", receipt: { ok: true } },
    { ...intents[0], intent_id: "i-refused", state: "refused", refusal: { code: "grant_exhausted", message: "raw" } },
    { ...intents[0], intent_id: "i-processing", state: "processing", upstream: null },
    { ...intents[0], intent_id: "i-unconfirmed", state: "processing", upstream: { state: "processing" } },
    { ...intents[0], intent_id: "i-held", state: "held", refusal: { code: "", message: "dispatch authority differs from frozen approval" } },
    { ...intents[0], intent_id: "i-held-material", state: "held", refusal: { code: "", message: "approved material changed since freeze" } },
  ];
  await render({ candidates: [candidate], });
  assert(text().includes("fixture000") && text().includes("never counts"), "Posted shows the verified receipt, never a bare string");
  assert(text().includes("no uses left"), "Refused names the grant cause");
  const uncertainCount = text().split("Reads as uncertain").length - 1;
  assert(text().includes("in flight") && uncertainCount === 1, "Only the evidence-backed card shows the uncertain display");
  assert(text().includes("Reads as uncertain") && text().includes("Never duplicate"), "Processing plus lost-response evidence shows the uncertain display with reconcile-before-retry");
  assert(text().includes("dispatch authority differs from frozen approval") && text().includes("approved material changed since freeze"), "Both held literals render raw with no invented code");
  assert(text().includes("Needs a human decision") && text().includes("reconnect_needed is a separate layer"), "Held names the human; ledger reconnect_needed stays separate");
  assert(text().includes("Processing"), "Processing state renders");
  // Read-only and failure states.
  await render({ candidates: [candidate], canWrite: false });
  assert(host.querySelector("fieldset[disabled]"), "Read-only disables the whole decision fieldset");
  failList = true;
  await render({ candidates: [] });
  assert(text().includes("Operator session expired"), "Failed intent load is explicit");
  failList = false;
  await React.act(async () => root.unmount());
  console.log("social publish panel checks passed");
}
void main();
