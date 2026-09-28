export {};
/** Mounted operator flow: a retained provider receipt, then a separate caption plan. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/app-installations/install-a" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLDialogElement", "HTMLInputElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
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
const WorkspaceApp = (require("../src/features/workspace-apps/WorkspaceApp") as typeof import("../src/features/workspace-apps/WorkspaceApp")).default;
const installation = { install_id: "install-a", title: "Social Content", name: "social-content", version: "0.3.0", digest: "bundle-v3", approved: true, storage_kind: "workspace", files: ["workflows/instagram.md", "workflows/facebook.md", "workflows/source-instagram.md"] };
const binding = (slot: string, connectionId: string, provider: string) => ({ id: `binding-${slot}`, install_id: "install-a", context_id: null, slot, revision: 1, state: "configured", digest: `digest-${slot}`, config: { bundle_digest: "bundle-v3", connection_id: connectionId, provider, account: "company-a" } });
const sourceSnapshot = { owner_pm: "pm-a", inputs: { profile_handle: "juicysuite_crm", writer: "writer-a" }, workflow: { title: "Read public Instagram posts: juicysuite_crm", steps: [{ id: "1", kind: "produce_text", assignee: "writer-a", dependencies: [], instruction: "Read bound source" }] }, capabilities: { source: { id: "binding-source", revision: 1, digest: "digest-source" } }, quotes: { source: { schema: 1, currency: "USD", unit_price_micros: 2000, units: 1, total_price_micros: 2000, price_revision: "sha256:quote" } }, assignments: {} };
const sourceRun = (state = "awaiting_approval") => ({ id: "source-run", install_id: "install-a", context_id: null, state, snapshot_digest: "source-digest", approved_digest: state === "awaiting_approval" ? null : "source-digest", snapshot: sourceSnapshot, steps: [{ step_id: "1", task_id: "task-a", state: state === "succeeded" ? "succeeded" : "pending", message_id: state === "succeeded" ? "message-a" : null }], artifacts: [], reviews: [] });
const receipt = { id: "receipt-a", run_id: "source-run", slot: "source", digest: "receipt-digest", binding_digest: "digest-source", result: { schema: 1, kind: "social.source.posts", provider: "agenticos_external", source_tool: "scrapecreators.instagram.user.posts", handle: "juicysuite_crm", profile_verified: true, empty_reason: null, more_available: false, posts: [{ id: "post-a", caption: "Original line one\nOriginal line two", permalink: "https://www.instagram.com/p/AbCd123/", published_at: "2026-09-27T00:00:00Z", published_at_unix: 1790467200, media_kind: "image", preview_url: null }] } };
const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
let rows: any[] = [];
let denySource = false;
let receiptRows: any[] = [receipt];
const reads: string[] = [];
const writes: { path: string; body: any }[] = [];
globalThis.fetch = async (input, init) => {
  const path = String(input);
  if (init?.method === "POST") {
    const body = JSON.parse(String(init.body)); writes.push({ path, body });
    if (path === "/api/app-runs") {
      if (body.workflow === "source-instagram") {
        rows = [sourceRun()]; return json(rows[0]);
      }
      if (body.workflow === "instagram") {
        assert(body.source_receipt_id === "receipt-a" && body.selected_post_id === "post-a", "Caption plan pins exact selected source");
        assert(!Object.hasOwn(body.inputs, "source"), "Caption plan cannot overwrite server-derived source text");
        return json({ ...sourceRun(), id: "caption-run", snapshot: { ...sourceSnapshot, inputs: { ...body.inputs, source: "Original line one Original line two" } } });
      }
    }
    if (path === "/api/app-runs/source-run/approve") { rows = [sourceRun("approved")]; return json(rows[0]); }
    if (path === "/api/app-runs/source-run/dispatch") { rows = [sourceRun("running")]; return json(rows[0]); }
    throw new Error(`Unexpected mutation ${path}`);
  }
  reads.push(path);
  if (denySource && path === "/api/app-runs/source-run/capability-results") return json({ error: "Operator session expired" }, 403);
  if (path === "/api/app-installations/install-a") return json(installation);
  if (path === "/api/app-installations/install-a/contexts") return json({ contexts: [] });
  if (path === "/api/app-installations/install-a/bindings") return json({ bindings: [binding("publication", "local-id", "local"), binding("source", "provider-id", "agenticos_external")] });
  if (path === "/api/connections") return json({ connections: [{ id: "local-id", provider: "local", account: "local", status: { manifest_status: "matched", custody_available: true } }, { id: "provider-id", provider: "agenticos_external", account: "company-a", status: { manifest_status: "matched", custody_available: true }, descriptor: { action_mappings: [{ capability: "social.read", action: "list_posts", effect: "read" }] } }] });
  if (path === "/api/agents") return json({ agents: [{ alias: "pm-a", role: "pm", group: "pm-a", state: "idle" }, { alias: "writer-a", role: "worker", group: "pm-a", provider: "codex", endpoint_kind: "managed", state: "idle" }, { alias: "reviewer-a", role: "worker", group: "pm-a", provider: "codex", endpoint_kind: "managed", state: "idle" }] });
  if (path === "/api/app-runs?install_id=install-a") return json({ runs: rows });
  if (path === "/api/app-installations/install-a/effects") return json({ effects: [] });
  if (path === "/api/app-installations/install-a/bindings/source/quote") return json({ slot: "source", binding_digest: "digest-source", quote_digest: "quote-digest", quote: sourceSnapshot.quotes.source });
  if (path === "/api/app-runs/source-run/capability-results") return json({ results: receiptRows });
  throw new Error(`Unexpected read ${path}`);
};
win.setInterval = () => 1; win.clearInterval = () => {};
const host = document.createElement("div"); document.body.append(host);
let root = createRoot(host);
function assert(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }
const flush = () => React.act(async () => { await new Promise(resolve => setTimeout(resolve, 0)); });
const button = (label: string) => Array.from(host.querySelectorAll("button")).find(value => value.textContent?.trim() === label);
async function click(element: Element | undefined | null) {
  assert(element, "Click target exists");
  await React.act(async () => { element.dispatchEvent(new MouseEvent("click", { bubbles: true })); }); await flush();
}
async function fill(selector: string, value: string) {
  const element = host.querySelector(selector) as HTMLInputElement | HTMLTextAreaElement;
  assert(element, "Field exists");
  const prototype = element instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  await React.act(async () => { Object.getOwnPropertyDescriptor(prototype, "value")!.set!.call(element, value); element.dispatchEvent(new Event("input", { bubbles: true })); });
}
async function choose(id: string, label: string) {
  await click(host.querySelector(`#${id}`));
  const option = Array.from(document.querySelectorAll('[role="option"]')).find(value => value.textContent?.includes(label));
  assert(option?.closest("dialog") === host.querySelector("dialog"), "Options stay inside the active dialog");
  await click(option);
}
async function main() {
  await React.act(async () => root.render(React.createElement(WorkspaceApp, { installId: "install-a", viewer: { operator: true, readOnly: false } }))); await flush();
  await click(button("Sources"));
  assert(host.textContent?.includes("No source reads yet"), "No source card is invented before a provider receipt");
  await click(button("Find public posts"));
  assert(host.textContent?.includes("USD 0.002000"), "Operator sees exact current source charge before planning a paid read");
  await fill("#wa-profile-handle", "@juicysuite_crm"); await choose("wa-source-owner", "pm-a"); await choose("wa-source-reader", "writer-a");
  await click(button("Create source plan"));
  assert(writes.length === 1 && writes[0].body.workflow === "source-instagram" && writes[0].body.inputs.profile_handle === "juicysuite_crm", "Source plan freezes operator-selected handle");
  assert(!Object.hasOwn(writes[0].body, "company") && !Object.hasOwn(writes[0].body, "tool"), "Board cannot name provider company or tool");
  assert(!reads.includes("/api/app-runs/source-run/capability-results"), "No provider receipt exists before dispatch and success");
  await click(button("Approve source plan")); await click(button("Start source reader"));
  assert(!reads.includes("/api/app-runs/source-run/capability-results"), "Dispatch is not a fabricated provider result");
  await click(host.querySelector('button[aria-label="Close Read @juicysuite_crm"]'));
  rows = [sourceRun("succeeded")]; await click(button("Refresh")); await flush();
  assert(reads.includes("/api/app-runs/source-run/capability-results") && host.textContent?.includes("Original line one"), "Only the stored broker receipt populates source posts");
  const readsBeforeFailure = reads.filter(path => path === "/api/app-runs/source-run/capability-results").length;
  rows = [sourceRun("failed")]; await click(button("Refresh")); await flush();
  assert(reads.filter(path => path === "/api/app-runs/source-run/capability-results").length > readsBeforeFailure,
    "A failed run still fetches its retained broker receipt");
  assert(host.textContent?.includes("Original line one") && button("Use as source"),
    "A later run failure does not discard an already retained paid read");
  assert(host.textContent?.includes("failed after the retained read"),
    "The failed run is still clearly identified when its verified posts are recoverable");
  receiptRows = []; await click(button("Board")); await click(button("Sources"));
  assert(!button("Use as source") && host.textContent?.includes("failed without a usable retained read"),
    "A failed run without a broker receipt does not fabricate a recoverable post");
  receiptRows = [receipt]; rows = [{ ...sourceRun("failed"), approved_digest: null }];
  await click(button("Refresh")); await flush();
  assert(!button("Use as source"), "A failed run without its matching approval cannot expose a source");
  rows = [sourceRun("succeeded")]; await click(button("Refresh")); await flush();
  receiptRows = [{ ...receipt, result: { ...receipt.result, posts: [], profile_verified: true, empty_reason: "no_public_posts" } }];
  await click(button("Board")); await click(button("Sources"));
  assert(host.textContent?.includes("No public posts were returned for this verified profile"), "A verified empty provider page has an explicit honest state");
  assert(!button("Use as source"), "An empty provider page cannot offer a source selection");
  receiptRows = [{ ...receipt, run_id: "other-run" }]; await click(button("Sources")); await click(button("Board")); await click(button("Sources"));
  assert(!button("Use as source"), "Receipt from another run cannot populate the source picker");
  receiptRows = [{ ...receipt, binding_digest: "other-binding" }];
  await click(button("Board")); await click(button("Sources"));
  assert(!button("Use as source"), "Receipt from another frozen provider binding cannot populate the picker");
  receiptRows = [{ ...receipt, result: { ...receipt.result, posts: [{ ...receipt.result.posts[0], preview_url: "https://attacker.example/track" }] } }];
  await click(button("Board")); await click(button("Sources"));
  assert(!host.querySelector(".wa-source-image"), "A tampered receipt cannot load an arbitrary browser image URL");
  assert(!!host.querySelector(".wa-source-image-fallback") && host.textContent?.includes("View original post"), "Unavailable previews retain a clear path to inspect the original");
  receiptRows = [receipt]; await click(button("Board")); await click(button("Sources"));
  await click(button("Use as source"));
  const sourceField = host.querySelector("#wa-post-source") as HTMLTextAreaElement;
  assert(sourceField.readOnly && sourceField.value.includes("Original line two"), "Selected raw caption is inspectable but not editable");
  await fill("#wa-post-title", "New social caption"); await choose("wa-workflow", "Instagram caption"); await choose("wa-owner", "pm-a"); await choose("wa-writer", "writer-a"); await choose("wa-reviewer", "reviewer-a");
  await click(button("Create plan"));
  assert(writes.some(value => value.body.workflow === "instagram"), "Selecting a post creates a separate caption plan");
  denySource = true; await click(button("Board")); await click(button("Sources"));
  assert(!host.textContent?.includes("Original line one") && !button("Find public posts"), "403 wipes stored source posts and operator controls");
  await React.act(async () => root.unmount()); root = createRoot(host); reads.length = 0;
  await React.act(async () => root.render(React.createElement(WorkspaceApp, { installId: "install-a", viewer: { operator: false, readOnly: false } }))); await flush();
  assert(reads.length === 0, "Unproven viewer never requests private provider receipts");
  await React.act(async () => root.unmount());
  console.log("social source mounted flow passed");
}
void main();
