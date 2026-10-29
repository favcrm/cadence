/** CAD-1932 positive behaviour: the core drawer for any screen app.
 *
 *  Part 1 mounts the real ScreenOutlet for a NON-records screen.v2 frame
 *  that opted into `screen-surfaces.v1` (a Social-like app). Its open
 *  mounts a separate child frame in the shell target with the primary's
 *  host-held token, the primary stays in place, the `size` hint reaches the
 *  shell, and an unknown size is refused in place.
 *  Part 2 mounts the real AppShell: a `wide` open marks the panel wide and
 *  the default stays narrow.
 *  HTTP is an isolated fixture; the native parent proof is the daemon's.
 */
import type * as ReactModule from "react";
import type * as ReactDomModule from "react-dom/client";
import type * as LocaleModule from "../src/lib/locale";
import type ScreenOutletComponent from "../src/features/workspace-apps/screen/ScreenOutlet";
import type AppShellComponent from "../src/features/app-shell/AppShell";
import type { ScreenPush } from "../src/features/workspace-apps/screen/screenProtocol";
import type { ScreenActions } from "../src/features/workspace-apps/screen/screenLifecycle";
import type { Viewer } from "../src/features/projects/work";
declare function require(name: string): any;
export {};
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/apps/universal-drawer", settings: { fetch: { interceptor: {
  beforeAsyncRequest: async ({ window }: { window: any }) => new window.Response("<!doctype html><title>Isolated surface</title>", { headers: { "Content-Type": "text/html" } }),
} } } });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLDialogElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "CustomEvent", "MouseEvent", "KeyboardEvent", "FocusEvent", "location", "history", "sessionStorage", "localStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["getComputedStyle", "requestAnimationFrame", "cancelAnimationFrame"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener", "dispatchEvent"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
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
const { LocaleProvider } = require("../src/lib/locale") as typeof LocaleModule;
const ScreenOutlet = require("../src/features/workspace-apps/screen/ScreenOutlet").default as typeof ScreenOutletComponent;
const AppShell = require("../src/features/app-shell/AppShell").default as typeof AppShellComponent;
const flush = () => React.act(async () => { await Promise.resolve(); });

class Port {
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onmessageerror: (() => void) | null = null;
  sent: Record<string, unknown>[] = [];
  closed = false;
  start() {}
  close() { this.closed = true; }
  postMessage(value: Record<string, unknown>) { this.sent.push(value); }
  receive(data: unknown) { this.onmessage?.({ data }); }
}

async function outletPart() {
  const install = "social-like", context = "context-a", digest = "sha256:" + "c".repeat(64);
  // A plain screen.v2 receipt: no records contract, a live action context.
  const primaryReceipt = { mount: `/api/app-screen/${"b".repeat(64)}`, bridge_nonce: "a".repeat(64), generation: 1, tag: "main", action_token: "d".repeat(64), tools: [] as string[] };
  const childReceipt = { ...primaryReceipt, mount: `/api/app-screen/${"4".repeat(64)}`, bridge_nonce: "2".repeat(64), action_token: "3".repeat(64), generation: 2 };
  const projection: ScreenPush = {
    v: 2, op: "screen", install_id: install, digest, tag: "main", context_id: context,
    installation: { install_id: install, name: install, title: "Social-like", version: "1.0.0", digest, summary: "" },
    contexts: [{ id: context, label: "General" }], runs: [], outbox: [], now: 1,
    publish_intents: { status: "ok", withheld: 0, rows: [] },
  };
  const mounts: Record<string, unknown>[] = [];
  globalThis.fetch = async (input, init) => {
    if (String(input) === "/api/app-screen-tools/revoke" && init?.method === "POST")
      return new Response("{}", { headers: { "Content-Type": "application/json" } });
    assert(String(input) === `/api/app-installations/${install}/screens/main/mount` && init?.method === "POST", `unexpected request ${String(input)}`);
    mounts.push(JSON.parse(String(init.body)));
    return new Response(JSON.stringify(mounts.length === 1 ? primaryReceipt : childReceipt), { headers: { "Content-Type": "application/json" } });
  };
  const actions: Pick<ScreenActions, "call" | "planner"> = {
    call: async () => ({ ok: false, refusal: { code: "failed", text: "not in this fixture" } }),
    planner: async () => ({ code: "forbidden", text: "not in this fixture" }),
  };
  const host = document.createElement("section"), target = document.createElement("section");
  document.body.append(host, target);
  const effects: { mode: string; surface?: string; independent?: boolean; size?: string }[] = [];
  const handshake = async (frame: HTMLIFrameElement, receipt: typeof primaryReceipt) => {
    assert(frame.contentWindow, "the admitted surface has a real frame window");
    const port = new Port(), event = new win.Event("message");
    for (const [key, value] of Object.entries({ source: frame.contentWindow, origin: "null", ports: [port], data: { v: 1, op: "init", tag: "main", bridge_nonce: receipt.bridge_nonce, generation: receipt.generation } }))
      Object.defineProperty(event, key, { value });
    await React.act(async () => {
      win.dispatchEvent(event);
      port.receive({ v: 1, op: "ready", accepts: ["screen.v2", "screen-actions.v1", "screen-surfaces.v1"] });
    });
    await flush();
    assert(!port.closed, "a non-records opted-in frame completes the handshake");
    return port;
  };
  const send = async (port: Port, id: string, verb: string, args: Record<string, unknown>) => {
    await React.act(async () => { port.receive({ v: 2, op: "call", id, verb, args }); });
    await flush();
    return port.sent.find(message => message.op === "reply" && message.id === id);
  };
  const root = createRoot(host);
  await React.act(async () => root.render(React.createElement(LocaleProvider, null, React.createElement(ScreenOutlet, {
    projection, fallback: React.createElement("p", null, "Frame refused"), actions, drawerTarget: target,
    onScreenDrawer: (mode, surface, independent, size) => { effects.push({ mode, surface, independent, size }); },
  }))));
  await flush();
  const primaryFrame = host.querySelector("iframe");
  assert(primaryFrame, "the primary mounts in its normal outlet");
  const primary = await handshake(primaryFrame, primaryReceipt);
  assert(primary.sent.some(m => m.op === "surface" && m.role === "primary"), "the primary is told its host role");

  const badSize = await send(primary, "bad-size", "screen.drawer.open", { surface: "draft", size: "huge" });
  assert(badSize?.ok === false && (badSize.refusal as { code?: string }).code === "bad_args", "an unknown size is refused with bad_args");
  assert(!primary.closed && effects.length === 0 && !target.querySelector("iframe") && mounts.length === 1, "an unknown size changes nothing and keeps the frame");

  const open = await send(primary, "open", "screen.drawer.open", { surface: "draft", size: "wide" });
  assert(open?.ok === true && JSON.stringify(open.data) === JSON.stringify({ mode: "drawer", surface: "draft" }), "the open is granted with the closed reply shape");
  assert(JSON.stringify(effects) === JSON.stringify([{ mode: "drawer", surface: "draft", independent: true, size: "wide" }]), `the shell gets an independent wide open: ${JSON.stringify(effects)}`);
  await flush();
  const childFrame = target.querySelector("iframe");
  assert(childFrame && childFrame !== primaryFrame && host.querySelector("iframe") === primaryFrame, "a separate child frame enters the overlay; the primary stays in flow");
  assert(mounts[1]?.parent_action_token === primaryReceipt.action_token && mounts[1]?.context_id === context, "the child mount carries the host-held parent token and the same context");
  const child = await handshake(childFrame, childReceipt);
  assert(child.sent.some(m => m.op === "surface" && m.role === "drawer"), "the child is told it is the drawer");
  assert(primary.sent.some(m => m.op === "surface" && m.role === "primary" && m.modal === true), "the primary is modal while its child is open");

  const close = await send(child, "close", "screen.drawer.close", {});
  assert(close?.ok === true, "the child closes its own drawer");
  await flush();
  assert(!primary.closed && host.querySelector("iframe") === primaryFrame, "closing the drawer keeps the primary");
  await React.act(async () => root.unmount());
  host.remove(); target.remove();
}

