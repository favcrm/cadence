/**
 * CAD-1638: independently authored acceptance check. The implementer must not
 * edit or weaken this check.
 *
 * A signed-out hosted tab must see only the sign-in gate. The former anonymous
 * directory branch must not render, and no catalogue UI may appear, when the
 * board answers every request with `board_session_required`.
 */
import type * as HappyDom from "happy-dom";
import type * as ReactNs from "react";
import type * as ReactDomClient from "react-dom/client";
import type * as LocaleModule from "../src/lib/locale";
import type AuthBoundaryType from "../src/features/auth/AuthBoundary";

declare function require(name: string): unknown;
declare const process: { exit(code: number): never };
export {};

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

type ModuleLoader = {
  prototype: { require: (this: unknown, id: string) => unknown };
};

/** Narrows an untyped module object to one named export without a cast. */
function namedExport(mod: unknown, name: string): unknown {
  assert(mod && typeof mod === "object" && name in mod, `module must export ${name}`);
  return Reflect.get(mod, name);
}

async function main() {
const { Window } = require("happy-dom") as typeof HappyDom;
const win = new Window({ url: "http://localhost/apps/catalog/crm" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"] as const)
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"] as const)
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });
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
const AuthBoundary = namedExport(require("../src/features/auth/AuthBoundary"), "default") as typeof AuthBoundaryType;

const REFUSAL = { ok: false, error: { code: "board_session_required", message: "a board session is required" } };
// Every request is refused the same way: no ETag, no 304, no retry. The
// conditional-get layer must surface the 401 as an ApiError on the first try.
let requests = 0;
globalThis.fetch = (async () => {
  requests += 1;
  return new Response(JSON.stringify(REFUSAL), { status: 401, headers: { "Content-Type": "application/json" } });
}) as typeof fetch;

const delay = (ms: number): Promise<void> => new Promise<void>((resolve) => { setTimeout(resolve, ms); });

const host = document.createElement("div");
document.body.appendChild(host);
const root = createRoot(host);
const flush = () => React.act(async () => { await delay(0); });
await React.act(async () => {
  root.render(React.createElement(LocaleProvider, null,
    React.createElement(AuthBoundary, { children: () => React.createElement("main", null, "PRIVATE WORKSPACE") })));
});
// Let the bootstrap check settle, then poll briefly for the gate to appear.
for (let i = 0; i < 20 && !host.textContent?.includes("Your work, behind a sign-in."); i++) {
  await flush();
  await delay(10);
}
await flush();

const text = host.textContent ?? "";
assert(text.includes("Your work, behind a sign-in."), "signed-out hosted tab shows the sign-in gate");
assert(text.includes("Continue with AgenticOS"), "signed-out hosted tab offers the platform sign-in");
assert(!text.includes("PRIVATE WORKSPACE"), "signed-out tab must not render the private workspace");
assert(!text.includes("All published apps"), "signed-out tab must not render catalogue UI");
assert(host.querySelector("[data-public-directory]") === null, "signed-out tab must not render the public directory");
assert(requests >= 1, "the boundary must have checked the session");
await React.act(async () => { root.unmount(); });
console.log("auth boundary directory checks passed");
}
void main().catch((e: unknown) => { console.error(e); process.exit(1); });
