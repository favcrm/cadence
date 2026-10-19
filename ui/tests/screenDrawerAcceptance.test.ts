/** CAD-1615 acceptance 2: malformed or unsupported drawer requests never change host presentation.
 * Independently authored: exercise the mounted outlet, its port parser and the real action guard.
 */
import type { runCall } from "../src/features/workspace-apps/screen/screenActions";
import type { ScreenPush } from "../src/features/workspace-apps/screen/screenProtocol";

declare function require(name: string): any;
export {};
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

const { Window } = require("happy-dom");
const frameLoads: string[] = [];
const win = new Window({ url: "http://localhost/apps/drawer-acceptance", settings: { fetch: { interceptor: {
  beforeAsyncRequest: async ({ request, window }: { request: { url: string }; window: any }) => {
    frameLoads.push(new URL(request.url).pathname);
    return new window.Response("<!doctype html><title>Acceptance screen</title>", { headers: { "Content-Type": "text/html" } });
  },
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
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const ScreenOutlet = (require("../src/features/workspace-apps/screen/ScreenOutlet") as typeof import("../src/features/workspace-apps/screen/ScreenOutlet")).default;
const { runCall: call } = require("../src/features/workspace-apps/screen/screenActions") as { runCall: typeof runCall };

type Context = Parameters<typeof runCall>[0];
const installation = { install_id: "drawer-acceptance", name: "drawer-acceptance", title: "Drawer acceptance" } as Context["installation"];
const ctx: Context = { installation, installId: installation.install_id, contextId: "context-a", runs: [], onChanged: () => {} };
const nonce = "a".repeat(64);
const receipt = { mount: `/api/app-screen/${"b".repeat(64)}`, bridge_nonce: nonce, generation: 1, tag: "main" };
const projection: ScreenPush = {
  v: 2, op: "screen", install_id: installation.install_id, digest: "sha256:" + "c".repeat(64), tag: "main", context_id: "context-a",
  installation: { install_id: installation.install_id, name: installation.name, title: installation.title, version: "1.0.0", digest: "sha256:" + "c".repeat(64), summary: "" },
  contexts: [{ id: "context-a", label: "Acceptance" }], runs: [], outbox: [], now: 1,
  publish_intents: { status: "ok", withheld: 0, rows: [] }, actions: ["screen.drawer.open", "screen.drawer.close"],
};
let mounts = 0;
globalThis.fetch = async (input, init) => {
  assert(String(input) === "/api/app-installations/drawer-acceptance/screens/main/mount" && init?.method === "POST", "only the isolated screen mount may use HTTP");
  mounts++;
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
const flush = () => React.act(async () => { await new Promise(resolve => setTimeout(resolve, 0)); });
async function mount(shell: boolean, operator = true, presentation = true) {
  const root = createRoot(host);
  const effects: { mode: string; surface?: string }[] = [];
  const actions: React.ComponentProps<typeof ScreenOutlet>["actions"] = operator ? {
    call: (verb, args, ui) => call(ctx, verb, args, ui),
    planner: async () => { throw new Error("a drawer request must never enter a spending slot"); },
  } : undefined;
  await React.act(async () => root.render(React.createElement(LocaleProvider, null, React.createElement(ScreenOutlet, {
    projection, fallback: React.createElement("p", null, "Frame refused"), actions, presentation: "outlet",
    onScreenDrawer: shell ? (mode, surface) => { effects.push({ mode, ...(surface === undefined ? {} : { surface }) }); } : undefined,
  }))));
  await flush();
  const frame = host.querySelector("iframe");
  assert(frame?.contentWindow, "the real outlet mounted its frame");
  const port = new Port();
  const event = new win.Event("message");
  for (const [key, value] of Object.entries({ source: frame.contentWindow, origin: "null", ports: [port], data: { v: 1, op: "init", tag: "main", bridge_nonce: nonce, generation: 1 } }))
    Object.defineProperty(event, key, { value });
  await React.act(async () => {
    win.dispatchEvent(event);
    port.receive({ v: 1, op: "ready", accepts: presentation ? ["screen.v2", "screen-presentation.v1"] : ["screen.v2"] });
  });
  await flush();
  assert(!port.closed && port.sent.some(message => message.op === "screen"), "the frame completed the real handshake");
  assert(port.sent.some(message => message.op === "presentation") === presentation, "presentation messages require the child opt-in");
  const send = async (id: string, verb: string, args: Record<string, unknown>) => {
    await React.act(async () => { port.receive({ v: 2, op: "call", id, verb, args }); });
    await flush();
    return port.sent.find(message => message.op === "reply" && message.id === id);
  };
  return { port, effects, send, frame, close: () => React.act(async () => root.unmount()) };
}
function refused(reply: Record<string, unknown> | undefined, code: string, why: string) {
  assert(reply?.ok === false && (reply.refusal as { code?: string } | undefined)?.code === code, why);
}
async function main() {
  const live = await mount(true);
  try {
    const malformed: Record<string, unknown>[] = [
      { surface: "../settings" }, { surface: "" }, { surface: "x".repeat(65) }, { surface: 7 }, {}, { surface: "settings", extra: true },
    ];
    for (const [index, args] of malformed.entries()) {
      refused(await live.send(`bad-${index}`, "screen.drawer.open", args), "bad_args", "malformed drawer arguments are refused");
      assert(live.effects.length === 0 && !live.port.closed, "a refused request cannot change presentation or destroy a valid frame");
    }
    refused(await live.send("bad-close", "screen.drawer.close", { extra: true }), "bad_args", "close refuses extra arguments too");
    assert(live.effects.length === 0, "malformed close cannot change presentation");
    // A valid control proves the negative cases reached a live, drawer-capable host.
    const before = mounts;
    assert((await live.send("open", "screen.drawer.open", { surface: "settings" }))?.ok === true, "a valid control can open the drawer");
    assert((await live.send("close", "screen.drawer.close", {}))?.ok === true, "a valid control can close the drawer");
    assert(JSON.stringify(live.effects) === JSON.stringify([{ mode: "drawer", surface: "settings" }, { mode: "outlet" }]), "only valid requests reach the host callback");
    assert(host.querySelector("iframe") === live.frame && mounts === before, "drawer requests preserve the mounted frame");
    const effectsBeforeUnknown = JSON.stringify(live.effects);
    await live.send("unknown", "screen.drawer.toggle", {});
    assert(live.port.closed && !host.querySelector("iframe") && JSON.stringify(live.effects) === effectsBeforeUnknown, "an unknown drawer verb closes the port without dispatch");
  } finally { await live.close(); }

  const noShell = await mount(false, true, false);
  try {
    refused(await noShell.send("no-shell", "screen.drawer.open", { surface: "settings" }), "unavailable", "the real outlet refuses a frame with no shell callback");
    refused(await noShell.send("no-shell-close", "screen.drawer.close", {}), "unavailable", "a frame with no shell cannot claim to close a drawer");
    assert(!noShell.port.closed && noShell.effects.length === 0, "no-shell refusal changes no presentation");
  } finally { await noShell.close(); }

  const nonOperator = await mount(true, false, false);
  try {
    await nonOperator.send("non-operator", "screen.drawer.open", { surface: "settings" });
    assert(nonOperator.port.closed && nonOperator.effects.length === 0 && !host.querySelector("iframe"), "a frame without an action host cannot open a drawer");
  } finally { await nonOperator.close(); }
  assert(frameLoads.length > 0 && frameLoads.every(path => path === receipt.mount), "frame documents never leave the isolated HTTP interceptor");
  await win.happyDOM.close();
  console.log("CAD-1615 independent drawer refusal acceptance passed");
}
void main();