async function shellPart() {
  const install = "social-like-shell", context = "context-a";
  const installation = {
    install_id: install, name: "social-content", title: "Social Content", version: "0.5.4",
    summary: "", digest: "sha256:" + "a".repeat(64), catalog_generation: "1", storage_kind: "workspace" as const,
    project_link: null, approved: true, executable: true, approval: { state: "approved" }, guide: "",
    files: ["workflows/instagram.md", "workflows/facebook.md", "workflows/source-instagram.md", "workflows/image-instagram.md", "workflows/image-manual.md"],
    capabilities: null, connection_slots: [],
  };
  const contexts = [{ id: context, install_id: install, revision: 1, digest: "sha256:" + "b".repeat(64), state: "active", config: { schema: 1, label: "General", input_defaults: {} } }];
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    const path = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
    const json = (value: unknown) => new Response(JSON.stringify(value), { headers: { "Content-Type": "application/json" } });
    if (path === `/api/app-installations/${install}`) return json(installation);
    if (path === `/api/app-installations/${install}/contexts`) return json({ contexts });
    return json({});
  }) as typeof fetch;
  type ScreenBridge = { onScreenDrawer?: (mode: "outlet" | "drawer", surface?: string, independent?: boolean, size?: "narrow" | "wide") => void };
  let bridge: ScreenBridge | null = null;
  const host = document.createElement("section");
  document.body.append(host);
  const viewer: Viewer = { operator: true, readOnly: false };
  const root = createRoot(host);
  await React.act(async () => root.render(React.createElement(LocaleProvider, null, React.createElement(AppShell, {
    installId: install, viewer,
    children: (_attach: unknown, _contextId: string, screen: ScreenBridge) => { bridge = screen; return React.createElement("div", { "data-surface": "list" }); },
  }))));
  for (let attempt = 0; attempt < 40 && !host.querySelector("[data-surface=list]"); attempt += 1) await flush();
  const drive = (bridge as ScreenBridge | null)?.onScreenDrawer;
  assert(drive, "the operator shell lends the drawer callback");
  await React.act(async () => { drive("drawer", "draft", true, "wide"); });
  await flush();
  assert((host.querySelector(".app-shell-screen-drawer") as HTMLElement | null)?.dataset.size === "wide", "a wide open marks the panel wide");
  await React.act(async () => { drive("outlet", undefined, true); });
  await flush();
  assert(host.querySelector(".app-shell-screen-drawer") === null, "the close removes the panel");
  await React.act(async () => { drive("drawer", "draft", true); });
  await flush();
  assert((host.querySelector(".app-shell-screen-drawer") as HTMLElement | null)?.dataset.size === "narrow", "an open without a size is today's narrow panel");
  await React.act(async () => root.unmount());
}

async function main() {
  await outletPart();
  await shellPart();
  await win.happyDOM.close();
  console.log("CAD-1932 universal drawer checks passed");
}
void main();
