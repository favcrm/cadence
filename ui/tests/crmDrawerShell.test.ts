/**
 * CAD-1052 CRM drawer shell: header, tabs, pinned footer, overflow menu,
 * in-place edit mode, scrim scope, Escape layering and focus return.
 */
export {};
declare function require(name: string): any;
declare const process: any;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history"])
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener"])
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  const loader = require("module"), originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, arguments);
  };
  const React = require("react") as typeof import("react");
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const DrawerShell = require("../src/features/app-shell/shared/DrawerShell").default as typeof import("../src/features/app-shell/shared/DrawerShell").default;
  const fs = require("fs");
  const h = React.createElement;

  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  const flush = () => React.act(async () => { await sleep(0); });
  const q = <T extends Element = HTMLElement>(sel: string) => host.querySelector(sel) as T | null;
  const click = async (el: Element | null | undefined) => {
    assert(el, "click target exists");
    await React.act(async () => { el.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
    await flush();
  };
  const key = async (k: string) => {
    await React.act(async () => { document.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true })); });
    await flush();
  };

  const st = { closed: 0, deleted: 0, saved: 0 };
  const closedCount = (): number => st.closed;
  let editing = false;
  function Harness() {
    const [edit, setEdit] = React.useState(false);
    editing = edit;
    return h(DrawerShell, {
      kind: "customer",
      label: "Customer details",
      title: "Ada Lovelace",
      subtitle: "ada@example.com",
      avatar: "AL",
      pills: h("span", { className: "chip" }, "Email: unknown"),
      warning: "Ada cannot receive campaigns",
      tabs: [
        { id: "overview", label: "Overview", panel: h("p", null, "overview panel") },
        { id: "activity", label: "Activity", panel: h("p", null, "activity panel") },
        { id: "details", label: "Details", panel: h("p", null, "details panel") },
      ],
      menu: [
        { key: "copy", label: "Copy email", onSelect: () => {} },
        { key: "del", label: "Archive", destructive: true, onSelect: () => { st.deleted += 1; } },
      ],
      secondary: h("button", { type: "button", className: "btn btn-sm" }, "Secondary"),
      primary: h("button", { type: "button", className: "btn btn-sm btn-primary", onClick: () => setEdit(true) }, "Edit profile"),
      edit: edit
        ? {
            formId: "f1",
            title: "Edit profile",
            onCancel: () => setEdit(false),
            body: h("form", { id: "f1", onSubmit: (e: Event) => { e.preventDefault(); st.saved += 1; } }, h("input", { id: "name", defaultValue: "Ada" })),
          }
        : null,
      onClose: () => { st.closed += 1; },
    });
  }

  // Layout: the chat pane is a sibling of the outlet; the scrim starts at the outlet's edge.
  const outlet = document.createElement("section");
  outlet.className = "app-shell-outlet";
  outlet.getBoundingClientRect = () => ({ left: 300, top: 0, right: 1440, bottom: 900, width: 1140, height: 900, x: 300, y: 0, toJSON() {} }) as DOMRect;
  document.body.append(outlet);
  const heading = document.createElement("h3");
  heading.setAttribute("data-outlet-heading", "");
  heading.textContent = "Customers";
  outlet.append(heading);
  Object.defineProperty(win, "innerWidth", { value: 1440, configurable: true });

  await React.act(async () => { root.render(h(Harness)); });
  await flush();

  // Header: avatar, title, subtitle, pills, warning strip with no controls.
  assert(q("h3.crm-drawer-title")?.textContent === "Ada Lovelace", "title renders");
  assert(q(".crm-drawer-sub")?.textContent === "ada@example.com", "subtitle renders");
  assert(q(".crm-drawer-avatar")?.textContent === "AL", "avatar renders");
  assert(q(".crm-drawer-pills .chip"), "status pills render");
  const warn = q(".crm-drawer-warn");
  assert(warn && warn.querySelectorAll("button, a").length === 0, "warning strip carries no buttons");
  assert(document.activeElement === q("h3.crm-drawer-title"), "drawer lands focus on its heading");

  // Tabs: only the active panel shows.
  assert(host.textContent?.includes("overview panel") && !host.textContent.includes("activity panel"), "overview first");
  await click(Array.from(host.querySelectorAll('[role="tab"]')).find((e) => e.textContent === "Activity"));
  assert(host.textContent?.includes("activity panel") && !host.textContent.includes("overview panel"), "tab switches panel");
  await click(Array.from(host.querySelectorAll('[role="tab"]')).find((e) => e.textContent === "Overview"));

  // All actions live in the pinned footer; header and body hold no action buttons.
  const foot = q(".crm-drawer-foot");
  assert(foot, "footer renders");
  const labels = Array.from(foot.querySelectorAll("button")).map((b) => b.getAttribute("aria-label") ?? b.textContent);
  assert(labels.join("|") === "More actions|Secondary|Edit profile", `footer order menu, secondary, primary: ${labels.join("|")}`);
  assert(foot.querySelectorAll(".btn-primary").length === 1, "exactly one primary action");
  const strays = Array.from(host.querySelectorAll(".crm-drawer-body button")).length;
  assert(strays === 0, "body holds no action buttons");
  const topButtons = Array.from(q(".crm-drawer-top")!.querySelectorAll("button")).filter((b) => b.getAttribute("role") !== "tab");
  assert(topButtons.length === 1 && topButtons[0].getAttribute("aria-label") === "Close customer details", "header holds only the icon close");

  // Overflow menu: opens, destructive item last and red, Escape closes the menu only.
  const menuBtn = q('button[aria-label="More actions"]')!;
  await click(menuBtn);
  assert(menuBtn.getAttribute("aria-expanded") === "true", "menu opens");
  const items = Array.from(host.querySelectorAll('[role="menuitem"]'));
  assert(items.length === 2 && items[1].className === "danger", "destructive item is last and marked");
  await key("Escape");
  assert(!q('[role="menu"]') && closedCount() === 0 && !q('[data-closing]'), "Escape closes the menu before the drawer");
  await click(menuBtn);
  await click(Array.from(host.querySelectorAll('[role="menuitem"]'))[1]);
  assert(st.deleted === 1 && !q('[role="menu"]'), "destructive item fires once and closes the menu");
  assert(document.activeElement === menuBtn, "focus returns to the menu trigger");

  // Scrim covers the outlet, not the chat pane.
  assert(q<HTMLElement>(".crm-drawer-scrim")?.style.left === "300px", "scrim starts at the outlet edge");

  // Edit mode: the body swaps in place, the footer becomes Cancel / Save, no read view below.
  await click(Array.from(foot.querySelectorAll("button")).find((b) => b.textContent === "Edit profile"));
  assert(editing && q("#name"), "edit form replaces the body");
  assert(!host.textContent?.includes("overview panel") && !q('[role="tablist"]'), "no duplicated read view or tabs in edit mode");
  assert(q("[data-mode='edit']"), "mode is exposed");
  const editLabels = Array.from(q(".crm-drawer-foot")!.querySelectorAll("button")).map((b) => b.textContent);
  assert(editLabels.join("|") === "Cancel|Save", `edit footer is Cancel / Save: ${editLabels.join("|")}`);
  assert(!q(".crm-drawer-warn"), "warning strip yields to the form");
  await click(Array.from(q(".crm-drawer-foot")!.querySelectorAll("button")).find((b) => b.textContent === "Save"));
  assert(st.saved === 1, "footer Save submits the form");
  await key("Escape");
  assert(!editing && closedCount() === 0, "Escape cancels the edit before closing the drawer");
  assert(host.textContent?.includes("overview panel"), "cancel restores the read view");

  // Scrim click closes (fallback timer unmounts); focus returns to the outlet heading with no opener.
  await click(q(".crm-drawer-scrim"));
  assert(q("[data-closing]"), "closing is marked");
  await React.act(async () => { await sleep(420); });
  assert(closedCount() === 1, "onClose fires once");
  assert(document.activeElement === heading, "deep-linked drawer returns focus to the outlet heading, not body");
  await React.act(async () => { root.unmount(); });

  // Unified DrawerShell features: single-view children + size + canonical classes + preventClose + nested LIFO stack.
  const host2 = document.createElement("div");
  document.body.append(host2);
  const root2 = createRoot(host2);
  const stackEvents = { parentClosed: 0, childClosed: 0 };
  const parentClosed = (): number => stackEvents.parentClosed;
  const childClosed = (): number => stackEvents.childClosed;
  function StackHarness() {
    const [showParent, setShowParent] = React.useState(true);
    const [showChild, setShowChild] = React.useState(true);
    const [guarded, setGuarded] = React.useState(true);
    return h(
      React.Fragment,
      null,
      showParent &&
        h(
          DrawerShell,
          {
            kind: "parent-doc",
            label: "Parent drawer",
            title: "Parent",
            scrim: "board",
            size: "sm",
            modal: true,
            trapFocus: true,
            onClose: () => {
              stackEvents.parentClosed += 1;
              setShowParent(false);
            },
          },
          h("p", null, "single-view body"),
        ),
      showChild &&
        h(
          DrawerShell,
          {
            kind: "child-doc",
            label: "Child drawer",
            title: "Child",
            scrim: "board",
            size: "lg",
            modal: true,
            trapFocus: true,
            preventClose: guarded,
            preventCloseNote: "Saving...",
            primary: h(
              "button",
              { type: "button", className: "btn btn-sm btn-primary", onClick: () => setGuarded(false) },
              "Unlock",
            ),
            onClose: () => {
              stackEvents.childClosed += 1;
              setShowChild(false);
            },
          },
          h("p", null, "nested child body"),
        ),
    );
  }
  await React.act(async () => { root2.render(h(StackHarness)); });
  await flush();

  const parentPanel = host2.querySelector('[data-drawer="parent-doc"]') as HTMLElement | null;
  const childPanel = host2.querySelector('[data-drawer="child-doc"]') as HTMLElement | null;
  assert(parentPanel && childPanel, "nested drawers render together");
  assert(parentPanel.classList.contains("s-drawer") && host2.querySelector(".s-drawer-scrim"), "canonical .s-drawer classes are present");
  assert(parentPanel.getAttribute("data-size") === "sm" && childPanel.getAttribute("data-size") === "lg", "size attributes apply");
  assert(host2.textContent?.includes("single-view body"), "children single-view renders without tabs");
  assert(parentPanel.getAttribute("data-receded") === "true" && parentPanel.hasAttribute("inert"), "parent recedes while child is open");
  assert(parentPanel.getAttribute("data-depth") === null && childPanel.getAttribute("data-depth") === "1", "child takes depth 1");

  // Guarded top drawer refuses Escape/scrim, marks data-nudged, and surfaces preventCloseNote.
  await key("Escape");
  assert(childClosed() === 0 && parentClosed() === 0, "preventClose blocks Escape");
  assert(childPanel.getAttribute("data-nudged") === "true", "preventClose triggers nudge feedback");
  assert(childPanel.querySelector(".s-drawer-note")?.textContent === "Saving...", "preventCloseNote surfaces in footer");

  // Unlock top drawer: Escape closes only child first (LIFO), un-recedes parent, then second Escape closes parent.
  await click(Array.from(childPanel.querySelectorAll("button")).find((b) => b.textContent === "Unlock"));
  await key("Escape");
  assert(childPanel.hasAttribute("data-closing") && !parentPanel.hasAttribute("data-closing"), "Escape closes only top child drawer");
  await React.act(async () => { await sleep(320); });
  assert(childClosed() === 1 && parentClosed() === 0, "only child unmounted on first Escape");
  assert(!parentPanel.hasAttribute("data-receded") && !parentPanel.hasAttribute("inert"), "parent clears receded and inert after child closes");
  await key("Escape");
  await React.act(async () => { await sleep(320); });
  assert(parentClosed() === 1, "second Escape closes parent drawer");
  await React.act(async () => { root2.unmount(); });
  host2.remove();

  // Reduced motion and layout fixes are in the sheet.
  const nativeCss = fs.readFileSync(require("path").join(process.cwd(), "src/features/app-shell/shared/drawer.css"), "utf8");
  assert(nativeCss.includes('@import "../../../../screen-ui/src/drawer.css";'), "native drawer imports the shared drawer styles");
  const css = fs.readFileSync(require("path").join(process.cwd(), "screen-ui/src/drawer.css"), "utf8") + nativeCss;
  assert(/prefers-reduced-motion: reduce\)[^]*crm-drawer-scrim[^]*animation: none/.test(css), "scrim honours reduced motion");
  assert(/\.crm-drawer\s*\{[^}]*\banimation:\s*none;\s*transition:\s*none;/.test(css), "drawer honours reduced motion");
  assert(/\.crm-field \.select-trigger \{\s*min-width: 0;/.test(css) || /\.crm-field \.select,\s*\.crm-field \.select-trigger \{\s*min-width: 0;/.test(css), "selects shrink to their column");
  assert(/\.crm-field-row \{\s*align-items: end;/.test(css), "hinted fields align with their row");

  console.log("crm drawer shell checks passed");
}

void main();
