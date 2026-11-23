/** CAD-1932 refusal acceptance (reviewer-owned; the implementer must not
 *  edit or weaken it). Every case runs a NON-records screen.v2 frame
 *  through the real ScreenOutlet + ScreenChannel guard (and, for the
 *  read-only viewer, the real AppShell):
 *  1. a frame that did not opt into `screen-surfaces.v1` is refused in
 *     place (`unavailable`), the frame survives, nothing opens and no app
 *     action is called;
 *  2. an opted-in frame without a live action context, and one whose shell
 *     lends no drawer callback, are refused the same way;
 *  3. a read-only viewer's AppShell withholds the callback, so an opted-in
 *     frame inside it is refused in place;
 *  4. a drawer-role frame that sends `screen.drawer.open` is closed, and no
 *     second child is mounted;
 *  5. an unknown or malformed `size` (or an extra key) refuses `bad_args`
 *     and changes no geometry.
 *  HTTP is an isolated fixture; the native parent proof is covered by
 *  src/daemon/cad1932_drawer_acceptance.rs.
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
const win = new Window({ url: "http://localhost/apps/cad1932-refusal", settings: { fetch: { interceptor: {
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

type Receipt = { mount: string; bridge_nonce: string; generation: number; tag: string; action_token: string | null; tools: string[] };
const digest = "sha256:" + "c".repeat(64), context = "context-a";
// Plain screen.v2 receipts: no records contract.
const primaryReceipt: Receipt = { mount: `/api/app-screen/${"b".repeat(64)}`, bridge_nonce: "a".repeat(64), generation: 1, tag: "main", action_token: "d".repeat(64), tools: [] };
const childReceipt: Receipt = { ...primaryReceipt, mount: `/api/app-screen/${"4".repeat(64)}`, bridge_nonce: "2".repeat(64), action_token: "3".repeat(64), generation: 2 };
const projectionFor = (install: string): ScreenPush => ({
  v: 2, op: "screen", install_id: install, digest, tag: "main", context_id: context,
  installation: { install_id: install, name: install, title: "Social-like", version: "1.0.0", digest, summary: "" },
  contexts: [{ id: context, label: "General" }], runs: [], outbox: [], now: 1,
  publish_intents: { status: "ok", withheld: 0, rows: [] },
});
const OPTED_IN = ["screen.v2", "screen-actions.v1", "screen-surfaces.v1"];
const NOT_OPTED_IN = ["screen.v2", "screen-actions.v1"];

async function handshake(frame: HTMLIFrameElement, receipt: Receipt, accepts: string[]) {
  assert(frame.contentWindow, "the admitted surface has a real frame window");
  const port = new Port(), event = new win.Event("message");
  for (const [key, value] of Object.entries({ source: frame.contentWindow, origin: "null", ports: [port], data: { v: 1, op: "init", tag: "main", bridge_nonce: receipt.bridge_nonce, generation: receipt.generation } }))
    Object.defineProperty(event, key, { value });
  await React.act(async () => {
    win.dispatchEvent(event);
    port.receive({ v: 1, op: "ready", accepts });
  });
  await flush();
  assert(!port.closed, `the non-records frame completes the handshake (${accepts.join(",")})`);
  return port;
}
async function send(port: Port, id: string, verb: string, args: Record<string, unknown>) {
  await React.act(async () => { port.receive({ v: 2, op: "call", id, verb, args }); });
  await flush();
  return port.sent.find(message => message.op === "reply" && message.id === id);
}
const refusalCode = (reply: Record<string, unknown> | undefined) => reply?.ok === false ? (reply.refusal as { code?: string } | undefined)?.code : undefined;

type Effect = { mode: string; surface?: string; independent?: boolean; size?: string };

/** Mount one ScreenOutlet primary with the given receipts and shell
 *  callback; returns the handles the cases assert on. */
