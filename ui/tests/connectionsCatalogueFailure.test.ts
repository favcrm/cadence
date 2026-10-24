/**
 * CAD-1797 boundary: a 501 provider catalogue means "not carried on this
 * tenant board" and falls back to SMTP. A real catalogue failure stays a
 * failed read with its retry, not the fallback.
 */
declare function require(name: string): any;
import type { act as reactAct } from "react";

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
let act: typeof reactAct;
async function settle(fn: () => void, ms = 8000) {
  const end = Date.now() + ms;
  for (;;) {
    try { fn(); return; } catch (e) {
      if (Date.now() > end) throw e;
      await act(async () => sleep(20));
    }
  }
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/settings/connections/new" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLSelectElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener"])
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  Object.defineProperty(globalThis, "requestAnimationFrame", { value: (fn: () => void) => setTimeout(fn, 0), configurable: true });
  const loader = require("module"), originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, arguments);
  };

  const React = require("react") as typeof import("react");
  act = React.act;
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
  const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof import("../screen-ui/src/navigation");
  const { hostNavigation } = require("../src/ui/hostNavigation") as typeof import("../src/ui/hostNavigation");
  const Connections = (require("../src/features/settings/Connections") as typeof import("../src/features/settings/Connections")).default;

  const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
  globalThis.fetch = (async (input: unknown) => {
    const url = new URL(String(input), "http://localhost");
    if (url.pathname === "/api/connection-providers") return json({ error: "catalogue unavailable" }, 500);
    if (url.pathname === "/api/connections") return json({ connections: [] });
    throw new Error(`Unexpected ${url.pathname}`);
  }) as typeof fetch;

  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  act(() => {
    root.render(
      React.createElement(NavigationProvider, {
        adapter: hostNavigation,
        children: React.createElement(LocaleProvider, null,
          React.createElement(Connections, { viewer: { operator: true, readOnly: false }, page: { kind: "add" } }),
        ),
      }),
    );
  });
  const text = () => host.textContent ?? "";
  await settle(() => assert(text().includes("could not load the registered services"), "catalogue failure is shown"));
  assert(!text().includes("Your email address"), "a real failure does not fall back to SMTP");
  assert(!text().includes("Connect email"), "no credential form on a failed catalogue");
  assert(Array.from(host.querySelectorAll("button")).some((b) => (b.textContent ?? "").trim().toLowerCase() === "retry"), "retry remains available");
  await act(async () => root.unmount());
  console.log("connections catalogue failure checks passed");
}
void main();
