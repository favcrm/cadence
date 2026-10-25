/** CAD-1777 AppShell-level regression: Escape over a host dialog.
 *
 *  The host drawer's own window Escape listener must not dismiss the drawer
 *  while a `WorkspaceDialog` (a native modal `<dialog>`) is open above it:
 *  the same keypress belongs to the dialog, so the drawer and the child
 *  surface's unsaved draft survive, and the next Escape closes the drawer.
 *  This mounts the REAL AppShell and the REAL WorkspaceDialog; the surface
 *  inside the shell only calls the host callback the shell hands it.
 */
import type * as ReactModule from "react";
import type * as ReactDomModule from "react-dom/client";
import type * as LocaleModule from "../src/lib/locale";
import type AppShellComponent from "../src/features/app-shell/AppShell";
import type { Viewer } from "../src/features/projects/work";
declare function require(name: string): any;
export {};
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
const { Window } = require("happy-dom");
/** The interceptor's own window type as far as this stub uses it. */
type InterceptedWindow = { Response: typeof Response };
const win = new Window({ url: "http://localhost/apps/drawer-escape", settings: { fetch: { interceptor: {
  beforeAsyncRequest: async ({ window }: { window: InterceptedWindow }) =>
    new window.Response("{}", { headers: { "Content-Type": "application/json" } }),
} } } });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLDialogElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "CustomEvent", "MouseEvent", "KeyboardEvent", "FocusEvent", "location", "history", "sessionStorage", "localStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["getComputedStyle", "requestAnimationFrame", "cancelAnimationFrame"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener", "dispatchEvent"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
/** happy-dom does not implement the `:modal` pseudo-class, which a real browser
 *  does. Track `showModal`/`close` and answer `:modal` from that, so AppShell's
 *  guard is exercised exactly as written instead of being weakened for the
 *  harness. */
