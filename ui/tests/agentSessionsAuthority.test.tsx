import { streamFixture } from "./streamFixture";
import type * as HappyDom from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDOMClient from "react-dom/client";
import type * as AppModule from "../src/App";
import type * as LocaleModule from "../src/lib/locale";

declare function require(name: string): unknown;
declare const process: { exit(code: number): never };

function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}

/** CAD-2562: neither tenant role can mount or fetch the retired explorer. */
async function main() {
  const ref = "as_0123456789abcdef0123456789abcdef";
  const direct = `/agents?sessions=${ref}`;
  const { Window } = require("happy-dom") as typeof HappyDom;
  const win = new Window({ url: `https://fav-limited.cadencecloud.app${direct}` });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage", "localStorage"] as const)
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"] as const)
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  const { webcrypto } = require("crypto") as { webcrypto: Crypto };
  Object.defineProperty(globalThis, "crypto", { value: webcrypto, configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  const loader = require("module") as { prototype: { require: (this: unknown, id: string) => unknown } };
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    if (id === "react-markdown") return { __esModule: true, default: () => null };
    if (id === "remark-gfm") return { __esModule: true, default: () => undefined };
    return originalRequire.call(this, id);
  };
  const React = require("react") as typeof ReactModule;
  const { createRoot } = require("react-dom/client") as typeof ReactDOMClient;
  const App = (require("../src/App") as typeof AppModule).default;
  const { LocaleProvider } = require("../src/lib/locale") as typeof LocaleModule;
  const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
  let role: "member" | "operator" = "member";
  const diagnostics: string[] = [];
  globalThis.fetch = (async (input: unknown) => {
    const path = String(input);
    const url = new URL(path, location.origin);
    if (url.pathname.startsWith("/api/agent-sessions")) {
      diagnostics.push(url.pathname + url.search);
      return json({ error: "unexpected diagnostics request" }, 500);
    }
    if (url.pathname.endsWith("/stream")) return streamFixture(path);
    if (url.pathname === "/api/meta") return json({ hosted: true, read_only: false, operator: true, signed_in: true, actor: `${role} (ui)`, session: {
      id: `authority-${role}`, origin: "public", created: 1700000000, last_used: 1700000000,
      idle_expires_at: 4102444800, expires_at: 4102444800, user_agent: "test",
      user: { sub: `human-${role}`, name: role, role },
    } });
    if (url.pathname === "/api/locale") return json({ organization_locale: "en", user_locale: "en" });
    if (url.pathname === "/api/issues") return json({ issues: [] });
    if (url.pathname === "/api/projects") return json({ projects: [] });
    if (url.pathname === "/api/agents") return json({ daemon: "unreachable", agents: [], totals: null });
    if (url.pathname === "/api/health") return json({ ok: true, pm_present: false, projects: 0, issues: 0, daemon: "stub", embedded: true });
    if (url.pathname === "/api/update/banner") return json(null);
    if (url.pathname.startsWith("/api/threads/master")) return json({ thread: null, entries: [], more_before: false });
    if (url.pathname === "/api/app-installations") return json([]);
    if (url.pathname === "/api/assist/actions") return json({ actions: [] });
    if (url.pathname === "/api/human-threads/notifications") return json({ notifications: [] });
    if (url.pathname.startsWith("/api/settings")) return json({});
    return json({ error: "not found" }, 404);
  }) as typeof fetch;

  const host = document.createElement("div");
  document.body.append(host);
  const { setTimeout: delay } = require("timers/promises") as { setTimeout(ms: number): Promise<void> };
  const flush = () => React.act(async () => { await delay(0); });
  async function settle(check: () => boolean, message: string) {
    for (let n = 0; n < 60 && !check(); n++) await flush();
    assert(check(), message);
  }
  async function mount() {
    const root = createRoot(host);
    await React.act(async () => { root.render(React.createElement(LocaleProvider, null, React.createElement(App))); });
    return root;
  }

  for (const currentRole of ["member", "operator"] as const) {
    role = currentRole;
    for (const url of ["/agents", "/agents?sessions=list", direct]) {
      win.sessionStorage.clear();
      win.localStorage.clear();
      history.replaceState(null, "", url);
      const root = await mount();
      await settle(() => host.querySelector("h1")?.textContent?.includes("Agents") === true, `${currentRole} sees normal Agents at ${url}`);
      for (let n = 0; n < 5; n++) await flush();
      assert(diagnostics.length === 0, `${currentRole} must not request diagnostics at ${url}: ${diagnostics.join(", ")}`);
      assert(!host.textContent?.includes("Agent sessions"), `${currentRole} sees no sessions navigation at ${url}`);
      assert(!host.textContent?.includes("Session unavailable"), `${currentRole} sees no retired session view at ${url}`);
      assert(!host.textContent?.includes("Transcript"), `${currentRole} sees no transcript at ${url}`);
      await React.act(async () => { root.unmount(); });
    }
  }
}
void main().catch((error: unknown) => { console.error(error); process.exit(1); });
