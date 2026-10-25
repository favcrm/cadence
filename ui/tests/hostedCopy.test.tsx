/**
 * CAD-1835: a hosted board must not render the local operator's copy.
 *
 * The "Since you left" card is built on the local daemon's `master_summary`
 * and the last-visit span of this browser. A hosted tenant has no daemon, so
 * the card must neither request the summary nor render its "needs a daemon"
 * line — an internal operation a member cannot act on.
 */
import type * as HappyDom from "happy-dom";
import type * as ReactNs from "react";
import type * as ReactDomClient from "react-dom/client";
import type * as LocaleModule from "../src/lib/locale";
import type SinceCardType from "../src/features/home/SinceCard";

declare function require(name: string): unknown;
declare const process: { exit(code: number): never };
export {};

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

type ModuleLoader = {
  prototype: { require: (this: unknown, id: string) => unknown };
};

function namedExport(mod: unknown, name: string): unknown {
  assert(mod && typeof mod === "object" && name in mod, `module must export ${name}`);
  return Reflect.get(mod, name);
}

async function main() {
const { Window } = require("happy-dom") as typeof HappyDom;
const win = new Window({ url: "http://localhost/" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage", "localStorage"] as const)
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"] as const)
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module") as ModuleLoader;
const originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, [id]);
};
const React = require("react") as typeof ReactNs;
const { createRoot } = require("react-dom/client") as typeof ReactDomClient;
const { LocaleProvider } = require("../src/lib/locale") as typeof LocaleModule;
const SinceCard = namedExport(require("../src/features/home/SinceCard"), "default") as typeof SinceCardType;

const KEY = "cadence-home-last-seen";
const seedAway = () => globalThis.localStorage.setItem(KEY, String(Math.floor(Date.now() / 1000) - 7200));
// A return visit long enough for the card's premise to hold, so the guard —
// not the away window — is what suppresses it.
seedAway();
const calls: string[] = [];
globalThis.fetch = (async (url: string) => {
  calls.push(String(url));
  return new Response("{}", { status: 200, headers: { "Content-Type": "application/json" } });
}) as typeof fetch;

const host = document.createElement("div");
document.body.appendChild(host);
const root = createRoot(host);
await React.act(async () => {
  root.render(React.createElement(LocaleProvider, null,
    React.createElement(SinceCard, { onOpenIssue: () => {}, hosted: true })));
});
await new Promise((resolve) => setTimeout(resolve, 50));
await React.act(async () => {});
assert(host.textContent === "", "a hosted board renders no since-you-left card");
assert(calls.length === 0, "a hosted board never requests the daemon summary");
await React.act(async () => { root.unmount(); });

// The same away state without `hosted` must fetch and render: otherwise the
// hosted assertions above would pass on a card that never had a premise.
seedAway();
const local = document.createElement("div");
document.body.appendChild(local);
const localRoot = createRoot(local);
await React.act(async () => {
  localRoot.render(React.createElement(LocaleProvider, null,
    React.createElement(SinceCard, { onOpenIssue: () => {} })));
});
await React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 60)); });
assert(calls.length > 0, "a local return visit requests the daemon summary");
assert(local.textContent !== "", "a local return visit renders the card");
await React.act(async () => { localRoot.unmount(); });
console.log("hosted copy checks passed");
}
void main().catch((e: unknown) => { console.error(e); process.exit(1); });
