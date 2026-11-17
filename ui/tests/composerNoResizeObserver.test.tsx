// CAD-2388: the Composer sizes itself from a ResizeObserver, but a host
// without the API (a bare DOM) must still mount it. No observer is created
// there; the initial measure stays.
import type * as HappyDOM from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDOM from "react-dom/client";
import type * as ComposerModule from "../src/features/app-shell/chat/Composer";
import type * as Locale from "../src/lib/locale";
declare function require<T>(name: string): T;
const { Window } = require<typeof HappyDOM>("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const key of ["window", "document", "navigator", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLButtonElement", "HTMLFormElement", "MutationObserver", "getComputedStyle", "localStorage", "sessionStorage"] as const)
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
  if (id === "react-markdown") return { __esModule: true, default: () => null };
  return originalRequire.call(this, id);
};
const React = require<typeof ReactModule>("react");
const { createRoot } = require<typeof ReactDOM>("react-dom/client");
const Composer = require<typeof ComposerModule>("../src/features/app-shell/chat/Composer").default;
const { LocaleProvider } = require<typeof Locale>("../src/lib/locale");

function assert(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }

async function main() {
  assert(typeof (globalThis as { ResizeObserver?: unknown }).ResizeObserver === "undefined", "this test needs a host without ResizeObserver");
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  await React.act(async () => {
    root.render(React.createElement(LocaleProvider, null, React.createElement(Composer, {
      density: "full", block: null, storeKey: "cad2388", onSend: () => undefined,
    })));
  });
  assert(host.querySelector("textarea"), "the composer mounts without ResizeObserver");
  await React.act(async () => { root.unmount(); });
  await win.happyDOM.close();
  console.log("Composer without ResizeObserver checks passed");
}
void main();
