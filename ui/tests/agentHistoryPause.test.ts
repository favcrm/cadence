import type * as ReactTypes from "react";
import type { Root } from "react-dom/client";
import type * as RailModule from "../src/features/home/AgentUpdates";
import type * as LocaleModule from "../src/lib/locale";
import type * as ResourcesModule from "../src/lib/resources";
import type * as PrivateSessionModule from "../src/lib/privateSession";
import type * as ApiModule from "../src/lib/api";

declare function require<T>(name: string): T;

function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}

async function main(): Promise<void> {
  type Dom = Window & Pick<typeof globalThis, "Node" | "Element" | "HTMLElement" | "SVGElement" | "Event">;
  const { Window: BrowserWindow } = require<{ Window: new (options: { url: string }) => Dom }>("happy-dom");
  const win = new BrowserWindow({ url: "http://localhost/" });
  Object.defineProperties(globalThis, {
    window: { value: win, configurable: true }, document: { value: win.document, configurable: true },
    navigator: { value: win.navigator, configurable: true }, location: { value: win.location, configurable: true },
    history: { value: win.history, configurable: true }, sessionStorage: { value: win.sessionStorage, configurable: true },
    localStorage: { value: win.localStorage, configurable: true }, Node: { value: win.Node, configurable: true },
    Element: { value: win.Element, configurable: true }, HTMLElement: { value: win.HTMLElement, configurable: true },
    SVGElement: { value: win.SVGElement, configurable: true }, Event: { value: win.Event, configurable: true },
    addEventListener: { value: win.addEventListener.bind(win), configurable: true },
    removeEventListener: { value: win.removeEventListener.bind(win), configurable: true },
    IS_REACT_ACT_ENVIRONMENT: { value: true, configurable: true },
  });
  const loader = require<{ prototype: { require: (this: unknown, id: string) => unknown } }>("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.call(this, id);
  };
  const React = require<typeof ReactTypes>("react");
  const { createRoot } = require<{ createRoot: (host: Element) => Root }>("react-dom/client");
  const AgentUpdates = require<typeof RailModule>("../src/features/home/AgentUpdates").default;
  const { LocaleProvider } = require<typeof LocaleModule>("../src/lib/locale");
  const { resources } = require<typeof ResourcesModule>("../src/lib/resources");
  const { pausePrivateReads, allowPrivateReads, resetPrivateSession } = require<typeof PrivateSessionModule>("../src/lib/privateSession");
  const { api } = require<typeof ApiModule>("../src/lib/api");
  const originalAgent = api.agent;
  const originalThread = api.thread;
  const settlements: Promise<PromiseSettledResult<unknown>>[] = [];
  function observe<T>(pending: Promise<T>): Promise<T> {
    settlements.push(pending.then(
      (value) => ({ status: "fulfilled" as const, value }),
      (reason: unknown) => ({ status: "rejected" as const, reason }),
    ));
    return pending;
  }
  api.agent = (alias) => observe(originalAgent(alias));
  api.thread = (alias, at) => {
    const pending = originalThread(alias, at);
    return alias === "master" ? pending : observe(pending);
  };
  const { setImmediate: nextTurn } = require<{ setImmediate: (callback: () => void) => void }>("node:timers");
  const flush = () => React.act(async () => { await new Promise<void>(nextTurn); });
  async function settle(check: () => boolean, message: string) {
    for (let attempt = 0; attempt < 40; attempt += 1) {
      await flush();
      if (check()) return;
    }
    throw new Error(message);
  }
  const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
  const agents = ["alpha", "beta"].map((alias) => ({
    alias, provider: "synthetic", endpoint_kind: "test", state: "idle", group: "synthetic",
    unknown: 0, parked: 0, fenced: false, on: [], running: 0, queued: 0, last_activity: "2026-10-15T14:00:00Z",
  }));
  const held: { path: string; signal: AbortSignal | null | undefined; release: () => void }[] = [];
  const forbidden: string[] = [];
  let mode: "hold" | "retired" | "fresh" = "hold";
  globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(input instanceof Request ? input.url : String(input), "http://localhost");
    if (url.pathname === "/api/agents") return json({ agents, daemon: "reachable" });
    if (url.pathname === "/api/threads/master") return json({ thread: null, entries: [] });
    if (url.pathname.startsWith("/api/threads/") || url.pathname.startsWith("/api/agents/")) {
      const alias = url.pathname.split("/").at(-1);
      const body = url.pathname.startsWith("/api/threads/")
        ? { thread: null, entries: [{ seq: 1, kind: "assistant_text", role: "agent", text: `Current note ${alias}`, created: "2026-10-15T14:00:00Z" }] }
        : { events: [] };
      if (mode === "retired") {
        forbidden.push(url.pathname);
        return json({ error: "Retired history work restarted" }, 503);
      }
      if (mode === "fresh") return json(body);
      return new Promise<Response>((resolve) => {
        held.push({ path: url.pathname, signal: init?.signal, release: () => resolve(json(body)) });
      });
    }
    throw new Error(`Unexpected history acceptance request ${url.pathname}`);
  };
  const roots: Root[] = [];
  const host = document.createElement("div");
  document.body.append(host);
  try {
    resetPrivateSession();
    allowPrivateReads();
    await React.act(async () => { await resources.agents.revalidate(); });
    const root = createRoot(host);
    roots.push(root);
    await React.act(async () => {
      root.render(React.createElement(LocaleProvider, null, React.createElement(AgentUpdates, {
        onAsk: () => undefined, onOpenIssue: () => undefined,
      })));
    });
    await settle(() => host.querySelectorAll("h3").length === agents.length && held.length > 0, "the actual rail has mounted agents and started held history reads");
    const queuedPaths = agents.flatMap(({ alias }) => [`/api/threads/${alias}`, `/api/agents/${alias}`])
      .filter((path) => !held.some((request) => request.path === path));
    assert(queuedPaths.length > 0, "fixture has real history work waiting behind occupied slots");
    await React.act(async () => { pausePrivateReads(); root.unmount(); });
    assert(held.every((request) => request.signal?.aborted), "pause aborts the active real-client history transports");
    mode = "retired";
    await React.act(async () => {
      allowPrivateReads();
      for (const request of held) request.release();
    });
    // Await actual client completions, including any incorrectly awakened queue entries.
    let observed = -1;
    while (observed !== settlements.length) {
      observed = settlements.length;
      await React.act(async () => { await Promise.all(settlements.slice()); });
      await flush();
    }
    assert(!forbidden.some((path) => queuedPaths.includes(path)), "readmission cannot start abandoned pre-pause queued history reads");

    mode = "fresh";
    const fresh = createRoot(host);
    roots.push(fresh);
    await React.act(async () => {
      fresh.render(React.createElement(LocaleProvider, null, React.createElement(AgentUpdates, {
        onAsk: () => undefined, onOpenIssue: () => undefined,
      })));
    });
    await settle(() => agents.every(({ alias }) => host.textContent?.includes(`Current note ${alias}`)), "fresh admitted rail loads and displays each current agent's notes");
    assert(!host.textContent?.includes("Retired history work restarted"), "fresh consumers do not inherit a retired queue failure");
    console.log("agent history pause checks passed");
  } finally {
    await React.act(async () => { for (const root of roots) root.unmount(); resetPrivateSession(); });
    win.close();
    api.agent = originalAgent;
    api.thread = originalThread;
    loader.prototype.require = originalRequire;
  }
}

void main().catch((error: unknown) => {
  console.error(error);
  require<{ exitCode?: number }>("process").exitCode = 1;
});
