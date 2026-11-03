/** CAD-2027: a hosted board offers only what it can run, and says so plainly. */
import { streamFixture } from "./streamFixture";
import type * as HappyDOM from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDOM from "react-dom/client";
import type * as HomeModule from "../src/features/home/Home";
import type * as Locale from "../src/lib/locale";
import type * as Stores from "../src/lib/resources";
declare function require<T>(name: string): T;
const { Window } = require<typeof HappyDOM>("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const key of ["window", "document", "navigator", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLButtonElement", "MutationObserver", "ResizeObserver", "IntersectionObserver", "getComputedStyle", "localStorage", "sessionStorage", "CustomEvent", "Event", "KeyboardEvent", "MouseEvent", "location", "history", "DOMParser"] as const)
  Object.defineProperty(globalThis, key, { value: key === "window" ? win : win[key], configurable: true });
for (const key of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"] as const)
  Object.defineProperty(globalThis, key, { value: win[key].bind(win), configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require<{ prototype: { require: (this: unknown, id: string) => unknown } }>("module");
const originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react" || id === "remark-gfm") return { HugeiconsIcon: () => null };
  if (id === "react-markdown") return { __esModule: true, default: ({ children }: { children: string }) => children };
  return originalRequire.call(this, id);
};
const React = require<typeof ReactModule>("react");
const { createRoot } = require<typeof ReactDOM>("react-dom/client");
const Home = require<typeof HomeModule>("../src/features/home/Home").default;
const { LocaleProvider } = require<typeof Locale>("../src/lib/locale");
const { resources } = require<typeof Stores>("../src/lib/resources");
const { setImmediate: nextTurn } = require<{ setImmediate: () => Promise<void> }>("node:timers/promises");

const master = {
  alias: "master", provider: "cadence", endpoint_kind: "durable_object", live: true,
  model: "deepseek/flash", model_label: "deepseek/flash", effort: null, context: null,
  identity: { name: null, tone: null, look: "robot", revision: "fixture" },
  session: { id: "home-thread", messages: null, streaming: false, compacting: false },
  commands: [] as string[], turn: null, queue: [], queued: 0,
};
const requests: string[] = [];
const json = (value: unknown) => new Response(JSON.stringify(value), { headers: { "Content-Type": "application/json" } });
globalThis.fetch = async (input, init) => {
  const url = new URL(String(input), "http://localhost");
  if (url.pathname.endsWith("/stream")) return streamFixture(url.href, master as never, init?.signal);
  requests.push(`${init?.method ?? "GET"} ${url.pathname}`);
  if (url.pathname === "/api/threads/master") return json({ thread: { id: "home-thread", alias: "master" }, conversation: "home-thread", entries: [], more_before: false });
  if (url.pathname === "/api/master/state") return json(master);
  if (url.pathname === "/api/agents") return json({ daemon: "reachable", agents: [{ alias: "master", state: "running" }], totals: null });
  if (url.pathname === "/api/meta") return json({ operator: true, read_only: false });
  return new Response(JSON.stringify({ ok: false }), { status: 501, headers: { "Content-Type": "application/json" } });
};
function assert(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }
async function main() {
  const host = document.createElement("div"); document.body.append(host);
  const root = createRoot(host);
  const settle = async () => { for (let i = 0; i < 6; i++) await React.act(async () => { await nextTurn(); }); };
  const mount = async (hosted: boolean) => {
    await React.act(async () => {
      root.render(React.createElement(LocaleProvider, null, React.createElement(Home, {
        readOnly: false, hosted, overview: resources.overview.get(),
        onOpenIssue: () => undefined, onOpenDecision: () => undefined,
        overviewHref: "/overview", permissionsHref: "/permissions", resolveAppPath: () => null,
      })));
    });
    await settle();
    await React.act(async () => {
      resources.agents.write(() => ({ daemon: "reachable", agents: [{ alias: "master", state: "running" }], totals: null }) as never);
    });
    await settle();
  };
  const box = () => host.querySelector<HTMLTextAreaElement>("[data-composer] textarea")!;
  const type = async (text: string) => {
    const setter = Object.getOwnPropertyDescriptor(win.HTMLTextAreaElement.prototype, "value")!.set!;
    await React.act(async () => {
      setter.call(box(), text);
      box().dispatchEvent(new win.Event("input", { bubbles: true }) as unknown as Event);
    });
    await settle();
  };
  const enter = async () => {
    await React.act(async () => {
      box().dispatchEvent(new win.KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }) as unknown as Event);
    });
    await settle();
  };
  const menu = () => Array.from(host.querySelectorAll(".slashmenu [role=option]")).map((row) => row.textContent ?? "");

  await mount(true);
  assert(box() !== null, "the composer is on the page");
  const chip = host.querySelector<HTMLButtonElement>(".master-chips .chip-btn");
  assert(chip?.disabled === true && !chip.textContent?.includes("▾"), "a hosted model chip is a read-out");
  assert(!requests.some((r) => r.includes("/api/master/models")), "a hosted chip reads no model list");

  await type("/");
  assert(menu().length === 1 && menu()[0].includes("help"), `a hosted menu offers help alone (got ${menu().length})`);

  requests.length = 0;
  await type("/compact");
  await enter();
  assert(!requests.some((r) => r.startsWith("POST")), `an unavailable command sends nothing (got ${requests.join(", ")})`);
  const card = host.querySelector('[data-cmd="compact"]');
  assert(card?.textContent?.includes("not available"), "the card says it is not available");
  assert(!card?.textContent?.includes("failed"), "an unavailable command did not fail");

  await type("/help");
  await enter();
  const help = host.querySelector('[data-cmd="help"]')?.textContent ?? "";
  assert(!help.includes("/compact") && !help.includes("/model"), "help lists only what this board can run");

  // A board that advertises a verb gets it in the menu and sends it.
  await React.act(async () => { root.render(null); });
  await settle();
  master.commands = ["new", "stop"];
  await React.act(async () => { resources.masterState.write(() => master as never); });
  await mount(true);
  await type("/");
  assert(menu().length === 3, `a hosted menu offers what the board advertises (got ${menu().length})`);
  requests.length = 0;
  await type("/new");
  await enter();
  assert(requests.includes("POST /api/master/command"), "an advertised command is sent");

  // A native board keeps everything, whatever its provider's partial list says.
  await React.act(async () => { root.render(null); });
  await settle();
  master.commands = ["state", "stats"];
  await React.act(async () => { resources.masterState.write(() => master as never); });
  requests.length = 0;
  await mount(false);
  await type("/");
  assert(menu().length === 10, `a native menu keeps every verb (got ${menu().length})`);
  assert(host.querySelector<HTMLButtonElement>(".master-chips .chip-btn")?.disabled === false, "a native model chip stays a picker");

  await React.act(async () => { root.unmount(); });
  await win.happyDOM.close();
  console.log("hosted commands checks passed");
}
void main();
