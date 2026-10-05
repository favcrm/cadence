import { appViewExamples } from "../src/features/app-shell/app-views/examples";
import { parseAppBinding } from "../src/features/app-shell/app-views/appBinding";
import { installedViewReceipt, resolveLiveView } from "../src/features/app-shell/app-views/viewReceipt";
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
function refused(check: () => unknown, why: string): void {
  try {
    check();
  } catch {
    return;
  }
  throw new Error(`${why}: expected refusal`);
}

async function main() {
  const fs = require("fs");
  const path = require("path");
  const contractExample = (name: string) => JSON.parse(fs.readFileSync(
    path.join(require("process").cwd(), "..", "contracts", "app-bindings", "v1", "examples", `${name}.json`),
    "utf8",
  ));
  const crmDescriptor = appViewExamples.crm.descriptor;
  const crmBindingRaw = contractExample("crm");
  const crmBinding = parseAppBinding(crmBindingRaw, crmDescriptor);
  equal(crmBinding.contract, "app-bindings/v1", "CRM binding contract parses");
  equal(crmBinding.app, "crm", "CRM binding pairs with its descriptor identity");
  equal(crmBinding.bindings.map((binding) => [binding.view, binding.source, binding.ops]), [
    ["customers", "customers", ["list"]],
    ["customer-detail", "customers", ["show"]],
  ], "CRM example declares the list/detail read pair");
  equal(crmBinding.bindings[0].fields[0], {
    field: "record_id", key: "record_id", format: "text",
  }, "the list binds its source identity for safe detail navigation");

  const socialDescriptor = appViewExamples["social-content"].descriptor;
  const socialBinding = parseAppBinding(contractExample("social-content"), socialDescriptor);
  equal(socialBinding.bindings.map((binding) => [binding.view, binding.source, binding.ops]), [
    ["caption-runs", "caption-runs", ["list"]],
    ["caption-detail", "caption-runs", ["show"]],
  ], "Social example declares a metadata-only list/detail read pair");

  const wrongApp = JSON.parse(JSON.stringify(crmBindingRaw));
  wrongApp.app = "social-content";
  refused(() => parseAppBinding(wrongApp, crmDescriptor), "a binding for another app refuses");
  const forgedEffect = JSON.parse(JSON.stringify(crmBindingRaw));
  forgedEffect.bindings[0].fields[0].actor = "operator";
  refused(() => parseAppBinding(forgedEffect, crmDescriptor), "forbidden actor data refuses recursively");
  const writeOp = JSON.parse(JSON.stringify(crmBindingRaw));
  writeOp.bindings[0].ops = ["delete"];
  refused(() => parseAppBinding(writeOp, crmDescriptor), "a mutation op refuses");
  const badFormat = JSON.parse(JSON.stringify(crmBindingRaw));
  badFormat.bindings[0].fields[0].format = "tags";
  refused(() => parseAppBinding(badFormat, crmDescriptor), "a source/descriptor format mismatch refuses");

  const digest = (char: string) => `sha256:${char.repeat(64)}`;
  const descriptorDigest = digest("b");
  const bindingDigest = digest("c");
  const bundleDigest = digest("a");
  const viewFile = "views/app-views-v1.json";
  const bindingFile = "bindings/app-bindings-v1.json";
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
    files: [viewFile, bindingFile],
    view_descriptor: crmDescriptor,
    view_descriptor_digest: descriptorDigest,
    view_binding: crmBindingRaw,
    view_binding_digest: bindingDigest,
    capabilities: null,
    connection_slots: [],
    ...overrides,
  } as unknown as Installation);

  const receipt = installedViewReceipt(makeInstallation());
  assert(receipt !== null && receipt.error === null, "paired installed receipt parses");
  equal(receipt.bundleDigest, bundleDigest, "receipt keeps the verified bundle pin");
  equal(receipt.descriptorDigest, descriptorDigest, "receipt keeps the descriptor pin");
  equal(receipt.bindingDigest, bindingDigest, "receipt keeps the binding pin");

  const descriptorOnly = installedViewReceipt(makeInstallation({
    files: [viewFile], view_binding: null, view_binding_digest: null,
  }));
  assert(descriptorOnly !== null && descriptorOnly.error === null && descriptorOnly.binding === null,
    "a descriptor-only receipt remains previewable but has no live binding");
  const partial = installedViewReceipt(makeInstallation({ view_binding_digest: null }));
  assert(partial !== null && partial.error !== null, "a partial paired receipt fails closed");
  const wrongIdentity = installedViewReceipt(makeInstallation({ name: "social-content" }));
  assert(wrongIdentity !== null && wrongIdentity.error !== null, "receipt app identity must match the installed app");
  equal(installedViewReceipt(makeInstallation({ files: [], view_descriptor: null, view_descriptor_digest: null,
    view_binding: null, view_binding_digest: null })), null, "packages without view files keep the native route");

  const tableResult = resolveLiveView(receipt, "customers", null);
  assert(tableResult.ok, "the installed customer table resolves");
  equal(tableResult.route.op, "list", "table route derives the list op");
  equal(tableResult.route.identityField, "record_id", "table identity comes from the paired binding");
  equal(tableResult.route.detailViewId, "customer-detail", "one matching detail view is linked");
  const detailResult = resolveLiveView(receipt, "customer-detail", "customer-1");
  assert(detailResult.ok, "a safe customer detail route resolves");
  equal(detailResult.route.op, "show", "detail route derives the show op");
  assert(!resolveLiveView(receipt, "customer-form", null).ok, "forms never resolve to live reads");
  assert(!resolveLiveView(receipt, "undeclared-view", null).ok, "unknown route ids refuse");
  assert(!resolveLiveView(receipt, "customer-detail", "not/a/record").ok, "unsafe detail ids refuse");
  assert(!resolveLiveView(descriptorOnly, "customers", null).ok, "a descriptor without a binding cannot read live data");

  // Mount the generic live component against the same installed receipt.
  // The request mock asserts that the UI sends only receipt pins and the
  // host-owned context; the returned rows then exercise list/detail render.
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/app-installations/install-1" });
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
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const LiveAppView = (require("../src/features/app-shell/app-views/LiveAppView") as
    typeof import("../src/features/app-shell/app-views/LiveAppView")).default;
  const host = document.createElement("div");
  document.body.append(host);
  const requests: URL[] = [];
  let badResponsePin = false;
  globalThis.fetch = (async (input: unknown) => {
    const url = new URL(String(input), "http://localhost");
    requests.push(url);
    const isDetail = url.pathname.includes("customer-detail");
    const cursor = url.searchParams.get("cursor");
    const rows = isDetail
      ? [{ record_id: "customer-1", name: "Ada Live", email: "ada.live@example.test", phone: "+1 555 0100",
        source: "operator", consent_email: "granted", consent_sms: "unknown", tags: ["vip"] }]
      : cursor
        ? [{ record_id: "customer-2", name: "Bea Live", email: "bea.live@example.test", phone: "+1 555 0101",
          source: "operator", consent_email: "unknown", tags: ["member"] }]
        : [{ record_id: "customer-1", name: "Ada Live", email: "ada.live@example.test", phone: "+1 555 0100",
          source: "operator", consent_email: "granted", tags: ["vip"] }];
    const body = {
      rows,
      ...(isDetail ? { record_revision: 2 } : { truncated: !cursor, next_cursor: cursor ? null : "cursor-2" }),
      view_id: isDetail ? "customer-detail" : "customers",
      op: isDetail ? "show" : "list",
      digest: badResponsePin ? digest("d") : bundleDigest,
      view_descriptor_digest: descriptorDigest,
      view_binding_digest: bindingDigest,
    };
    return new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } });
  }) as typeof fetch;
  const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
  const viewProps = (route: typeof tableResult.route, enabled = true) => ({
    installId: "install-1",
    contextId: "context-1",
    receipt,
    route,
    enabled,
    blockedReason: enabled ? null : "Choose an active context.",
    returnHref: "/app-installations/install-1",
    backHref: route.op === "show" ? "/app-installations/install-1?view=customers" : null,
    detailHref: (viewId: string, recordId: string) => `/app-installations/install-1?view=${viewId}&record=${recordId}`,
  });

  const disabledRoot = createRoot(host);
  await React.act(async () => { disabledRoot.render(React.createElement(LiveAppView, viewProps(tableResult.route, false))); });
  await flush();
  assert(host.querySelector('[data-state="live-read-disabled"]'), "the active-context refusal renders accessibly");
  equal(requests.length, 0, "a blocked route never calls the live-read endpoint");
  await React.act(async () => { disabledRoot.unmount(); });

  const listRoot = createRoot(host);
  await React.act(async () => { listRoot.render(React.createElement(LiveAppView, viewProps(tableResult.route))); });
  await flush();
  assert(host.querySelector("table"), "a live table renders as a semantic table");
  assert(host.textContent?.includes("Ada Live"), "verified customer fields render as text");
  assert(host.querySelector('[data-state="live-read-only"]'), "live views label the surface read-only");
  const openLink = Array.from(host.querySelectorAll("a")).find((link) => link.textContent === "Open");
  equal(openLink?.getAttribute("href"), "/app-installations/install-1?view=customer-detail&record=customer-1",
    "row navigation carries only the bound source identity");
  equal(requests[0].pathname, "/api/app-installations/install-1/views/customers/rows", "table calls the bound list route");
  equal(requests[0].searchParams.get("context_id"), "context-1", "the host context scopes the request");
  equal(requests[0].searchParams.get("digest"), bundleDigest, "the bundle pin accompanies the read");
  equal(requests[0].searchParams.get("descriptor"), descriptorDigest, "the descriptor pin accompanies the read");
  equal(requests[0].searchParams.get("binding"), bindingDigest, "the binding pin accompanies the read");
  equal(requests[0].searchParams.get("limit"), "50", "customer list reads are page bounded");
  const loadMore = Array.from(host.querySelectorAll("button")).find((button) => button.textContent === "Load more");
  assert(loadMore, "a truncated page offers an explicit next-page control");
  await React.act(async () => { loadMore.click(); await new Promise((resolve) => setTimeout(resolve, 0)); });
  await flush();
  assert(host.textContent?.includes("Bea Live"), "the next bounded page appends without losing rows");
  equal(requests[1].searchParams.get("cursor"), "cursor-2", "pagination carries only the returned cursor");
  await React.act(async () => { listRoot.unmount(); });

  const detailRoot = createRoot(host);
  await React.act(async () => { detailRoot.render(React.createElement(LiveAppView, viewProps(detailResult.route))); });
  await flush();
  assert(host.querySelector("dl"), "a live detail renders as a labelled description list");
  assert(host.textContent?.includes("ada.live@example.test"), "the detail shows its bound customer values");
  assert(host.querySelector('a[href="/app-installations/install-1?view=customers"]'), "detail keeps a table back-link");
  equal(requests[2].pathname,
    "/api/app-installations/install-1/views/customer-detail/rows/customer-1", "detail calls the bound show route");
  equal(requests[2].searchParams.get("context_id"), "context-1", "detail reads keep the host context");
  assert(!requests[2].searchParams.has("limit"), "detail reads do not carry list selectors");
  await React.act(async () => { detailRoot.unmount(); });

  badResponsePin = true;
  const refusedRoot = createRoot(host);
  await React.act(async () => { refusedRoot.render(React.createElement(LiveAppView, viewProps(tableResult.route))); });
  await flush();
  assert(host.querySelector('[role="alert"]')?.textContent?.includes("does not match the installed view pins"),
    "a response with different receipt pins is refused before rendering rows");
  assert(!host.querySelector("table"), "a mismatched response never renders its data");
  await React.act(async () => { refusedRoot.unmount(); });

  win.happyDOM.abort();
  console.log("app binding and live view checks passed");
}
void main().catch((error) => { console.error(error); require("process").exitCode = 1; });