async function outlet(name: string, receipts: Receipt[], accepts: string[], withShell: boolean) {
  const install = `cad1932-${name}`;
  const mounts: Record<string, unknown>[] = [];
  const appCalls: string[] = [];
  globalThis.fetch = async (input, init) => {
    if (String(input) === "/api/app-screen-tools/revoke" && init?.method === "POST")
      return new Response("{}", { headers: { "Content-Type": "application/json" } });
    assert(String(input) === `/api/app-installations/${install}/screens/main/mount` && init?.method === "POST", `unexpected request ${String(input)}`);
    mounts.push(JSON.parse(String(init.body)));
    const receipt = receipts[mounts.length - 1];
    assert(receipt, `${name}: an unexpected extra mount (${mounts.length})`);
    return new Response(JSON.stringify(receipt), { headers: { "Content-Type": "application/json" } });
  };
  const actions: Pick<ScreenActions, "call" | "planner"> = {
    call: async (verb: string) => { appCalls.push(verb); return { ok: false, refusal: { code: "failed", text: "not in this fixture" } }; },
    planner: async () => ({ code: "forbidden", text: "not in this fixture" }),
  } as Pick<ScreenActions, "call" | "planner">;
  const host = document.createElement("section"), target = document.createElement("section");
  document.body.append(host, target);
  const effects: Effect[] = [];
  const root = createRoot(host);
  await React.act(async () => root.render(React.createElement(LocaleProvider, null, React.createElement(ScreenOutlet, {
    projection: projectionFor(install), fallback: React.createElement("p", null, "Frame refused"), actions, drawerTarget: target,
    onScreenDrawer: withShell ? (mode, surface, independent, size) => { effects.push({ mode, surface, independent, size }); } : undefined,
  }))));
  await flush();
  const frame = host.querySelector("iframe");
  assert(frame, `${name}: the primary mounts in its normal outlet`);
  const port = await handshake(frame, receipts[0], accepts);
  const done = async () => { await React.act(async () => root.unmount()); host.remove(); target.remove(); };
  return { port, frame, host, target, mounts, appCalls, effects, done };
}

/** A refused-in-place open: `unavailable`, the frame survives, nothing
 *  opens, no child mounts and no app action is reached. */
async function refusedInPlace(name: string, receipts: Receipt[], accepts: string[], withShell: boolean) {
  const o = await outlet(name, receipts, accepts, withShell);
  const reply = await send(o.port, "open", "screen.drawer.open", { surface: "draft" });
  assert(refusalCode(reply) === "unavailable", `${name}: the open is refused in place with unavailable: ${JSON.stringify(reply)}`);
  assert(!o.port.closed, `${name}: the refused frame survives`);
  assert(o.effects.length === 0 && !o.target.querySelector("iframe") && o.mounts.length === 1, `${name}: nothing opens and no child mounts`);
  assert(!o.appCalls.includes("screen.drawer.open"), `${name}: the drawer verb never reaches an app action`);
  const close = await send(o.port, "close", "screen.drawer.close", {});
  assert(refusalCode(close) === "unavailable" && !o.port.closed && o.effects.length === 0, `${name}: close is refused in place too`);
  await o.done();
}

async function notOptedIn() {
  await refusedInPlace("no-opt-in", [primaryReceipt], NOT_OPTED_IN, true);
}

async function noActionContext() {
  await refusedInPlace("no-action-context", [{ ...primaryReceipt, action_token: null }], OPTED_IN, true);
}

async function noShellCallback() {
  await refusedInPlace("no-shell-callback", [primaryReceipt], OPTED_IN, false);
}

/** The real AppShell for a read-only viewer: its bridge lends no drawer
 *  callback, so an opted-in frame rendered through it is refused. */