type DialogPrototype = { showModal(...args: unknown[]): void; close(...args: unknown[]): void };
const shownModally = new WeakSet<Element>();
const dialogPrototype = win.HTMLDialogElement.prototype as DialogPrototype;
const nativeShowModal = dialogPrototype.showModal;
dialogPrototype.showModal = function (this: Element, ...args: unknown[]) { shownModally.add(this); nativeShowModal.apply(this, args); };
const nativeClose = dialogPrototype.close;
dialogPrototype.close = function (this: Element, ...args: unknown[]) { shownModally.delete(this); nativeClose.apply(this, args); };
const nativeMatches = win.Element.prototype.matches as (this: Element, selector: string) => boolean;
win.Element.prototype.matches = function (this: Element, selector: string) {
  if (selector !== ":modal") return nativeMatches.call(this, selector);
  return (this as Partial<HTMLDialogElement>).tagName === "DIALOG" && (this as HTMLDialogElement).open && shownModally.has(this);
};
if (!("matchMedia" in win)) Object.defineProperty(win, "matchMedia", { value: () => ({ matches: false, addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {} }) });
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function(this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof ReactModule;
const { createRoot } = require("react-dom/client") as typeof ReactDomModule;
const { createPortal } = require("react-dom") as typeof import("react-dom");
const { LocaleProvider } = require("../src/lib/locale") as typeof LocaleModule;
const AppShell = require("../src/features/app-shell/AppShell").default as typeof AppShellComponent;
const { WorkspaceDialog } = require("../src/features/workspace-apps/WorkspaceDialog");

const install = "drawer-escape", context = "context-a";
const installation = {
  install_id: install, name: "social-content", title: "Social Content", version: "0.5.4",
  summary: "", digest: "sha256:" + "a".repeat(64), catalog_generation: "1", storage_kind: "workspace" as const,
  project_link: null, approved: true, executable: true, approval: { state: "approved" }, guide: "",
  files: ["workflows/instagram.md", "workflows/facebook.md", "workflows/source-instagram.md", "workflows/image-instagram.md", "workflows/image-manual.md"],
  capabilities: null, connection_slots: [],
};
const contexts = [{ id: context, install_id: install, revision: 1, digest: "sha256:" + "b".repeat(64), state: "active", config: { schema: 1, label: "General", input_defaults: {} } }];
const requests: string[] = [];
globalThis.fetch = (async (input: RequestInfo | URL) => {
  const path = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
  requests.push(path);
  const json = (value: unknown) => new Response(JSON.stringify(value), { headers: { "Content-Type": "application/json" } });
  if (path === `/api/app-installations/${install}`) return json(installation);
  if (path === `/api/app-installations/${install}/contexts`) return json({ contexts });
  return json({});
}) as typeof fetch;

const flush = () => React.act(async () => { await Promise.resolve(); });
const host = document.createElement("section");
document.body.append(host);
const viewer: Viewer = { operator: true, readOnly: false };

/** Exactly the host bridge the real AppShell hands its child. */
type ScreenBridge = {
  onScreenDrawer?: (mode: "outlet" | "drawer", surface?: string, independent?: boolean) => void;
  presentation: "outlet" | "drawer";
  drawerTarget: HTMLElement | null;
  onScreenClosed: () => void;
};

/** Stands in for the app's own detail surface inside the shell: it uses only
 *  the callback and portal target the real AppShell hands it. */
function Surface({ screen }: { screen: ScreenBridge }) {
  const [confirm, setConfirm] = React.useState(false);
  const target = screen.drawerTarget;
  return React.createElement(React.Fragment, null,
    React.createElement("button", { "data-open": "details", onClick: () => screen.onScreenDrawer?.("drawer", "customer", true) }, "Open details"),
    target ? createPortal(
      React.createElement("div", null,
        React.createElement("input", { "aria-label": "Customer name", "data-draft": "customer", defaultValue: "Ada (unsaved)" }),
        React.createElement("button", { "data-delete": "customer", onClick: () => setConfirm(true) }, "Delete customer"),
        confirm ? React.createElement(WorkspaceDialog, { title: "Delete customer?", onClose: () => setConfirm(false) },
          React.createElement("p", null, "This cannot be undone.")) : null,
      ), target) : null,
  );
}

function escape() {
  const event = new win.KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
  win.dispatchEvent(event);
  return event;
}

async function main() {
  const root = createRoot(host);
  await React.act(async () => root.render(
    React.createElement(LocaleProvider, null,
      React.createElement(AppShell, {
        installId: install,
        viewer,
        children: (_attach: unknown, _contextId: string, screen: ScreenBridge) => React.createElement(Surface, { screen }),
      }))),
  );
  for (let attempt = 0; attempt < 40 && !host.querySelector("[data-open=details]"); attempt += 1) await flush();
  assert(host.querySelector(".app-shell-grid"), "the real AppShell renders its workspace");
  assert(requests.some(path => path.endsWith(`/api/app-installations/${install}`)), `the real shell loads the installation receipt: ${requests.join(", ")}`);

  // The drawer is open with the app's own surface inside the host panel.
  const open = host.querySelector("[data-open=details]") as HTMLElement;
  assert(open, "the app surface mounted inside the shell outlet");
  await React.act(async () => { open.dispatchEvent(new win.MouseEvent("click", { bubbles: true })); });
  await flush();
  const panel = host.querySelector(".app-shell-screen-drawer") as HTMLElement;
  assert(panel, "the independent drawer opened through the host callback");
  const draft = panel.querySelector("[data-draft=customer]") as HTMLInputElement;
  assert(draft && draft.value === "Ada (unsaved)", "the child surface owns the draft inside the host panel");
  assert(host.querySelectorAll(".app-shell-screen-scrim").length === 1, "exactly one host scrim");

  // A host dialog opens over the drawer (the child's own confirmation).
  const remove = panel.querySelector("[data-delete=customer]") as HTMLElement;
  await React.act(async () => { remove.dispatchEvent(new win.MouseEvent("click", { bubbles: true })); });
  await flush();
  const dialog = document.querySelector("dialog[open]") as HTMLDialogElement | null;
  assert(dialog, "the host WorkspaceDialog is open above the drawer");
  assert(dialog?.getAttribute("aria-label") === "Delete customer?", "the opened dialog is the real WorkspaceDialog");

  // Escape first belongs to the dialog: the browser dispatch order is the
  // window keydown (AppShell's listener) then the dialog's own cancel.
  await React.act(async () => {
    escape();
    dialog!.dispatchEvent(new win.Event("cancel", { cancelable: true }));
  });
  await flush();
  assert(document.querySelector("dialog[open]") === null, "the dialog closes on Escape");
  assert(host.querySelector(".app-shell-screen-drawer"), "the drawer survives the dialog's Escape");
  assert((host.querySelector("[data-draft=customer]") as HTMLInputElement | null)?.value === "Ada (unsaved)", "the child draft survives the dialog's Escape");

  // The next Escape has no dialog above it and closes the drawer.
  await React.act(async () => { escape(); });
  await flush();
  assert(host.querySelector(".app-shell-screen-drawer") === null, "the next Escape closes the drawer");
  assert(host.querySelector("[data-draft=customer]") === null, "closing the drawer drops the child surface");
  assert(host.querySelector("[data-open=details]") !== null, "the primary surface stayed mounted throughout");

  // A NONMODAL dialog left open elsewhere on the board must not swallow the
  // drawer's Escape: only a modal host dialog owns the first keypress.
  await React.act(async () => {
    (host.querySelector("[data-open=details]") as HTMLElement).dispatchEvent(new win.MouseEvent("click", { bubbles: true }));
  });
  await flush();
  assert(host.querySelector(".app-shell-screen-drawer"), "the drawer reopens for the nonmodal case");
  const popover = document.createElement("dialog");
  popover.setAttribute("open", "");
  document.body.append(popover);
  assert(!popover.matches(":modal"), "the open popover dialog is not modal");
  await React.act(async () => { escape(); });
  await flush();
  assert(host.querySelector(".app-shell-screen-drawer") === null, "a nonmodal open dialog does not swallow the drawer's Escape");
  assert(popover.open, "the nonmodal dialog is left untouched");
  popover.remove();

  await React.act(async () => root.unmount());
  await win.happyDOM.close();
  console.log("CAD-1777 AppShell drawer Escape-vs-dialog acceptance passed");
}
void main();
