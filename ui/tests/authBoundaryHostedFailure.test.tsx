/**
 * CAD-1819: a hosted board must never fall back to the local-operator identity.
 *
 * The gate paints before /api/meta answers, and a failed check may never
 * answer. On a hosted origin every one of those states must show hosted copy —
 * a retry and a real sign-in — never "Private board" nor the `cadence ui login`
 * instruction, which a hosted member cannot run.
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

function namedExport(mod: unknown, name: string): unknown {
  assert(mod && typeof mod === "object" && name in mod, `module must export ${name}`);
  return Reflect.get(mod, name);
}

async function main() {
const { Window } = require("happy-dom") as typeof HappyDom;
const win = new Window({ url: "https://fav-limited.cadencecloud.app/issues/12" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"] as const)
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
const AuthBoundary = namedExport(require("../src/features/auth/AuthBoundary"), "default") as typeof AuthBoundaryType;

const delay = (ms: number): Promise<void> => new Promise<void>((resolve) => { setTimeout(resolve, ms); });

// A verification failure, after enough delay for the checking state to paint.
globalThis.fetch = (async () => {
  await delay(40);
  return new Response(JSON.stringify({ ok: false, error: { code: "capability_unavailable", message: "unavailable" } }), {
    status: 503,
    headers: { "Content-Type": "application/json" },
  });
}) as typeof fetch;

const host = document.createElement("div");
document.body.appendChild(host);
const root = createRoot(host);
const flush = () => React.act(async () => { await delay(0); });
await React.act(async () => {
  root.render(React.createElement(LocaleProvider, null,
    React.createElement(AuthBoundary, { children: () => React.createElement("main", null, "PRIVATE WORKSPACE") })));
});

// The checking state already knows this is a hosted board.
await flush();
const checking = host.textContent ?? "";
assert(checking.includes("Hosted workspace"), "checking a hosted board names it hosted");
assert(checking.includes("Verifying session & workspace access"), "checking copy is the hosted one");
assert(!checking.includes("Private board"), "checking a hosted board never says Private board");
assert(!checking.includes("cadence ui login"), "checking a hosted board never shows the CLI sign-in");

// Let the 503 settle into the unavailable state.
await React.act(async () => { await delay(120); });
await flush();
const settled = host.textContent ?? "";
assert(settled.includes("We couldn’t verify your session."), "verification failure is explained");
assert(settled.includes("Hosted workspace"), "failed verification keeps the hosted identity");
assert(settled.includes("Use another account"), "failed verification offers the hosted recovery path");
assert(settled.includes("Retry verification"), "failed verification offers a retry");
assert(!settled.includes("Private board"), "failed verification must not fall back to the private board");
assert(!settled.includes("Browser-bound Cadence session"), "no local session footer on a hosted board");
assert(!settled.includes("cadence ui login"), "no CLI instruction on a hosted board");
assert(!(host.textContent ?? "").includes("PRIVATE WORKSPACE"), "no workspace content before admission");
await React.act(async () => { root.unmount(); });
console.log("hosted gate failure checks passed");
}
void main().catch((e: unknown) => { console.error(e); process.exit(1); });
