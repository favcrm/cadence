/** CAD-1777 independent acceptance: fixed overlay uses a separate admitted
 * surface; the list frame and its authority are not repurposed or exposed.
 * HTTP responses are isolated fixtures; real native parent proof is covered
 * by daemon::cad1777_drawer_acceptance, not claimed by this browser harness.
 */
import type * as ReactModule from "react";
import type * as ReactDomModule from "react-dom/client";
import type * as LocaleModule from "../src/lib/locale";
import type ScreenOutletComponent from "../src/features/workspace-apps/screen/ScreenOutlet";
import type { ScreenPush } from "../src/features/workspace-apps/screen/screenProtocol";
declare function require(name: string): any;
export {};
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/apps/independent-surface", settings: { fetch: { interceptor: {
  beforeAsyncRequest: async ({ window }: { window: any }) => new window.Response("<!doctype html><title>Isolated surface</title>", { headers: { "Content-Type": "text/html" } }),
} } } });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
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
const React = require("react") as typeof ReactModule;
const { createRoot } = require("react-dom/client") as typeof ReactDomModule;
const { LocaleProvider } = require("../src/lib/locale") as typeof LocaleModule;
const ScreenOutlet = require("../src/features/workspace-apps/screen/ScreenOutlet").default as typeof ScreenOutletComponent;
const install = "surface-acceptance", context = "context-a", digest = "sha256:" + "c".repeat(64);
const primaryReceipt = {
  mount: `/api/app-screen/${"b".repeat(64)}`, bridge_nonce: "a".repeat(64), generation: 1, tag: "main",
  action_token: "d".repeat(64), tools: [], host_contract: "screen-records.v1", operations: ["records.list"], navigation: null,
  install_id: install, context_id: context, digest, display_scope_key: "e".repeat(64), display_session_key: "f".repeat(64), remote_images: [],
};
const childReceipt = { ...primaryReceipt, mount: `/api/app-screen/${"4".repeat(64)}`, bridge_nonce: "2".repeat(64), action_token: "3".repeat(64), generation: 2 };
const projection: ScreenPush = {
  v: 2, op: "screen", install_id: install, digest, tag: "main", context_id: context,
  installation: { install_id: install, name: install, title: "Independent surface", version: "1.0.0", digest, summary: "" },
  contexts: [{ id: context, label: "Acceptance" }], runs: [], outbox: [], now: 1,
  publish_intents: { status: "ok", withheld: 0, rows: [] }, actions: ["records.list"],
};
const mounts: Record<string, unknown>[] = [];
globalThis.fetch = async (input, init) => {
  if (String(input) === "/api/app-screen-tools/revoke" && init?.method === "POST")
    return new Response("{}", { headers: { "Content-Type": "application/json" } });
  assert(String(input) === `/api/app-installations/${install}/screens/main/mount` && init?.method === "POST", "surface creation uses only the isolated admitted mount endpoint");
  const body = JSON.parse(String(init.body));
  mounts.push(body);
  assert(mounts.length <= 2, "a drawer frame cannot recursively mint another drawer");
  return new Response(JSON.stringify(mounts.length === 1 ? primaryReceipt : childReceipt), { headers: { "Content-Type": "application/json" } });
};
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
const host = document.createElement("section"), target = document.createElement("section");
document.body.append(host, target);
const flush = () => React.act(async () => { await Promise.resolve(); });
async function handshake(frame: HTMLIFrameElement, receipt: typeof primaryReceipt) {
  assert(frame.contentWindow, "the admitted surface has a real frame window");
  const port = new Port(), event = new win.Event("message");
  for (const [key, value] of Object.entries({ source: frame.contentWindow, origin: "null", ports: [port], data: { v: 1, op: "init", tag: "main", bridge_nonce: receipt.bridge_nonce, generation: receipt.generation } }))
    Object.defineProperty(event, key, { value });
  await React.act(async () => {
    win.dispatchEvent(event);
    port.receive({ v: 1, op: "ready", accepts: ["screen.v2", "screen-records.v1", "screen-surfaces.v1"] });
  });
  await flush();
  assert(!port.closed, "the actual source/nonce/generation handshake admits this surface");
  return port;
}
async function send(port: Port, id: string, verb: string, args: Record<string, unknown>) {
  await React.act(async () => { port.receive({ v: 2, op: "call", id, verb, args }); });
  await flush();
  return port.sent.find(message => message.op === "reply" && message.id === id);
}
async function main() {
  const root = createRoot(host);
  const effects: boolean[] = [];
  function Harness() {
    const [mode, setMode] = React.useState<"outlet" | "drawer">("outlet");
    return React.createElement(LocaleProvider, null, React.createElement(ScreenOutlet, {
      projection, fallback: React.createElement("p", null, "Frame refused"), presentation: mode, drawerTarget: target,
      onScreenDrawer: (next, _surface, independent) => { effects.push(independent === true); setMode(next); },
    }));
  }
  await React.act(async () => root.render(React.createElement(Harness)));
  await flush();
  const primaryFrame = host.querySelector("iframe");
  assert(primaryFrame, "the primary surface mounts in its normal outlet");
  const primary = await handshake(primaryFrame, primaryReceipt);
  assert(primary.sent.findIndex(m => m.op === "surface" && m.role === "primary") < primary.sent.findIndex(m => m.op === "screen") && primary.sent.some(m => m.op === "surface" && m.role === "primary"), "host role arrives before primary rendering data");
  const forged = await send(primary, "forged-parent", "screen.drawer.open", { surface: "customer", parent_action_token: "forged" });
  assert(forged?.ok === false && !target.querySelector("iframe") && mounts.length === 1, "a frame cannot choose parent authority through drawer arguments");
  assert((await send(primary, "open", "screen.drawer.open", { surface: "customer" }))?.ok === true, "valid independent open reaches the host");
  assert(effects[0] === true, "only actual surface negotiation selects independent presentation");
  const childFrame = target.querySelector("iframe");
  assert(childFrame && host.querySelector("iframe") === primaryFrame && childFrame !== primaryFrame, "only a new detail frame enters the overlay target; primary stays mounted in place");
  assert(mounts[1]?.parent_action_token === primaryReceipt.action_token, "child mint uses the host-held live parent handle rather than caller arguments");
  const child = await handshake(childFrame, childReceipt);
  assert(child.sent.some(m => m.op === "surface" && m.role === "drawer") && child.sent.findIndex(m => m.op === "surface") < child.sent.findIndex(m => m.op === "screen"), "child role is host-assigned before any child screen projection");
  await send(child, "nested", "screen.drawer.open", { surface: "segment" });
  assert(mounts.at(1) !== undefined && mounts.at(2) === undefined && host.querySelector("iframe") === primaryFrame && !primary.closed, "nested drawer requests cannot create a third mount or retire the list frame");
  const messages = JSON.stringify([...primary.sent, ...child.sent]);
  assert(!messages.includes(primaryReceipt.action_token) && !messages.includes(childReceipt.action_token), "host-held action handles never cross either child port");
  await React.act(async () => root.unmount());
  assert(primary.closed && child.closed && !host.querySelector("iframe") && !target.querySelector("iframe"), "retiring the parent closes both owned ports and removes the overlay frame");
  await win.happyDOM.close();
  console.log("CAD-1777 independent paired surface UI guard acceptance passed");
}
void main();
