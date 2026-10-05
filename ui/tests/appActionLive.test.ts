import { appViewExamples } from "../src/features/app-shell/app-views/examples";
import { installedViewReceipt, resolveLiveAction, resolveLiveView, type ResolvedLiveAction } from "../src/features/app-shell/app-views/viewReceipt";
import type { Installation } from "../src/features/workspace-apps/workspaceApps";

declare function require(name: string): any;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}

async function main() {
  const fs = require("fs");
  const path = require("path");
  const process = require("process");
  const example = (name: string, contract: string, version: string) => JSON.parse(fs.readFileSync(
    path.join(process.cwd(), "..", "contracts", contract, version, "examples", `${name}.json`),
    "utf8",
  ));
  const bindingRaw = example("crm", "app-bindings", "v1");
  const actionRaw = example("crm", "app-actions", "v2");
  const bundleDigest = `sha256:${"a".repeat(64)}`;
  const descriptorDigest = `sha256:${"b".repeat(64)}`;
  const bindingDigest = `sha256:${"c".repeat(64)}`;
  const makeInstallation = (overrides: Record<string, unknown> = {}): Installation => ({
    install_id: "install-1",
    name: "crm",
    title: "CRM",
    version: "1.0.0",
    summary: "",
    digest: bundleDigest,
    catalog_generation: "fixture-generation",
    storage_kind: "workspace",
    project_link: null,
    approved: true,
    executable: true,
    approval: { state: "approved" },
    guide: "",
    files: [
      "views/app-views-v1.json",
      "bindings/app-bindings-v1.json",
      "actions/app-actions-v2.json",
    ],
    view_descriptor: appViewExamples.crm.descriptor,
    view_descriptor_digest: descriptorDigest,
    view_binding: bindingRaw,
    view_binding_digest: bindingDigest,
    action_descriptor: actionRaw,
    capabilities: null,
    connection_slots: [],
    ...overrides,
  } as unknown as Installation);

  const receipt = installedViewReceipt(makeInstallation());
  assert(receipt !== null && receipt.error === null && receipt.actionDescriptor !== null,
    "the explicit action companion parses only with its paired installed view receipt");
  const create = resolveLiveAction(receipt, "customer.create", null);
  assert(create.ok && create.route.operation === "create", "customer.create resolves to a host create route");
  equal(create.route.action.form_view, "customer-create-form", "create selects its paired inert preview");
  const update = resolveLiveAction(receipt, "customer.update", "cust-0123456789abcdef0123456789abcdef");
  assert(update.ok && update.route.operation === "update" && update.route.detailRoute !== null,
    "customer.update resolves through the unique bound customer detail");
  assert(!resolveLiveAction(receipt, "customer.create", "client-id").ok, "create cannot carry a record id");
  assert(!resolveLiveAction(receipt, "customer.update", null).ok, "update requires a safe record id");
  assert(!resolveLiveAction(receipt, "customer.delete", "cust-0123456789abcdef0123456789abcdef").ok,
    "unknown action ids refuse");
  assert(!resolveLiveAction(receipt, "customer.create", null, true).ok,
    "an action selector cannot override a simultaneous live view selector");
  assert(!resolveLiveView(receipt, "customer-create-form", null).ok,
    "v1 form previews remain inert after the v2 companion is installed");
  const missingActionFile = installedViewReceipt(makeInstallation({ files: [
    "views/app-views-v1.json", "bindings/app-bindings-v1.json",
  ] }));
  assert(missingActionFile?.error !== null, "action data without its exact bundle file receipt fails closed");
  const wrongActionApp = installedViewReceipt(makeInstallation({
    action_descriptor: { ...actionRaw, app: "social-content" },
  }));
  assert(wrongActionApp?.error !== null, "action app identity must match the paired view and binding");
  const customFormDescriptor = JSON.parse(JSON.stringify(appViewExamples.crm.descriptor)) as {
    views: { id: string }[];
  };
  customFormDescriptor.views[2].id = "signup_form";
  customFormDescriptor.views[3].id = "profile_editor";
  const customFormActions = structuredClone(actionRaw) as {
    actions: { form_view: string }[];
  };
  customFormActions.actions[0].form_view = "signup_form";
  customFormActions.actions[1].form_view = "profile_editor";
  const descriptorDeclaredForms = installedViewReceipt(makeInstallation({
    view_descriptor: customFormDescriptor,
    action_descriptor: customFormActions,
  }));
  assert(descriptorDeclaredForms?.error === null,
    "action form references resolve by descriptor ID rather than a host form-name allowlist");

  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/app-installations/install-1?ctx=context-1&action=customer.create" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement",
    "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent",
    "KeyboardEvent", "location", "history", "sessionStorage"]) {
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name], configurable: true, writable: true,
    });
  }
  for (const name of ["addEventListener", "removeEventListener"]) {
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  }
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true, configurable: true });
  const React = require("react") as typeof import("react");
  const { createRoot: createDomRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const LiveAppAction = (require("../src/features/app-shell/app-views/LiveAppAction") as
    typeof import("../src/features/app-shell/app-views/LiveAppAction")).default;
  const host = document.createElement("div");
  document.body.append(host);
  const requests: { url: URL; method: string; body: Record<string, unknown> | null }[] = [];
  const recordId = "cust-0123456789abcdef0123456789abcdef";
  const recordDigest = `sha256:${"d".repeat(64)}`;
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = new URL(String(input), "http://localhost");
    const method = init?.method ?? "GET";
    const requestBody = typeof init?.body === "string" ? JSON.parse(init.body) as Record<string, unknown> : null;
    requests.push({ url, method, body: requestBody });
    if (method === "GET") {
      return new Response(JSON.stringify({
        rows: [{ record_id: recordId, name: "Existing Ada", email: "ada@example.test", phone: "+1 555 0100",
          source: "referral", consent_email: "granted", consent_sms: "unknown", tags: ["vip"] }],
        view_id: "customer-detail",
        op: "show",
        digest: bundleDigest,
        view_descriptor_digest: descriptorDigest,
        view_binding_digest: bindingDigest,
        record_revision: 3,
      }), { status: 200, headers: { "Content-Type": "application/json" } });
    }
    const isUpdate = url.pathname.includes("customer.update");
    const profile = isUpdate
      ? { schema: 1, display_name: "Existing Ada", email: "ada@example.test", phone: "+1 555 0100", source: "referral", tags: ["vip"], consent: { email: "granted" } }
      : { schema: 1, display_name: "Grace Hopper", email: null, tags: [], consent: { email: "unknown" } };
    return new Response(JSON.stringify({
      record: {
        id: isUpdate ? recordId : `cust-${"e".repeat(32)}`,
        install_id: "install-1",
        context_id: "context-1",
        kind: "customer",
        revision: isUpdate ? 4 : 1,
        digest: recordDigest,
        profile,
        history: [],
        consent_history: [],
      },
      digest: bundleDigest,
      view_descriptor_digest: descriptorDigest,
      view_binding_digest: bindingDigest,
    }), { status: 200, headers: { "Content-Type": "application/json" } });
  }) as typeof fetch;
  const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
  const props = (route: ResolvedLiveAction) => ({
    installId: "install-1",
    contextId: "context-1",
    receipt,
    route,
    enabled: true,
    blockedReason: null,
    returnHref: "/app-installations/install-1?ctx=context-1&view=customers",
    detailHref: (viewId: string, id: string) => `/app-installations/install-1?ctx=context-1&view=${viewId}&record=${id}`,
  });
  const createRoot = createDomRoot(host);
  await React.act(async () => { createRoot.render(React.createElement(LiveAppAction, props(create.route))); });
  await flush();
  assert(host.querySelector('form[aria-label="Create customer"]'), "the host creates a separate live form");
  assert(host.textContent?.includes("v1 form preview remains inert"), "the action UI keeps the inert-preview boundary visible");
  equal(requests.length, 0, "create does not read or submit until the operator asks");
  const nameInput = host.querySelector('input[id$="display_name"]') as HTMLInputElement;
  assert(nameInput, "the action renders a labelled display name field");
  await React.act(async () => {
    Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")!.set!.call(nameInput, "Grace Hopper");
    nameInput.dispatchEvent(new win.Event("input", { bubbles: true }));
  });
  const createForm = host.querySelector("form")!;
  await React.act(async () => { createForm.dispatchEvent(new win.Event("submit", { bubbles: true, cancelable: true })); await flush(); });
  const createRequest = requests[0];
  equal(createRequest.method, "POST", "create uses a write request");
  equal(createRequest.url.pathname,
    "/api/app-installations/install-1/contexts/context-1/views/customer-create-form/actions/customer.create",
    "create URL carries the verified installation/context/form/action route");
  assert(createRequest.body !== null, "create has a JSON body");
  equal(Object.keys(createRequest.body!).sort(), ["binding", "descriptor", "digest", "input"],
    "create body contains only the three pins and typed input");
  equal(createRequest.body!.input, {
    display_name: "Grace Hopper", email: null, phone: null, source: null, tags: [],
  }, "create keeps the host-owned mutable profile fields explicit and consent-free");
  assert(win.location.search.includes(`view=customer-detail&record=${"cust-"}${"e".repeat(32)}`),
    "only a verified host-created receipt navigates to the customer detail");
  await React.act(async () => { createRoot.unmount(); });

  win.history.replaceState(null, "", `/app-installations/install-1?ctx=context-1&action=customer.update&record=${recordId}`);
  const updateRoute = resolveLiveAction(receipt, "customer.update", recordId);
  assert(updateRoute.ok, "the customer update route resolves for its detail read");
  const updateRoot = createDomRoot(host);
  await React.act(async () => { updateRoot.render(React.createElement(LiveAppAction, props(updateRoute.route))); });
  await flush();
  const updateForm = host.querySelector("form")!;
  assert(updateForm, "the edit form waits for the verified current detail");
  equal((host.querySelector('input[id$="display_name"]') as HTMLInputElement).value, "Existing Ada",
    "edit prefills the host-bound customer display name");
  equal((host.querySelector('textarea') as HTMLTextAreaElement).value, "vip",
    "edit prefills current tags rather than silently clearing them");
  equal(requests[1].method, "GET", "edit reads the current customer before enabling save");
  equal(requests[1].url.searchParams.get("context_id"), "context-1", "edit read is explicitly context-bound");
  equal(requests[1].url.searchParams.get("digest"), bundleDigest, "edit read carries the bundle pin");
  const editInput = host.querySelector('input[id$="display_name"]') as HTMLInputElement;
  await React.act(async () => {
    Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")!.set!.call(editInput, "Ada Byron");
    editInput.dispatchEvent(new win.Event("input", { bubbles: true }));
  });
  await React.act(async () => { updateForm.dispatchEvent(new win.Event("submit", { bubbles: true, cancelable: true })); await flush(); });
  const updateRequest = requests[2];
  equal(updateRequest.method, "POST", "edit uses a write request");
  equal(updateRequest.url.pathname,
    `/api/app-installations/install-1/contexts/context-1/views/customer-edit-form/actions/customer.update/records/${recordId}`,
    "update URL carries the record identity only in the route");
  equal(Object.keys(updateRequest.body!).sort(), ["binding", "descriptor", "digest", "expected_revision", "input"],
    "update adds only the host-owned expected revision to the create body shape");
  equal(updateRequest.body!.expected_revision, 3, "update CAS uses the revision from the bound current detail read");
  equal((updateRequest.body!.input as Record<string, unknown>).display_name, "Ada Byron", "edit submits the changed value");
  equal((updateRequest.body!.input as Record<string, unknown>).tags, ["vip"], "edit retains the current tags");
  assert(win.location.search.includes(`view=customer-detail&record=${recordId}`),
    "a validated update receipt returns to the scoped customer detail");
  await React.act(async () => { updateRoot.unmount(); });
  win.happyDOM.abort();
  console.log("app-actions/v2 receipt, routing and host form checks passed");
}
void main().catch((error) => { console.error(error); require("process").exitCode = 1; });
