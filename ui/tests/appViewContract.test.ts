import {
  AppViewContractError,
  FORBIDDEN_DESCRIPTOR_KEYS,
  describeAppView,
  fixtureRows,
  parseAppView,
  cellText,
} from "../src/features/app-shell/app-views/contract";
import { appViewExamples } from "../src/features/app-shell/app-views/examples";

export {};
/** CAD-861: strict app-views/v1 validation + text-only rendering. */
declare function require(name: string): any;

function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
function refused(check: (d: unknown) => void, value: unknown, why: string): void {
  try {
    check(value);
  } catch (e) {
    assert(e instanceof AppViewContractError, `${why}: wrong error ${String(e)}`);
    return;
  }
  throw new Error(`${why}: expected a refusal`);
}

async function main() {
// --- The two worked examples parse clean through the shared gate. ---
equal(appViewExamples.crm.descriptor.contract, "app-views/v1", "crm example declares the v1 contract");
equal(appViewExamples.crm.descriptor.app, "crm", "crm example names its app kind");
equal(appViewExamples.crm.descriptor.views.length, 3, "crm example declares three views");
equal(appViewExamples["social-content"].descriptor.app, "social-content", "social example names its app kind");
equal(appViewExamples["social-content"].descriptor.views.length, 3, "social example declares three views");

// The JSON files under contracts/ are the same descriptors the TS
// fixtures embed — parse them through the identical gate so the two
// copies cannot drift.
const fs = require("fs");
const path = require("path");
for (const name of ["crm", "social-content"]) {
  const raw = JSON.parse(
    fs.readFileSync(
      path.join(require("process").cwd(), "..", "contracts", "app-views", "v1", "examples", `${name}.json`),
      "utf8",
    ),
  );
  const parsed = parseAppView(raw);
  equal(parsed.app, name === "crm" ? "crm" : "social-content", `${name}.json parses and names its app`);
  equal(parsed.views.length, 3, `${name}.json declares three views`);
  equal(parsed, appViewExamples[name as keyof typeof appViewExamples].descriptor,
    `${name}.json matches the renderer descriptor, including fields and formats`);
}

// --- Malformed descriptors refuse. ---
refused((v) => parseAppView(v), null, "null is not a descriptor");
refused((v) => parseAppView(v), {}, "empty object lacks the contract tag");
refused((v) => parseAppView(v), { contract: "app-views/v2" }, "a wrong contract tag refuses");
refused((v) => parseAppView(v), { contract: "app-views/v1", app: "CRM", title: "t", views: [{ id: "v", title: "t", kind: "table", fields: [{ id: "f", label: "l" }], columns: [{ field: "f" }] }] }, "uppercase app id refuses");
refused((v) => parseAppView(v), { contract: "app-views/v1", app: "crm", title: "t", views: [] }, "zero views refuse");
refused((v) => parseAppView(v), { contract: "app-views/v1", app: "crm", title: "t", views: [{ id: "v", title: "t", kind: "table", fields: [{ id: "f", label: "l" }], columns: [{ field: "ghost" }] }] }, "column naming an undeclared field refuses");
refused((v) => parseAppView(v), { contract: "app-views/v1", app: "crm", title: "t", views: [{ id: "v", title: "t", kind: "table", fields: [{ id: "f", label: "l" }], columns: [{ field: "f" }, { field: "f" }] }] }, "duplicate column field refuses");
refused((v) => parseAppView(v), { contract: "app-views/v1", app: "crm", title: "t", views: [{ id: "v", title: "t", kind: "table", fields: [{ id: "f", label: "l" }, { id: "f", label: "l2" }], columns: [{ field: "f" }] }] }, "duplicate field id refuses");
refused((v) => parseAppView(v), { contract: "app-views/v1", app: "crm", title: "t", views: [{ id: "v", title: "t", kind: "detail", fields: [{ id: "f", label: "l" }], columns: [{ field: "f" }] }] }, "a detail view carrying columns refuses");
refused((v) => parseAppView(v), { contract: "app-views/v1", app: "crm", title: "t", views: [{ id: "v", title: "t", kind: "form", fields: [{ id: "f", label: "l" }] }] }, "a form view carrying fields refuses");
refused((v) => parseAppView(v), { contract: "app-views/v1", app: "crm", title: "t", views: [{ id: "v", title: "t", kind: "table", fields: [{ id: "f", label: "l", format: "enum" }], columns: [{ field: "f" }] }] }, "enum without values refuses");
refused((v) => parseAppView(v), { contract: "app-views/v1", app: "crm", title: "t", views: [{ id: "v", title: "t", kind: "table", fields: [{ id: "f", label: "l", format: "text", values: ["x"] }], columns: [{ field: "f" }] }] }, "values on a non-enum format refuses");
refused((v) => parseAppView(v), { contract: "app-views/v1", app: "crm", title: "t", views: [{ id: "v", title: "t", kind: "table", fields: [{ id: "f", label: "l", format: "tags" }], columns: [{ field: "f" }] }] }, "tags without kind list refuses");
refused((v) => parseAppView(v), { contract: "app-views/v1", app: "crm", title: "t", views: [{ id: "v", title: "t", kind: "table", fields: [{ id: "f", label: "l", createView: "no-such-view" }], columns: [{ field: "f" }] }] }, "createView naming no declared form view refuses");
// Two views with the same id refuse.
refused(
  (v) => parseAppView(v),
  { contract: "app-views/v1", app: "crm", title: "t", views: [
    { id: "dup", title: "a", kind: "detail", fields: [{ id: "f", label: "l" }] },
    { id: "dup", title: "b", kind: "detail", fields: [{ id: "f", label: "l" }] },
  ] },
  "duplicate view id refuses",
);
// An unknown top-level key refuses.
refused((v) => parseAppView(v), { contract: "app-views/v1", app: "crm", title: "t", views: [{ id: "v", title: "t", kind: "detail", fields: [{ id: "f", label: "l" }] }], extra: 1 }, "unknown descriptor key refuses");
// An unknown view key refuses.
refused((v) => parseAppView(v), { contract: "app-views/v1", app: "crm", title: "t", views: [{ id: "v", title: "t", kind: "detail", fields: [{ id: "f", label: "l" }], route: "/x" }] }, "unknown view key refuses");
// An unknown field key refuses.
refused((v) => parseAppView(v), { contract: "app-views/v1", app: "crm", title: "t", views: [{ id: "v", title: "t", kind: "detail", fields: [{ id: "f", label: "l", hint: "x" }] }] }, "unknown field key refuses");

// --- Forbidden keys refuse recursively, at every level. ---
const dangerous = (key: string, value: unknown) => ({
  contract: "app-views/v1", app: "crm", title: "t",
  views: [{ id: "v", title: "t", kind: "detail", fields: [{ id: "f", label: "l" }], [key]: value }],
});
for (const key of FORBIDDEN_DESCRIPTOR_KEYS) {
  refused((v) => parseAppView(v), dangerous(key, "x"), `forbidden view key refused: ${key}`);
}
// Nested inside a field the forbidden key still refuses.
refused((v) => parseAppView(v), { contract: "app-views/v1", app: "crm", title: "t", views: [{ id: "v", title: "t", kind: "detail", fields: [{ id: "f", label: "l", url: "https://evil.test" }] }] }, "url inside a field refuses");
// Scope/actor/credential escapes refuse at the descriptor root.
refused((v) => parseAppView(v), { contract: "app-views/v1", app: "crm", title: "t", install_id: "forged", views: [{ id: "v", title: "t", kind: "detail", fields: [{ id: "f", label: "l" }] }] }, "forged install_id refuses");
refused((v) => parseAppView(v), { contract: "app-views/v1", app: "crm", title: "t", context_id: "ctx", views: [{ id: "v", title: "t", kind: "detail", fields: [{ id: "f", label: "l" }] }] }, "forged context_id refuses");
refused((v) => parseAppView(v), { contract: "app-views/v1", app: "crm", title: "t", actor: "root", views: [{ id: "v", title: "t", kind: "detail", fields: [{ id: "f", label: "l" }] }] }, "forged actor refuses");
refused((v) => parseAppView(v), { contract: "app-views/v1", app: "crm", title: "t", effect: "send", views: [{ id: "v", title: "t", kind: "detail", fields: [{ id: "f", label: "l" }] }] }, "forged effect refuses");
refused((v) => parseAppView(v), { contract: "app-views/v1", app: "crm", title: "t", script: "alert(1)", views: [{ id: "v", title: "t", kind: "detail", fields: [{ id: "f", label: "l" }] }] }, "script key refuses");
refused((v) => parseAppView(v), { contract: "app-views/v1", app: "crm", title: "t", href: "javascript:alert(1)", views: [{ id: "v", title: "t", kind: "detail", fields: [{ id: "f", label: "l" }] }] }, "href key refuses");
// Prototype pollution keys refuse.
refused((v) => parseAppView(v), { contract: "app-views/v1", app: "crm", title: "t", views: [{ id: "v", title: "t", kind: "detail", fields: [{ id: "f", label: "l" }] }], __proto__: { polluted: true } }, "__proto__ refuses");
refused((v) => parseAppView(v), JSON.parse('{"contract":"app-views/v1","app":"crm","title":"t","views":[{"id":"v","title":"t","kind":"detail","fields":[{"id":"f","label":"l","__proto__":{"x":1}}]}]}'), "__proto__ inside a field refuses");

// --- Bounds refuse. ---
const bigLabel = { contract: "app-views/v1", app: "crm", title: "t", views: [{ id: "v", title: "t", kind: "detail", fields: [{ id: "f", label: "x".repeat(81) }] }] };
refused((v) => parseAppView(v), bigLabel, "oversized label refuses");
const manyViews = { contract: "app-views/v1", app: "crm", title: "t", views: Array.from({ length: 17 }, (_, i) => ({ id: `v${i}`, title: "t", kind: "detail", fields: [{ id: "f", label: "l" }] })) };
refused((v) => parseAppView(v), manyViews, "more than 16 views refuse");
const deep: { v: unknown } = { v: { contract: "app-views/v1", app: "crm", title: "t", views: [{ id: "v", title: "t", kind: "detail", fields: [{ id: "f", label: "l" }] }] } };
for (let i = 0; i < 30; i++) deep.v = { nested: deep.v };
refused((v) => parseAppView(v), deep.v, "deeply nested descriptor refuses");
const huge = { contract: "app-views/v1", app: "crm", title: "x".repeat(65 * 1024), views: [{ id: "v", title: "t", kind: "detail", fields: [{ id: "f", label: "l" }] }] };
refused((v) => parseAppView(v), huge, "oversized serialized descriptor refuses");
const control = { contract: "app-views/v1", app: "crm", title: "has\nnewline", views: [{ id: "v", title: "t", kind: "detail", fields: [{ id: "f", label: "l" }] }] };
refused((v) => parseAppView(v), control, "control characters refuse");

// Non-JSON/executable inputs and cycles refuse before serialization.
const cyclic: Record<string, unknown> = {}; cyclic.self = cyclic;
refused((v) => parseAppView(v), cyclic, "cyclic input refuses with a bounded contract error");
let executed = false;
refused((v) => parseAppView(v), { toJSON() { executed = true; return {}; } }, "toJSON is not executed");
assert(!executed, "serialization never executes supplied functions");
refused((v) => parseAppView(v), { value: () => 1 }, "function values refuse");
refused((v) => parseAppView(v), { value: Infinity }, "non-finite JSON numbers refuse");

// Formatted lists are intentionally unsupported in v1, not a path around
// scalar format checks. Text lists and tags remain the bounded vocabulary.
for (const format of ["number", "date", "datetime", "enum"]) {
  refused((v) => parseAppView(v), {
    contract: "app-views/v1", app: "demo", title: "Demo", views: [{
      id: "v", title: "Demo", kind: "detail",
      fields: [{ id: "f", label: "Field", format, kind: "list", ...(format === "enum" ? { values: ["x"] } : {}) }],
    }],
  }, `formatted ${format} lists refuse`);
}
function formattedView(format: string) {
  return parseAppView({ contract: "app-views/v1", app: "demo", title: "Demo", views: [{
    id: "v", title: "Demo", kind: "detail", fields: [{ id: "f", label: "Field", format }],
  }] }).views[0];
}
for (const value of ["2026-02-31", "2026-02-29", "1900-02-29", "2026-04-31", "2026-00-10", "2026-13-01", "2026-01-00"]) {
  refused((v) => fixtureRows(formattedView("date"), v), [{ f: value }], `invalid calendar date ${value} refuses`);
}
for (const value of ["2026-13-99T25:61:00Z", "2026-02-31T12:00Z", "2026-01-01T24:00Z", "2026-01-01T12:60Z", "2026-01-01T12:00:60Z", "2026-01-01T12:00+24:00", "2026-01-01T12:00+08:60", "2026-01-01T12:00.5Z"]) {
  refused((v) => fixtureRows(formattedView("datetime"), v), [{ f: value }], `invalid datetime ${value} refuses`);
}
for (const value of ["2024-02-29", "2000-02-29", "2026-02-28"]) {
  equal(fixtureRows(formattedView("date"), [{ f: value }])[0].f, value, `valid calendar date ${value} retained`);
}
for (const value of ["2024-02-29T23:59:59.123Z", "2026-01-01T00:00+08:00", "2026-01-01T12:00:01-05:30"]) {
  equal(fixtureRows(formattedView("datetime"), [{ f: value }])[0].f, value, `valid datetime ${value} retained`);
}

// --- Fixture rows are strict: unknown keys, bad enums, bad shapes. ---
const crmTable = appViewExamples.crm.descriptor.views.find((v) => v.id === "customers")!;
refused((v) => fixtureRows(crmTable, v), [{ name: "Ada", ghost_field: "x" }], "row naming undeclared field refuses");
refused((v) => fixtureRows(crmTable, v), [{ name: "Ada", tier: "superuser" }], "enum outside declared values refuses");
refused((v) => fixtureRows(crmTable, v), [{ name: "Ada", visits: "not-a-number" }], "non-numeric number cell refuses");
refused((v) => fixtureRows(crmTable, v), [{ name: "x".repeat(513) }], "oversized cell refuses");
refused((v) => fixtureRows(crmTable, v), [{ name: "a\nb" }], "control chars in a cell refuse");
refused((v) => fixtureRows(crmTable, v), [{ name: "Ada", tags: "not-a-list" }], "scalar where the field is a list refuses");
// Good rows parse; missing cells are allowed and render as "—".
const okRows = fixtureRows(crmTable, [{ name: "Ada", tier: "vip" }]);
equal(okRows.length, 1, "one row parses");
equal(okRows[0].name, "Ada", "declared cell kept");
equal(okRows[0].email, undefined, "missing cell stays missing");
equal(cellText(okRows[0].email), "—", "absent cell renders as em-dash");
equal(cellText(["a", "b"]), "a, b", "list cell joins for display");

// A form view declares no fields — fixture rows for it refuse.
const crmForm = appViewExamples.crm.descriptor.views.find((v) => v.id === "customer-form")!;
refused((v) => fixtureRows(crmForm, v), [{}], "a form view has no row-bearing fields");

// --- Inert script-looking text is allowed as data but never runs. ---
const inert = fixtureRows(crmTable, [{ name: "<script>alert(1)</script>" }]);
equal(inert[0].name, "<script>alert(1)</script>", "script-looking text stays a plain string cell");

// describeAppView reports errors without throwing.
const bad = describeAppView({ contract: "nope" });
equal(bad.ok, false, "bad descriptor reports not-ok");
assert(!bad.ok && bad.error.includes("contract"), "the error names the failing key");
const good = describeAppView(appViewExamples.crm.descriptor);
assert(good.ok, "the bundled example re-parses through the reporting gate");

// --- Mounted render: both fixtures produce text, never markup. ---
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/app-installations/install-preview?contract-preview=crm" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true, configurable: true });
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id.endsWith("/env") || id === "../../env") return { isDev: true };
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const AppView = (require("../src/features/app-shell/app-views/AppView") as typeof import("../src/features/app-shell/app-views/AppView")).default;
const { renderToStaticMarkup } = require("react-dom/server") as typeof import("react-dom/server");

