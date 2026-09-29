export {};
/** CAD-787 mounted panel QA with an injected stub client — no fetch, no
 *  provider, no live post. Proves the approve gate, now-vs-schedule choice,
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
let intents: any[] = [];
let failList = false;
const scheduled: any[] = [];
const cancelled: string[] = [];
const stub = {
  list: async () => { if (failList) throw new Error("Operator session expired"); return { intents }; },
  show: async (intentId: string) => ({ intent: intents.find(value => value.intent_id === intentId) }),
  schedule: async (body: any) => {
    assert(body.slot === "publication" && body.timezone === "Asia/Hong_Kong" && body.destination_id === "17841400008460056", "Panel schedules exact pilot destination with timezone");
    assert(!Object.hasOwn(body, "caption_digest"), "Panel never invents digests");
    const intent = { intent_id: `intent-${scheduled.length}`, install_id: "install-a", context_id: null, run_id: body.run_id, effect_id: body.effect_id, state: body.due_epoch <= Math.floor(Date.now() / 1000) + 5 ? "processing" : "queued", channel: body.toolkit, destination_id: body.destination_id, destination_handle: "@sakeboyhk", caption_digest: "c-digest", image_digest: "i-digest", destination_digest: "d-digest", idempotency_key: `key-${scheduled.length}`, due_epoch: body.due_epoch, timezone: body.timezone, grant_id: body.grant_id, approval_id: body.approval_id, writer: "writer-a", reviewer: "reviewer-a", permalink: null, receipt: null, refusal: null };
    scheduled.push(body); intents = [intent, ...intents];
    return { intent };
  },
  cancel: async (intentId: string) => { cancelled.push(intentId); intents = intents.map(value => value.intent_id === intentId ? { ...value, state: "cancelled" } : value); return { intent: intents.find(value => value.intent_id === intentId) }; },
};
let mounts = 0;
async function render(props: any) {
  await React.act(async () => { root.render(React.createElement(PublishPanel, { key: ++mounts, installId: "install-a", contextId: null, candidates: [], grantId: "", approvalId: "", canWrite: true, client: stub, ...props })); });
  await flush();
}
async function main() {
  await render({});
  assert(text().includes("No publish grant") && text().includes("No approved drafts"), "Missing grant and drafts render honest empty states");
  assert(text().includes("1–10 uses") && text().includes("send_disabled"), "Grant-use range and disabled gate are always visible");
  assert(!button("Post now"), "No send action without a candidate");
  await render({ candidates: [candidate] });
  assert(text().includes("No publish grant"), "Grant gate holds with a candidate present");
  assert(button("Post now")?.disabled, "Post now stays disabled until a grant binds");
  await render({ candidates: [candidate], grantId: "grant-a", approvalId: "op-a" });
  // Candidate picker + approval gate.
  await React.act(async () => { host.querySelector('button[aria-label="Approved draft"]')?.dispatchEvent(new win.MouseEvent("click", { bubbles: true })); });
  await flush();
  const option = Array.from(document.querySelectorAll('[role="option"]')).find(value => value.textContent?.includes("Synthetic caption"));
  await React.act(async () => { option?.dispatchEvent(new win.MouseEvent("click", { bubbles: true })); });
  await flush();
  assert(text().includes("@sakeboyhk"), "Exact destination is shown before any decision");
  const postNow = button("Post now");
  assert(postNow?.disabled, "Post now is gated on explicit approval");
  const approve = host.querySelector('input[aria-label="Approve the exact destination and content digests"]') as HTMLInputElement;
  await React.act(async () => { approve.click(); });
  await flush();
  assert(!button("Post now")?.disabled, "Approval unlocks Post now");
  await React.act(async () => { button("Post now")?.click(); });
  await flush(); await flush();
  assert(scheduled.length === 1 && text().includes("intent-0"), "Post now schedules with due at now");
  // Schedule path needs a due time.
  await React.act(async () => { host.querySelector('input[value="schedule"]')?.dispatchEvent(new win.MouseEvent("click", { bubbles: true })); });
  await flush();
  assert(button("Schedule")?.disabled, "Schedule without a due time stays gated");
  // Cancel takes two explicit presses from queued only (cancel_closed past queued).
  assert(!button("Cancel before dispatch"), "Processing intent offers no cancel");
  intents = [{ ...intents[0], intent_id: "intent-q", state: "queued" }];
  await render({ candidates: [candidate], grantId: "grant-a", approvalId: "op-a" });
  assert(button("Cancel before dispatch"), "Queued intent offers cancel");
  await React.act(async () => { button("Cancel before dispatch")?.click(); });
  await flush();
  assert(button("Confirm cancel"), "Cancel needs an explicit second press");
  await React.act(async () => { button("Confirm cancel")?.click(); });
  await flush(); await flush();
  assert(cancelled.includes("intent-q") && text().includes("Nothing was sent"), "Cancel closes the intent without a send");
  // Every state renders its operator copy.
  intents = [
    { ...intents[0], intent_id: "i-posted", state: "posted", permalink: "https://www.instagram.com/p/fixture000/", receipt: { ok: true } },
    { ...intents[0], intent_id: "i-refused", state: "refused", refusal: { code: "grant_exhausted", message: "raw" } },
    { ...intents[0], intent_id: "i-processing", state: "processing" },
    { ...intents[0], intent_id: "i-held", state: "held", refusal: { code: "grant_window", message: "" } },
  ];
  await render({ candidates: [candidate], grantId: "grant-a", approvalId: "op-a" });
  assert(text().includes("fixture000") && text().includes("never counts"), "Posted shows the verified receipt, never a bare string");
  assert(text().includes("no uses left"), "Refused names the grant cause");
  assert(text().includes("read this as uncertain") && text().includes("Never duplicate"), "Processing carries the uncertain reading with reconcile-before-retry");
  assert(text().includes("validity window") && text().includes("Needs a human decision"), "Held names the cause and the human, never a reconnect state");
  assert(text().includes("Processing"), "Processing state renders");
  await render({ candidates: [candidate], grantId: "grant-a", approvalId: "x".repeat(121) });
  assert(text().includes("grant_approval") && button("Post now")?.disabled, "Oversize approval gates with its contract code");
  // Read-only and failure states.
  await render({ candidates: [candidate], grantId: "grant-a", approvalId: "op-a", canWrite: false });
  assert(host.querySelector("fieldset[disabled]"), "Read-only disables the whole decision fieldset");
  failList = true;
  await render({ candidates: [], grantId: "g", approvalId: "a" });
  assert(text().includes("Operator session expired"), "Failed intent load is explicit");
  failList = false;
  await React.act(async () => root.unmount());
  console.log("social publish panel checks passed");
}
void main();
