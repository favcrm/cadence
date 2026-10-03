export {};
/**
 * CAD-866: shared host primitives (Field, DataTable, Detail, States) keep
 * the accessible label/error/table semantics the CRM and app-views
 * consumers hand-rolled, and the migrated consumers still expose the
 * same DOM contract (ids, roles, disabled previews, region overflow).
 *
 * These are the risky migrations: label↔control association, a focusable
 * scrollable table region, description-list semantics, and a disabled
 * form-preview control that must stay inert.
 */
declare function require(name: string): any;
declare const process: { cwd(): string };

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}
function assert(cond: unknown, what: string): void {
  if (!cond) throw new Error(what);
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/" });
  for (const name of [
    "window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement",
    "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator",
    "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent",
    "location", "history",
  ])
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  for (const name of ["addEventListener", "removeEventListener"])
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "crypto", {
    value: require("crypto").webcrypto,
    configurable: true,
  });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  const loader = require("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, arguments);
  };
  const React = require("react") as typeof import("react");
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const Field = (require("../src/features/app-shell/shared/Field") as typeof import("../src/features/app-shell/shared/Field")).default;
  const DataTable = (require("../src/features/app-shell/shared/DataTable") as typeof import("../src/features/app-shell/shared/DataTable")).default;
  const Detail = (require("../src/features/app-shell/shared/Detail") as typeof import("../src/features/app-shell/shared/Detail")).default;
  const States = require("../src/features/app-shell/shared/States") as typeof import("../src/features/app-shell/shared/States");
  const Select = (require("../src/ui/Select") as typeof import("../src/ui/Select")).default;

  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const flush = () => React.act(async () => { await new Promise((r) => setTimeout(r, 0)); });

  // --- Field: label/htmlFor association, describedby, invalid, disabled ---
  await React.act(async () => {
    root.render(
      React.createElement(Field, {
        id: "f-name",
        label: "Display name",
        hint: "As it appears",
        error: "Required",
        required: true,
        children: (c: any) =>
          React.createElement("input", { ...c, className: "field" }),
      }),
    );
  });
  await flush();
  const input = host.querySelector("#f-name") as HTMLInputElement | null;
  const label = host.querySelector('label[for="f-name"]');
  assert(input !== null && label !== null, "Field renders a labelled control with matching for/id");
  const el = input!;
  assert(label!.textContent?.includes("Display name"), "label text renders");
  const describedBy = el.getAttribute("aria-describedby") ?? "";
  assert(describedBy.includes("f-name-hint") && describedBy.includes("f-name-error"),
    "hint and error are linked via aria-describedby");
  equal(el.getAttribute("aria-invalid"), "true", "an error marks the control invalid");
  assert(host.querySelector("#f-name-error")?.getAttribute("role") === "alert",
    "the error message is an alert region");
  assert(host.querySelector("#f-name-hint")?.textContent?.includes("As it appears"),
    "the hint renders");
  assert(el.required, "required is forwarded to the control");

  // Field forwards disabled/readOnly and can drive a composite control.
  await React.act(async () => {
    root.render(
      React.createElement(Field, {
        id: "f-consent",
        label: "Email consent",
        disabled: true,
        children: (c: any) =>
          React.createElement(Select, {
            id: c.id,
            value: "unknown",
            onChange: () => {},
            options: [{ value: "unknown", label: "Unknown" }],
            disabled: c.disabled,
            "aria-label": "Email consent",
          }),
      }),
    );
  });
  await flush();
  assert(host.querySelector('[data-state="disabled"]'), "a disabled field marks its wrapper");
  assert(host.querySelector("#f-consent"), "the composite control keeps the field id");

  // --- DataTable: focusable labelled scroll region, headers, wrap ---
  await React.act(async () => {
    root.render(
      React.createElement(DataTable, {
        label: "Rows — scroll horizontally",
        wrapClassName: "crm-table-wrap",
        tableClassName: "crm-table",
        rowKey: (r: any) => r.id,
        columns: [
          { key: "name", header: "Name", cell: (r: any) => r.name },
          { key: "open", header: React.createElement("span", { className: "sr-only" }, "Open"), cell: () => "Open" },
        ],
        rows: [{ id: "a", name: "Alpha" }, { id: "b", name: "Beta" }],
      }),
    );
  });
  await flush();
  const region = host.querySelector('[role="region"]');
  assert(region, "the table renders inside a labelled scroll region");
  equal(region?.getAttribute("tabindex"), "0", "the region is keyboard-focusable to scroll");
  assert(region?.getAttribute("aria-label")?.includes("scroll horizontally"),
    "the region explains why it scrolls");
  equal(host.querySelectorAll('th[scope="col"]').length, 2, "column headers are scoped");
  assert(host.querySelector("table"), "a real table renders");
  assert(host.querySelector(".stable-wrap"), "the shared wrap class applies");

  // --- Detail: a real description list ---
  await React.act(async () => {
    root.render(
      React.createElement(Detail, {
        label: "Fields",
        items: [
          { key: "email", term: "Email", value: "a@b.c" },
          { key: "rev", term: "Revision", value: "r1" },
        ],
      }),
    );
  });
  await flush();
  assert(host.querySelector("dl"), "Detail renders a description list");
  equal(host.querySelectorAll("dt").length, 2, "each term renders a dt");
  equal(host.querySelectorAll("dd").length, 2, "each value renders a dd");

  // --- States: roles and retry ---
  let retried = 0;
  await React.act(async () => {
    root.render(
      React.createElement(React.Fragment, null,
        React.createElement(States.Loading, null, "Reading…"),
        React.createElement(States.ErrorNotice, { onRetry: () => { retried += 1; }, children: "Failed" }),
        React.createElement(States.EmptyState, { name: "customers", title: "None" }),
        React.createElement(States.Notice, { state: "read-only", children: "Read-only view." }),
      ),
    );
  });
  await flush();
  assert(host.querySelector('[role="status"]'), "loading/empty use status");
  assert(host.querySelector('[role="alert"]'), "the error uses alert");
  assert(host.querySelector('[data-empty="customers"]'), "empty state keeps its name marker");
  assert(host.querySelector('[data-state="read-only"]'), "notice keeps its state marker");
  (host.querySelector('[role="alert"] .lnk') as HTMLElement)?.dispatchEvent(
    new MouseEvent("click", { bubbles: true }),
  );
  await flush();
  equal(retried, 1, "the retry button invokes onRetry");

  await React.act(async () => { root.unmount(); });
  console.log("shared host ui checks passed");
}
void main();