async function readOnlyViewer() {
  const install = "cad1932-read-only";
  const installation = {
    install_id: install, name: "social-content", title: "Social Content", version: "0.5.4",
    summary: "", digest, catalog_generation: "1", storage_kind: "workspace" as const,
    project_link: null, approved: true, executable: true, approval: { state: "approved" }, guide: "",
    files: ["workflows/instagram.md"], capabilities: null, connection_slots: [],
  };
  const contexts = [{ id: context, install_id: install, revision: 1, digest: "sha256:" + "b".repeat(64), state: "active", config: { schema: 1, label: "General", input_defaults: {} } }];
  const mounts: unknown[] = [];
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const path = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
    const json = (value: unknown) => new Response(JSON.stringify(value), { headers: { "Content-Type": "application/json" } });
    if (path === `/api/app-installations/${install}`) return json(installation);
    if (path === `/api/app-installations/${install}/contexts`) return json({ contexts });
    if (path === `/api/app-installations/${install}/screens/main/mount` && init?.method === "POST") { mounts.push(init.body); return json(mounts.length === 1 ? primaryReceipt : childReceipt); }
    return json({});
  }) as typeof fetch;
  type ScreenBridge = { onScreenDrawer?: (mode: "outlet" | "drawer", surface?: string, independent?: boolean, size?: "narrow" | "wide") => void; drawerTarget?: HTMLElement | null };
  let bridge: ScreenBridge | null = null;
  const appCalls: string[] = [];
  const actions: Pick<ScreenActions, "call" | "planner"> = {
    call: async (verb: string) => { appCalls.push(verb); return { ok: false, refusal: { code: "failed", text: "not in this fixture" } }; },
    planner: async () => ({ code: "forbidden", text: "not in this fixture" }),
  } as Pick<ScreenActions, "call" | "planner">;
  const host = document.createElement("section");
  document.body.append(host);
  const viewer: Viewer = { operator: true, readOnly: true };
  const root = createRoot(host);
  await React.act(async () => root.render(React.createElement(LocaleProvider, null, React.createElement(AppShell, {
    installId: install, viewer,
    children: (_attach: unknown, _contextId: string, screen: ScreenBridge) => {
      bridge = screen;
      return React.createElement(ScreenOutlet, {
        projection: projectionFor(install), fallback: React.createElement("p", null, "Frame refused"), actions,
        drawerTarget: screen.drawerTarget ?? null, onScreenDrawer: screen.onScreenDrawer,
      });
    },
  }))));
  for (let attempt = 0; attempt < 40 && !host.querySelector("iframe"); attempt += 1) await flush();
  assert(bridge !== null, "read-only: the shell rendered its screen children (the bridge exists)");
  assert((bridge as ScreenBridge).onScreenDrawer === undefined, "read-only: the shell withholds the drawer callback");
  const frame = host.querySelector("iframe");
  assert(frame, "read-only: the screen frame mounts through the shell");
  const port = await handshake(frame, primaryReceipt, OPTED_IN);
  const reply = await send(port, "open", "screen.drawer.open", { surface: "draft" });
  assert(refusalCode(reply) === "unavailable", `read-only: the open is refused in place: ${JSON.stringify(reply)}`);
  assert(!port.closed && mounts.length === 1 && host.querySelector(".app-shell-screen-drawer") === null, "read-only: the frame survives and no drawer panel or child appears");
  assert(!appCalls.includes("screen.drawer.open"), "read-only: the drawer verb never reaches an app action");
  await React.act(async () => root.unmount());
  host.remove();
}

/** A drawer-role frame may only close itself: its open closes it, and the
 *  primary keeps its place. Also: unknown sizes refuse bad_args. */
async function drawerOpensDrawerAndSize() {
  const o = await outlet("drawer-role", [primaryReceipt, childReceipt], OPTED_IN, true);
  for (const [id, args] of [
    ["size-huge", { surface: "draft", size: "huge" }],
    ["size-number", { surface: "draft", size: 1 }],
    ["size-null", { surface: "draft", size: null }],
    ["size-case", { surface: "draft", size: "WIDE" }],
    ["extra-key", { surface: "draft", size: "wide", width: 900 }],
  ] as [string, Record<string, unknown>][]) {
    const reply = await send(o.port, id, "screen.drawer.open", args);
    assert(refusalCode(reply) === "bad_args", `${id}: refused with bad_args: ${JSON.stringify(reply)}`);
    assert(!o.port.closed && o.effects.length === 0 && !o.target.querySelector("iframe") && o.mounts.length === 1, `${id}: no geometry change and the frame survives`);
  }
  const open = await send(o.port, "open", "screen.drawer.open", { surface: "draft" });
  assert(open?.ok === true, "positive control: the opted-in primary opens its drawer");
  await flush();
  const childFrame = o.target.querySelector("iframe");
  assert(childFrame && o.mounts.length === 2, "positive control: the child frame mounts in the overlay");
  const child = await handshake(childFrame, childReceipt, OPTED_IN);
  assert(child.sent.some(m => m.op === "surface" && m.role === "drawer"), "the child is told it is the drawer");
  const effectsBefore = o.effects.length;
  await send(child, "nested", "screen.drawer.open", { surface: "other" });
  assert(child.closed, "a drawer-role frame that opens a drawer is closed");
  await flush();
  assert(o.mounts.length === 2, "no second child mount follows a drawer-opens-drawer request");
  assert(!o.effects.slice(effectsBefore).some(e => e.mode === "drawer"), "the shell never receives the nested open");
  assert(!o.port.closed && o.host.querySelector("iframe") === o.frame, "the primary survives in place");
  await o.done();
}

async function main() {
  await notOptedIn();
  await noActionContext();
  await noShellCallback();
  await readOnlyViewer();
  await drawerOpensDrawerAndSize();
  await win.happyDOM.close();
  console.log("CAD-1932 drawer refusal acceptance passed");
}
void main();
