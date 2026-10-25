/** CAD-1777: independently authored from the ticket, not the implementation.
 * Exercise the mounted records frame, real parser and local action dispatch.
 */
import type { ScreenPush } from "../src/features/workspace-apps/screen/screenProtocol";
import type * as ReactModule from "react";
import type * as ReactDomModule from "react-dom/client";
import type * as LocaleModule from "../src/lib/locale";
import type ScreenOutletComponent from "../src/features/workspace-apps/screen/ScreenOutlet";

declare function require(name: string): any;
export {};
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/apps/records-drawer-acceptance", settings: { fetch: { interceptor: {
  beforeAsyncRequest: async ({ window }: { window: any }) => new window.Response("<!doctype html><title>Records acceptance</title>", { headers: { "Content-Type": "text/html" } }),
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
const digest = "sha256:" + "c".repeat(64);
const receipt = {
  mount: `/api/app-screen/${"b".repeat(64)}`, bridge_nonce: "a".repeat(64), generation: 1, tag: "main",
  action_token: "d".repeat(64), tools: [], host_contract: "screen-records.v1", operations: ["records.list"], navigation: null,
  install_id: "records-drawer-acceptance", context_id: "context-a", digest,
  display_scope_key: "e".repeat(64), display_session_key: "f".repeat(64), remote_images: [],
};
const projection: ScreenPush = {
  v: 2, op: "screen", install_id: receipt.install_id, digest, tag: "main", context_id: receipt.context_id,
  installation: { install_id: receipt.install_id, name: receipt.install_id, title: "Records drawer", version: "1.0.0", digest, summary: "" },
  contexts: [{ id: receipt.context_id, label: "Acceptance" }], runs: [], outbox: [], now: 1,
  publish_intents: { status: "ok", withheld: 0, rows: [] }, actions: ["records.list"],
};
const requests: string[] = [];
globalThis.fetch = async (input, init) => {
  requests.push(String(input));
  // Frame teardown revokes its mount token; this is not a records operation.
  if (String(input) === "/api/app-screen-tools/revoke" && init?.method === "POST")
    return new Response("{}", { headers: { "Content-Type": "application/json" } });
  assert(String(input) === `/api/app-installations/${receipt.install_id}/screens/main/mount` && init?.method === "POST", "presentation must never reach a backend operation");
  return new Response(JSON.stringify(receipt), { headers: { "Content-Type": "application/json" } });
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
const host = document.createElement("section");
document.body.append(host);
const flush = () => React.act(async () => { await Promise.resolve(); });
async function mount(shell: boolean, optedIn = true) {
  const root = createRoot(host);
  const effects: string[] = [];
  function Harness() {
    const [mode, setMode] = React.useState<"outlet" | "drawer">("outlet");
    return React.createElement(LocaleProvider, null, React.createElement(ScreenOutlet, {
      projection, fallback: React.createElement("p", null, "Frame refused"), presentation: mode,
      onScreenDrawer: shell ? (next, surface) => { effects.push(`${next}:${surface ?? ""}`); setMode(next); } : undefined,
    }));
  }
  await React.act(async () => root.render(React.createElement(Harness)));
  await flush();
  const frame = host.querySelector("iframe");
  assert(frame?.contentWindow, "the real records outlet mounted");
  const port = new Port();
  const event = new win.Event("message");
  for (const [key, value] of Object.entries({ source: frame.contentWindow, origin: "null", ports: [port], data: { v: 1, op: "init", tag: "main", bridge_nonce: receipt.bridge_nonce, generation: 1 } }))
    Object.defineProperty(event, key, { value });
  await React.act(async () => {
    win.dispatchEvent(event);
    port.receive({ v: 1, op: "ready", accepts: ["screen.v2", "screen-records.v1", ...(optedIn ? ["screen-presentation.v1"] : [])] });
  });
  await flush();
  assert(!port.closed && port.sent.some(message => message.op === "screen"), "records handshake succeeded before exercising the guard");
  const send = async (id: string, verb: string, args: Record<string, unknown>) => {
    await React.act(async () => { port.receive({ v: 2, op: "call", id, verb, args }); });
    await flush();
    return port.sent.find(message => message.op === "reply" && message.id === id);
  };
  return { port, effects, frame, send, close: () => React.act(async () => root.unmount()) };
}
async function main() {
  const live = await mount(true);
  try {
    const before = requests.length;
    const bad = await live.send("forged", "screen.drawer.open", { surface: "customer", install_id: "another-install" });
    const refusal = bad?.refusal;
    assert(bad?.ok === false && refusal !== null && typeof refusal === "object" && "code" in refusal && refusal.code === "bad_args", "forged scope field is refused by the actual local guard");
    assert(!live.port.closed && live.effects.length === 0, "bad arguments neither open the drawer nor retire a valid frame");
    assert((await live.send("open", "screen.drawer.open", { surface: "customer" }))?.ok === true, "valid control opens an authorized negotiated records drawer");
    assert(live.port.sent.some(message => message.op === "presentation" && message.mode === "drawer"), "real host state reaches the same frame");
    assert((await live.send("close", "screen.drawer.close", {}))?.ok === true, "valid control closes the drawer");
    assert(JSON.stringify(live.effects) === JSON.stringify(["drawer:customer", "outlet:"]), "only validated presentation calls change geometry");
    assert(host.querySelector("iframe") === live.frame && requests.length === before, "open and close preserve the frame and make no backend call");
    await live.send("cross-contract", "social.drafts.create", {});
    assert(live.port.closed && !host.querySelector("iframe") && JSON.stringify(live.effects) === JSON.stringify(["drawer:customer", "outlet:"]), "presentation does not admit unrelated generic social actions");
  } finally { await live.close(); }

  for (const [label, shell, optedIn] of [["no-capable-host", false, true], ["no-negotiation", true, false]] as const) {
    const denied = await mount(shell, optedIn);
    try {
      const reply = await denied.send(label, "screen.drawer.open", { surface: "customer" });
      assert(denied.effects.length === 0 && (denied.port.closed || reply?.ok === false), `${label}: a forged call cannot change presentation`);
    } finally { await denied.close(); }
  }
  const retired = await mount(true);
  await retired.close();
  await retired.send("retired", "screen.drawer.open", { surface: "segment" });
  assert(retired.port.closed && retired.effects.length === 0, "a retired records frame cannot reopen host presentation");
  assert(requests.every(path => path === `/api/app-installations/${receipt.install_id}/screens/main/mount` || path === "/api/app-screen-tools/revoke"), `no presentation path used a records backend endpoint: ${JSON.stringify(requests)}`);
  await win.happyDOM.close();
  console.log("CAD-1777 independent records drawer guard acceptance passed");
}
void main();
