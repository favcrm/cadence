export {};
/** Mounted first-flow QA. Synthetic HTTP receipts use the stored workspace contract. */
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
const installation = { install_id: "install-a", title: "Social Content", name: "social-content", version: "0.2.0", digest: "bundle-digest", catalog_generation: "catalog-generation", approved: true, storage_kind: "workspace", files: ["workflows/instagram.md", "workflows/facebook.md"], capabilities: { publication: { schema: 1, capability: "text.publish", version: 1, action: "publish", resource_kind: "connection_account", effect: "send" } }, connection_slots: ["cms"] };
let simulatedInstallation: typeof installation | null = null;
let loseUpgradeReply = false;
const snapshot = { owner_pm: "pm-a", inputs: { subject: "Synthetic caption", source: "Synthetic source facts", writer: "writer-a", reviewer: "reviewer-a" }, workflow: { title: "Instagram caption: Synthetic caption", publication_slot: "publication", steps: [{ id: "1", kind: "produce_text", assignee: "writer-a", dependencies: [], instruction: "Write" }, { id: "2", kind: "review_text", assignee: "reviewer-a", dependencies: ["1"], instruction: "Review" }] }, publication: { slot: "publication", binding: { id: "binding-a", revision: 1, digest: "binding-digest" } }, assignments: {} };
let rows: any[] = [];
let effects: any[] = [];
const acceptedRun = () => ({ id: "run-a", install_id: "install-a", context_id: null, state: "succeeded", snapshot_digest: "frozen-digest", approved_digest: "frozen-digest", snapshot, steps: [{ step_id: "1", state: "succeeded", task_id: "task-1", message_id: "message-1" }, { step_id: "2", state: "succeeded", task_id: "task-2", message_id: "message-2" }], artifacts: [{ id: "artifact-a", step_id: "1", digest: "text-digest", media_type: "text/plain", size: 13 }], reviews: [{ step_id: "2", artifact_digest: "text-digest", reviewer: "reviewer-a", decision: "approve", rationale: "Source and disclaimer checked" }] });
const waitingEffect = () => ({ effect_id: "effect-a", digest: "exact-release-digest", authorization_kind: "app_artifact", state: "waiting", needs_you: true, authority: { install_id: "install-a", run_id: "run-a", context: null, artifact_id: "artifact-a", artifact_digest: "text-digest", binding: { id: "binding-a", revision: 1, digest: "binding-digest" } }, record: { title: "Synthetic caption", input: { text: "Reviewed text" }, preview: "Reviewed text" } });
const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
const reads: string[] = [];
const writes: { path: string; body: any }[] = [];
let holdReads = false;
let denyReads = false;
let denyStage = false;
let held: { path: string; resolve: (value: Response) => void }[] = [];
let releaseError = false;
let holdWrite = false;
let pendingWrite: (() => void) | null = null;
let loseCreateReply = false;
function read(path: string): Response {
  if (denyReads) return json({ error: "Operator session expired" }, 403);
  if (path === "/api/app-installations/install-a") return json(simulatedInstallation ?? installation);
  if (path === "/api/app-installations/install-a/contexts") return json({ contexts: [{ id: "context-b", install_id: "install-a", revision: 1, state: "active", digest: "brand-digest", config: { schema: 1, label: "Brand B", input_defaults: {} } }] });
  if (path === "/api/app-installations/install-a/bindings") return json({ bindings: [{ id: "binding-a", install_id: "install-a", context_id: null, slot: "publication", revision: 1, state: "configured", digest: "binding-digest", config: { bundle_digest: "bundle-digest", connection_id: "local-id", account: "default" } }] });
  if (path === "/api/connections") return json({ connections: [
    { id: "local-id", provider: "local", account: "local", kind: "builtin", scopes: [], descriptor: { action_mappings: [{ capability: "text.publish", version: 1, action: "publish", resource_kind: "connection_account", effect: "send", scopes: [] }] }, status: { manifest_status: "matched", custody_available: true, adapter_registered: true } },
    { id: "conn-extra", provider: "local", account: "extra", kind: "builtin", scopes: [], descriptor: { action_mappings: [{ capability: "text.publish", version: 1, action: "publish", resource_kind: "connection_account", effect: "send", scopes: [] }] }, status: { manifest_status: "matched", custody_available: true, adapter_registered: true } },
  ] });
  if (path === "/api/agents") return json({ agents: [{ alias: "pm-a", role: "pm", group: "pm-a", state: "idle" }, { alias: "writer-a", role: "worker", group: "pm-a", provider: "codex", endpoint_kind: "managed", state: "idle" }, { alias: "reviewer-a", role: "worker", group: "pm-a", provider: "codex", endpoint_kind: "managed", state: "idle" }] });
  if (path === "/api/app-runs?install_id=install-a") return json({ runs: rows });
  if (path === "/api/app-installations/install-a/effects") return json({ effects });
  if (path === "/api/app-run-artifacts/artifact-a") return json({ id: "artifact-a", digest: "text-digest", media_type: "text/plain", size: 13, text: "Reviewed text" });
  if (path === "/api/outbox?effect_id=effect-a") return json({ item: { effect_id: "effect-a", project: null, scope: "app", post: "Reviewed text", provenance: { run_id: "run-a" } } });
  // CAD-980 installed Schedule: the calendar reads publish intents (empty here).
  if (path.startsWith("/api/social-publishes")) return json({ intents: [] });
  throw new Error(`Unexpected read ${path}`);
}
globalThis.fetch = async (input, init) => {
  const path = String(input);
  if (init?.method === "POST") {
    const body = JSON.parse(String(init.body)); writes.push({ path, body });
    const respond = () => {
      if (path === "/api/app-installations/install-a/upgrade/check") {
        if (simulatedInstallation) {
          const target = body.source === "/tmp/social-v04" ? "bundle-digest" : "new-bundle-digest";
          assert(body.expected_digest === simulatedInstallation.digest && body.expected_generation === simulatedInstallation.catalog_generation && target !== simulatedInstallation.digest, "Repeat upgrade check pins the active version and generation");
          return json({ install_id: "install-a", name: "social-content", version: body.source === "/tmp/social-v04" ? "0.4.0" : "0.5.0", digest: target, expected_digest: body.expected_digest, expected_generation: body.expected_generation, structural_diff: { added: [], changed: ["app.md"], removed: [] }, secret_warnings: [] });
        }
        assert(body.source === "/tmp/social-v05" && body.expected_digest === "bundle-digest" && body.expected_generation === "catalog-generation", "Upgrade check pins current catalog and bundle");
        return json({ install_id: "install-a", name: "social-content", version: "0.5.0", digest: "new-bundle-digest", expected_digest: "bundle-digest", expected_generation: "catalog-generation", structural_diff: { added: ["workflows/image-manual.md"], changed: ["app.md"], removed: [] }, secret_warnings: [] });
      }
      if (path === "/api/app-installations/install-a/upgrade") {
        if (simulatedInstallation) {
          assert(body.expected_digest === simulatedInstallation.digest && body.expected_generation === simulatedInstallation.catalog_generation && body.expected_new_digest !== simulatedInstallation.digest, "Repeat upgrade apply matches its reviewed current version");
          if (loseUpgradeReply) { loseUpgradeReply = false; throw new Error("Upgrade reply lost"); }
          simulatedInstallation = { ...simulatedInstallation, digest: body.expected_new_digest, catalog_generation: body.expected_new_digest === "bundle-digest" ? "catalog-generation" : "generation-b", approved: false };
          return json(simulatedInstallation);
        }
        assert(body.source === "/tmp/social-v05" && body.expected_digest === "bundle-digest" && body.expected_generation === "catalog-generation" && body.expected_new_digest === "new-bundle-digest" && body.request_id, "Upgrade apply sends only the reviewed old/new material and a stable request");
        return json({ ...installation, digest: "new-bundle-digest", approved: false });
      }
      if (path === "/api/app-installations/install-a/bindings") return json({ binding: { id: `binding-${writes.length}`, install_id: "install-a", context_id: body.context_id ?? null, slot: body.slot, revision: 1, state: "configured", digest: `binding-digest-${writes.length}`, config: { bundle_digest: (simulatedInstallation ?? installation).digest, connection_id: body.connection_id, account: "default" } } });
      if (path === "/api/app-runs") {
        assert(body.inputs.source === "Synthetic source facts" && !Object.hasOwn(body.inputs, "source_text"), "Create uses exact supported package input source");
        assert(!Object.hasOwn(body, "context_id") && !Object.hasOwn(body, "project_link"), "No brand and no project require no synthetic ownership fields");
        rows = [{ ...acceptedRun(), state: "awaiting_approval", approved_digest: null, artifacts: [], reviews: [], steps: acceptedRun().steps.map(step => ({ ...step, state: "pending", message_id: null })) }];
        if (loseCreateReply) { loseCreateReply = false; throw new Error("Connection lost after create"); }
        return json(rows[0]);
      }
      if (path === "/api/app-runs/run-a/approve") {
        assert(body.digest === "frozen-digest", "Plan approval uses exact server snapshot digest");
        rows = [{ ...rows[0], state: "approved", approved_digest: "frozen-digest" }]; return json(rows[0]);
      }
      if (path === "/api/app-runs/run-a/dispatch") {
        rows = [{ ...rows[0], state: "running" }]; return json(rows[0]);
      }
      if (path === "/api/app-runs/run-a/effects") {
        if (denyStage) return json({ error: "Operator session expired" }, 403);
        effects = [waitingEffect()]; return json({ effect: effects[0] });
      }
      if (path === "/api/app-effects/effect-a/decide") {
        if (releaseError) return json({ error: "Binding revision changed" }, 409);
        effects = [{ ...waitingEffect(), state: "done", needs_you: false, record: { ...waitingEffect().record, outcome: { kind: "succeeded", verified: true, result: { effect_id: "effect-a" } } } }];
        return json({ effect: effects[0] });
      }
      if (path === "/api/app-effects/effect-a/resolve") {
        assert(body.digest === "exact-release-digest" && body.resolution === "close", "Uncertain outcome resolution pins historical digest");
        effects = [{ ...effects[0], state: "closed", needs_you: false }]; return json({ effect: effects[0] });
      }
      throw new Error(`Unexpected write ${path}`);
    };
    if (holdWrite) return new Promise<Response>(resolve => { pendingWrite = () => resolve(respond()); });
    return respond();
  }
  reads.push(path);
  if (holdReads) return new Promise<Response>(resolve => held.push({ path, resolve }));
  return read(path);
};
win.setInterval = () => 1;
win.clearInterval = () => {};
const host = document.createElement("div"); document.body.append(host);
let root = createRoot(host);
function assert(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }
const flush = () => React.act(async () => { await new Promise(resolve => setTimeout(resolve, 0)); });
const text = () => host.textContent ?? "";
const button = (label: string) => Array.from(host.querySelectorAll("button")).find(value => value.textContent?.trim() === label);
async function click(element: Element | undefined | null) {
  assert(element, "Click target exists");
  await React.act(async () => { element.dispatchEvent(new MouseEvent("click", { bubbles: true })); }); await flush();
}
async function render(operator = true, readOnly = false) {
  await React.act(async () => root.render(React.createElement(WorkspaceApp, { installId: "install-a", viewer: { operator, readOnly } }))); await flush();
}
async function fresh(operator = true, readOnly = false) {
  await React.act(async () => root.unmount()); root = createRoot(host); reads.length = writes.length = 0; await render(operator, readOnly);
}
async function fill(selector: string, value: string) {
  const element = host.querySelector(selector) as HTMLInputElement | HTMLTextAreaElement;
  assert(element, "Field exists");
  const prototype = element instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  await React.act(async () => {
    Object.getOwnPropertyDescriptor(prototype, "value")!.set!.call(element, value);
    element.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
async function choose(id: string, label: string) {
  await click(host.querySelector(`#${id}`));
  const option = Array.from(document.querySelectorAll('[role="option"]')).find(value => value.textContent?.includes(label));
  assert(option?.closest("dialog") === host.querySelector("dialog"), "Modal combobox options stay inside the active dialog instead of inert page body");
  await click(option);
}
async function choosePage(label: string, optionLabel: string) {
  await click(host.querySelector(`button[aria-label="${label}"]`));
  await click(Array.from(document.querySelectorAll(`[role="listbox"][aria-label="${label}"] [role="option"]`)).find(value => value.textContent?.includes(optionLabel)));
}
async function main() {
  holdReads = true; await render();
  assert(text().includes("Loading this workspace app"), "Unresolved app load has a visible loading state");
  assert(!button("New post"), "Unresolved authority offers no run mutation");
  holdReads = false;
  await React.act(async () => { held.splice(0).forEach(value => value.resolve(read(value.path))); }); await flush();
  assert(text().includes("No posts in drafting"), "Empty board shows real empty state");
  assert(button("New post")?.disabled && text().includes("Choose and save a Local destination"), "First visit selects the sole active brand, which cannot borrow the context-free destination");
  await click(host.querySelector('button[aria-label="Optional brand context"]'));
  await click(Array.from(document.querySelectorAll('[role="option"]')).find(value => value.textContent?.includes("No brand context")));
  assert(!button("New post")?.disabled, "Explicit No brand context reveals the actual Local binding");
  // The shell's header-row context choice is announced through the remembered
  // selection; the workspace follows it (CAD-1177).
  await React.act(async () => { require("../src/features/workspace-apps/contextSelection").rememberContext("install-a", "context-b"); });
  await flush();
  assert((host.querySelector('button[aria-label="Optional brand context"]')?.textContent ?? "").includes("Brand B"), "An externally chosen context is the workspace's context");
  await choosePage("Optional brand context", "No brand context");
  await click(host.querySelector('button[aria-label="Optional brand context"]'));
  await click(Array.from(document.querySelectorAll('[role="option"]')).find(value => value.textContent?.includes("Brand B")));
  assert(button("New post")?.disabled && text().includes("Choose and save a Local destination"), "A brand cannot borrow the context-free destination binding");
  await click(host.querySelector('button[aria-label="Optional brand context"]'));
  await click(Array.from(document.querySelectorAll('[role="option"]')).find(value => value.textContent?.includes("No brand context")));
  await click(button("Schedule"));
  await flush();
  assert(text().includes("Schedule") && text().includes("No posts are planned"), "Installed Schedule renders the real calendar's honest empty state");
  assert(writes.length === 0, "Loading and schedule navigation cause zero side effects");
  await click(button("New post"));
  await fill("#wa-post-title", "Synthetic caption"); await fill("#wa-post-source", "Synthetic\nsource facts");
  await click(button("Create plan"));
  assert(text().includes("without line breaks") && writes.length === 0, "Unsupported multiline input is refused before creating a run");
  await fill("#wa-post-source", "Synthetic source facts");
  await choose("wa-workflow", "Instagram caption"); await choose("wa-owner", "pm-a"); await choose("wa-writer", "writer-a"); await choose("wa-reviewer", "reviewer-a");
  loseCreateReply = true;
  await click(button("Create plan"));
  assert(Number(writes.length) === 1 && text().includes("Connection lost after create"), "Lost create reply stays visible with no automatic create retry");
  const firstCreateId = writes[0].body.request_id;
  await click(button("Create plan"));
  assert(Number(writes.length) === 2 && writes[1].body.request_id === firstCreateId && text().includes("Approve this plan"), "Explicit create retry retains its identity and creates a stored plan awaiting approval");
  assert(!button("Prepare Local release"), "A proposed plan has no fake artifact or synchronous review result");
  await click(button("Approve this plan"));
  assert(Number(writes.length) === 3 && button("Start writer and reviewer"), "Approval alone never dispatches workers");
  await click(button("Start writer and reviewer"));
  assert(Number(writes.length) === 4 && !text().includes("Reviewer accepted"), "Dispatch waits for actual run evidence instead of fixture review");
  await click(host.querySelector('button[aria-label="Close Synthetic caption"]'));
  await click(button("New post"));
  await fill("#wa-post-title", "Synthetic caption"); await fill("#wa-post-source", "Synthetic source facts");
  await choose("wa-workflow", "Instagram caption"); await choose("wa-owner", "pm-a"); await choose("wa-writer", "writer-a"); await choose("wa-reviewer", "reviewer-a");
  await click(button("Create plan"));
  assert(Number(writes.length) === 5 && writes[4].body.request_id !== firstCreateId, "A later deliberately created identical post gets a new intent after confirmed creation");
  await click(host.querySelector('button[aria-label="Close Synthetic caption"]'));
  writes.length = 0;
  rows = [acceptedRun()]; await click(button("Refresh")); await click(button("Library"));
  assert(host.querySelector(".wa-post-card")?.textContent?.includes("Reviewed text"), "Library card previews the accepted stored caption rather than source facts");
  const card = host.querySelector(".wa-post-card"); await click(card);
  assert(host.querySelector("dialog")?.open, "Post inspection uses an opened native dialog");
  assert(text().includes("Reviewer accepted") && text().includes("Reviewed text"), "Library opens real persisted review and immutable artifact");
  assert(text().includes("Synthetic source facts"), "Frozen review shows source facts from the package's actual source input");
  assert(writes.length === 0, "Artifact inspection does not stage or release automatically");
  holdWrite = true;
  const prepare = button("Prepare Local release");
  await React.act(async () => { assert(prepare, "Prepare control exists"); prepare.dispatchEvent(new MouseEvent("click", { bubbles: true })); prepare.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  for (let i = 0; i < 5 && !pendingWrite; i++) await flush();
  assert(Number(writes.length) === 1, "Repeated stage presses share the synchronous mutation lock");
  assert(writes[0].body.artifact_id === "artifact-a" && !Object.hasOwn(writes[0].body, "text"), "Staging identifies stored artifact instead of passing editable caption");
  assert(writes[0].body.request_id, "Staging has a retained request identity");
  const firstStageId = writes[0].body.request_id;
  holdWrite = false; await React.act(async () => { assert(pendingWrite, "Stage is pending"); pendingWrite(); pendingWrite = null; }); await flush();
  assert(text().includes("exact-release-digest") && button("Release this text to Local"), "Explicit server digest is presented before release");
  assert(Number(writes.length) === 1, "Staging never auto-accepts a release");
  releaseError = true; await click(button("Release this text to Local"));
  assert(text().includes("Binding revision changed"), "Stale binding refusal is visible");
  assert(Number(writes.length) === 2 && writes[1].body.digest === "exact-release-digest", "Release sends observed exact digest once, with no automatic changed-authority retry");
  releaseError = false; await click(button("Release this text to Local"));
  assert(text().includes("Actual Local item") && text().includes("Persisted item and retained verification"), "Release shows matching durable Local item and honest verification boundary");
  assert(reads.includes("/api/outbox?effect_id=effect-a"), "Receipt is fetched using exact effect identity");
  await click(host.querySelector('button[aria-label="Close Synthetic caption"]'));
  await fresh(); await click(button("Library")); await click(host.querySelector(".wa-post-card")); await click(button("Prepare Local release"));
  assert(writes[0].body.request_id === firstStageId, "Remount retains stage request ID and cannot duplicate logical release preparation");
  effects = [{ ...waitingEffect(), state: "reconcile", record: { ...waitingEffect().record, outcome: { kind: "uncertain", verified: true } } }];
  await fresh(); await click(host.querySelector('button[aria-label="Optional brand context"]'));
  await click(Array.from(document.querySelectorAll('[role="option"]')).find(value => value.textContent?.includes("No brand context")));
  await click(button("Library")); await click(host.querySelector(".wa-post-card"));
  assert(text().includes("completion is uncertain") && !button("Release this text to Local"), "Positive retained verification cannot turn uncertain completion into success or a resend button");
  await click(button("Close after checking outcome"));
  assert(Number(writes.length) === 1 && writes[0].path.endsWith("/resolve"), "Closing uncertainty resolves historical receipt without resending text");
  await fresh(true, true);
  assert(button("New post")?.disabled, "Read-only operator cannot create posts");
  await click(button("Settings"));
  assert(!host.querySelector("#wa-default-content-prompt") && !host.querySelector("#wa-default-image-prompt") && text().includes("needs an upgrade"), "Older installed bundles do not offer prompt defaults their workflow cannot save");
  assert(button("Save publication connection")?.disabled && button("Add brand")?.disabled && button("Check update")?.disabled, "Read-only settings have no enabled mutation controls");
  await click(button("Save publication connection")); await click(button("Add brand"));
  assert(writes.length === 0, "Read-only attempted actions cause zero POSTs");
  await fresh(); await click(button("Settings"));
  assert(!button("Apply checked update"), "No package applies before a reviewed check");
  await fill('input[placeholder="/absolute/path/to/app or https://github.com/owner/repo"]', "/tmp/social-v05");
  await click(button("Check update"));
  assert(Number(writes.length) === 1 && text().includes("new-bundle-digest") && text().includes("workflows/image-manual.md"), "Board shows exact proposed digest and changed files before apply");
  await click(button("Apply checked update"));
  assert(Number(writes.length) === 2 && writes[1].path.endsWith("/upgrade"), "Only an explicit second action applies the checked upgrade");
  const firstUpgradeId = writes[1].body.request_id;
  await fresh(false, false);
  assert(reads.length === 0, "Unproven viewer must not request operator-only app, connection, artifact or effect receipts");
  await fresh(); await click(button("Library")); await click(host.querySelector(".wa-post-card"));
  assert(text().includes("Reviewed text"), "Private draft is populated before credential loss");
  await render(false, false);
  assert(!text().includes("Reviewed text") && !text().includes("Synthetic source facts"), "Operator-to-unproven transition immediately removes private draft and source");
  await fresh(); denyReads = true; await click(button("Refresh"));
  assert(!text().includes("Synthetic source facts") && !button("New post"), "Explicit HTTP403 removes private snapshots and mutation controls");
  denyReads = false;
  await fresh(); await click(host.querySelector('button[aria-label="Optional brand context"]'));
  await click(Array.from(document.querySelectorAll('[role="option"]')).find(value => value.textContent?.includes("No brand context")));
  await click(button("Library")); await click(host.querySelector(".wa-post-card"));
  holdReads = true; await click(button("Refresh"));
  const stale = held.splice(0);
  holdReads = false; denyStage = denyReads = true;
  await click(button("Prepare Local release"));
  assert(!button("New post") && !text().includes("Reviewed text"), "Mutation403 clears snapshots while an older poll remains pending");
  denyStage = denyReads = false;
  await React.act(async () => { stale.forEach(value => value.resolve(read(value.path))); }); await flush();
  assert(!button("New post") && !text().includes("Reviewed text"), "A late successful pre-refusal poll cannot restore revoked private data");
  simulatedInstallation = { ...installation };
  await fresh();
  await click(host.querySelector('button[aria-label="Optional brand context"]'));
  await click(Array.from(document.querySelectorAll('[role="option"]')).find(value => value.textContent?.includes("Brand B")));
  await click(button("Settings"));
  await choosePage("Publication connection", "Local outbox");
  await click(button("Save publication connection"));
  const oldBindingId = writes.at(-1)?.body.request_id;
  assert(oldBindingId && writes.at(-1)?.body.slot === "publication", "Old package can bind a brand Local destination");
  simulatedInstallation = { ...installation, digest: "new-bundle-digest", catalog_generation: "generation-b" };
  await fresh(); await click(button("Settings"));
  await choosePage("Publication connection", "Local outbox");
  await click(button("Save publication connection"));
  assert(writes.at(-1)?.body.request_id && writes.at(-1)?.body.request_id !== oldBindingId, "Same connection in a new bundle needs a new binding request identity");
  assert(text().includes("binds through"), "Legacy untyped slots name their app-set path instead of offering a board save");
  // An unsaved Brand B choice must not leak into the context-free slot.
  await choosePage("Publication connection", "local · extra");
  await click(host.querySelector('button[aria-label="Optional brand context"]'));
  await click(Array.from(document.querySelectorAll('[role="option"]')).find(value => value.textContent?.includes("No brand context")));
  assert(host.querySelector('button[aria-label="Publication connection"]')?.textContent?.includes("Local outbox"), "Switching brand keeps no unsaved choice");
  await choosePage("Publication connection", "local · extra");
  await click(button("Save publication connection"));
  assert(writes.at(-1)?.body.slot === "publication" && writes.at(-1)?.body.connection_id === "conn-extra" && !Object.hasOwn(writes.at(-1)?.body ?? {}, "context_id"), "Context-free save uses its own draft, never the other brand's");
  await fill('input[placeholder="/absolute/path/to/app or https://github.com/owner/repo"]', "/tmp/social-v04");
  await click(button("Check update")); await click(button("Apply checked update"));
  assert(simulatedInstallation.digest === "bundle-digest" && simulatedInstallation.catalog_generation === "catalog-generation", "Return to the earlier bundle restores its prior content-derived catalog generation");
  await fill('input[placeholder="/absolute/path/to/app or https://github.com/owner/repo"]', "/tmp/social-v05");
  await click(button("Check update"));
  loseUpgradeReply = true;
  await click(button("Apply checked update"));
  const retryUpgradeId = writes.at(-1)?.body.request_id;
  assert(text().includes("Upgrade reply lost") && retryUpgradeId !== firstUpgradeId, "A later A-to-B operation gets a fresh identity even when old digest and catalog hash repeat");
  await click(button("Apply checked update"));
  assert(String(simulatedInstallation.digest) === "new-bundle-digest" && writes.at(-1)?.body.request_id === retryUpgradeId, "An uncertain A-to-B response retains its request identity for an explicit retry");
  simulatedInstallation = null;
  installation.name = "another-app"; await fresh();
  assert(text().includes("no supported board workspace") && !button("New post") && !button("Approve app"), "Unrelated workspace apps get a truthful guide without Social Content actions");
  installation.name = "social-content";
  await React.act(async () => root.unmount());
  console.log("workspace App mounted first-flow checks passed");
}
void main();