// Static markup: a script-looking fixture cell renders as text, and no
// descriptor string can become an element, attribute or URL.
const staticMarkup = renderToStaticMarkup(
  React.createElement(AppView, {
    descriptor: appViewExamples.crm.descriptor,
    rows: appViewExamples.crm.rows,
    initialViewId: "customers",
  }),
);
assert(!/<script/i.test(staticMarkup), "no script element is produced");
assert(staticMarkup.includes("&lt;script&gt;"), "the script-looking name renders escaped as text");
assert(!/href="javascript:/i.test(staticMarkup), "no javascript: URL is produced");
assert(!/onerror=/i.test(staticMarkup), "no event handler attribute is produced");

// Mounted render in happy-dom: table, detail and form-preview all
// render accessible structure, and the descriptor's own strings stay
// text nodes.
const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
await React.act(async () => {
  root.render(
    React.createElement(AppView, {
      descriptor: appViewExamples.crm.descriptor,
      rows: appViewExamples.crm.rows,
      initialViewId: "customers",
    }),
  );
});
await new Promise((r) => setTimeout(r, 0));
const text = () => host.textContent ?? "";
assert(text().includes("Customers"), "the CRM table title renders");
assert(text().includes("Synthetic Ada Demo"), "a synthetic fixture row renders as text");
assert(text().includes("<script>alert(1)</script>"), "script-looking fixture text is visible text");
assert(!host.querySelector("script"), "no script element mounted");
assert(host.querySelector("table"), "the table view renders a real table");
assert(host.querySelectorAll("th").length === 5, "five declared columns render as headers");
assert(host.querySelector('[role="region"]'), "the scrollable table region is labelled");

// Duplicate tag values are valid list data; the renderer must keep unique
// reconciliation keys and preserve every chip when the list changes.
const keyErrors: string[] = [];
const originalConsoleError = console.error;
console.error = (...args: unknown[]) => {
  const message = args.map(String).join(" ");
  if (/same key|unique.*key/i.test(message)) keyErrors.push(message);
  else originalConsoleError(...args);
};
try {
  for (const tags of [["vip", "vip"], ["vip", "member", "vip"]]) {
    const rows = { customers: fixtureRows(crmTable, [{ ...appViewExamples.crm.rows.customers[0], tags }]) };
    await React.act(async () => { root.render(React.createElement(AppView, {
      descriptor: appViewExamples.crm.descriptor, rows, initialViewId: "customers",
    })); });
    equal(Array.from(host.querySelectorAll(".av-tags .chip")).map((el) => el.textContent), tags,
      "duplicate chips survive list reconciliation without extra or missing nodes");
  }
  equal(keyErrors, [], "duplicate tag values produce no React key warnings");
} finally {
  console.error = originalConsoleError;
}

// Detail view renders labelled fields.
await React.act(async () => {
  root.render(
    React.createElement(AppView, {
      descriptor: appViewExamples.crm.descriptor,
      rows: appViewExamples.crm.rows,
      initialViewId: "customer-detail",
    }),
  );
});
await new Promise((r) => setTimeout(r, 0));
assert(host.querySelector("dl"), "the detail view renders a description list");
assert(text().includes("Last visit"), "a declared field label renders");
assert(text().includes("2026-10-02"), "a date cell renders as text");

// Form preview renders disabled labelled controls — never a live form.
await React.act(async () => {
  root.render(
    React.createElement(AppView, {
      descriptor: appViewExamples.crm.descriptor,
      rows: appViewExamples.crm.rows,
      initialViewId: "customer-form",
    }),
  );
});
await new Promise((r) => setTimeout(r, 0));
const form = host.querySelector("form");
assert(form, "the form view renders a form element");
assert(host.querySelectorAll("input[disabled], select[disabled], textarea[disabled]").length >= 4,
  "preview controls are disabled");
const submit = host.querySelector('button[type="submit"]') as HTMLButtonElement | null;
assert(submit?.disabled, "the preview submit is disabled");
assert(host.querySelector('label[for]'), "form fields keep label/id association");

// The Social fixture renders through the same component — one
// renderer, two packages, no kind-specific code.
await React.act(async () => {
  root.render(
    React.createElement(AppView, {
      descriptor: appViewExamples["social-content"].descriptor,
      rows: appViewExamples["social-content"].rows,
      initialViewId: "caption-runs",
    }),
  );
});
await new Promise((r) => setTimeout(r, 0));
assert(text().includes("Caption runs"), "the social table title renders");
assert(text().includes("Summer ramen launch"), "social fixture rows render");
assert(text().includes("夏日限定"), "zh-HK fixture text renders intact");

await React.act(async () => { root.unmount(); });

// Real shell lifecycle with fixture HTTP receipts: direct preview links and
// same-installation navigation remain, App-menu installation switches exit.
let streams = 0;
Object.defineProperty(globalThis, "EventSource", { configurable: true, value: class {
  onopen = null; onerror = null;
  constructor() { streams += 1; }
  addEventListener() {} removeEventListener() {} close() {}
} });
const shellBase = {
  title: "Reports", name: "reports", version: "0.1.0", digest: "fixture-digest",
  catalog_generation: "fixture-generation", approved: true, storage_kind: "workspace",
  files: [], capabilities: null, connection_slots: [],
};
globalThis.fetch = (async (input: unknown) => {
  const url = String(input);
  if (url.endsWith("/conversations")) return new Response("{}", { status: 404 });
  const receipt = url.endsWith("/contexts") ? { contexts: [] }
    : url.startsWith("/api/threads/master") ? { entries: [], more_before: false }
    : url === "/api/app-installations/install-a" ? { ...shellBase, install_id: "install-a" }
    : url === "/api/app-installations/install-b" ? { ...shellBase, install_id: "install-b", name: "social-content", title: "Social Content" }
    : null;
  assert(receipt !== null, `unexpected fixture read ${url}`);
  return new Response(JSON.stringify(receipt), { status: 200 });
}) as typeof fetch;
const AppShellComponent = (require("../src/features/app-shell/AppShell") as typeof import("../src/features/app-shell/AppShell")).default;
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const AppShell = (props: React.ComponentProps<typeof AppShellComponent>) =>
  React.createElement(LocaleProvider, null, React.createElement(AppShellComponent, props));
const { navigate } = require("../src/lib/useLocation") as typeof import("../src/lib/useLocation");
const { contextNavigationSearch } = require("../src/features/projects/contextRoute") as typeof import("../src/features/projects/contextRoute");
history.replaceState(null, "", "/app-installations/install-a?contract-preview=crm");
const shellRoot = createRoot(host);
const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
await React.act(async () => { shellRoot.render(React.createElement(AppShell, {
  installId: "install-a", viewer: { operator: true, readOnly: false },
})); });
await flush(); await flush(); await flush();
assert(host.querySelector('[data-contract-preview="crm"]'), "first-mount direct preview link remains");
await React.act(async () => { navigate("/app-installations/install-a?contract-preview=crm&contract-preview-view=customer-form"); });
await flush();
assert(host.querySelector('[data-app-view="customer-form"]'), "same-installation preview navigation remains");
const { matchRoute } = require("../src/lib/router") as typeof import("../src/lib/router");
const menuQuery = contextNavigationSearch(
  matchRoute(location.pathname), matchRoute("/app-installations/install-b"), location.search.slice(1),
);
await React.act(async () => {
  navigate(`/app-installations/install-b?${menuQuery}`);
  shellRoot.render(React.createElement(AppShell, {
    installId: "install-b", viewer: { operator: true, readOnly: false },
    children: React.createElement("p", null, "Social operational outlet"),
  }));
});
await flush(); await flush(); await flush();
assert(!location.search.includes("contract-preview"), "App menu installation switch clears both preview keys");
assert(!host.querySelector("[data-contract-preview]"), "new installation never stays behind an old fixture");
assert(text().includes("Social operational outlet"), "new installation opens its real outlet");
equal(host.querySelectorAll("[data-chat-pane]").length, 1, "installation switch preserves one chat pane");
equal(streams, 2, "installation switch reconnects the chat stream once (per-install legacy detection)");
await React.act(async () => { shellRoot.unmount(); });
console.log("app view contract checks passed");
}
void main().catch((error) => { console.error(error); require("process").exitCode = 1; });
