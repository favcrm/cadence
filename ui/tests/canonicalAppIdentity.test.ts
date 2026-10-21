import type * as HappyDom from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDOMClient from "react-dom/client";
import type * as AppModule from "../src/App";
import type * as LocaleModule from "../src/lib/locale";
import type * as LocationModule from "../src/lib/useLocation";
import type * as WorkspaceAppsModule from "../src/features/workspace-apps/workspaceApps";
import type { MasterState, Overview } from "../src/lib/types";
import type { SetupReport } from "../src/features/setup/checks";
/**
 * CAD-1438: following a legacy installation link must not transfer its work
 * to a replacement installation that happens to own the same app key.
 *
 * This mounts the real App, warms its real installation-list hook with A,
 * then holds every list refresh while the backend replaces A/crm with B/crm
 * and navigation follows /app-installations/A. Explicit response releases,
 * not timers, determine when the replacement becomes visible to the route.
 * The fetch log includes even aborted reads, so a transient mount of B cannot
 * hide behind the final retired page. No route/resolver module is mocked.
 */
declare function require(name: string): unknown;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}

async function main() {
  // CommonJS loading keeps browser-dependent module evaluation after setup.
  const { Window } = require("happy-dom") as typeof HappyDom;
  const win = new Window({ url: "http://localhost/" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage", "localStorage"] as const)
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener"] as const)
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  // Node provides the browser-compatible Web Crypto fixture.
  const { webcrypto } = require("crypto") as { webcrypto: Crypto };
  Object.defineProperty(globalThis, "crypto", { value: webcrypto, configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  // Match neighboring real-App checks: only browser presentation dependencies
  // are substituted. AppShell, both route guards and useInstallations run.
  // Node's CommonJS loader is replaced only at its presentation-module seam.
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
  const { navigate } = require("../src/lib/useLocation") as typeof LocationModule;
  const { notifyAppsChanged } = require("../src/features/workspace-apps/workspaceApps") as typeof WorkspaceAppsModule;

  const mk = (id: string, title: string) => ({
    install_id: id, title, name: "crm", version: "0.1.0", removed: null,
    summary: "", digest: `digest-${id}`, catalog_generation: "gen-1", approved: true,
    storage_kind: "workspace", project_link: null, executable: true,
    approval: { state: "approved" }, guide: "", files: [], capabilities: null, connection_slots: [],
  });
  const A = mk("install-a", "Original CRM");
  const B = mk("install-b", "Replacement CRM");
  const context = (id: string) => ({
    id: id === A.install_id ? "ctx-a" : "ctx-b", install_id: id,
    revision: 1, digest: `context-${id}`, state: "active",
    config: { schema: 1, label: "Main", input_defaults: {} },
  });
  const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), {
    status, headers: { "Content-Type": "application/json" },
  });
  let backendList = [A];
  let holdLists = false;
  let listCalls = 0;
  // CAD-1704: a loopback board never mounts the human-threads inbox.
  let humanThreadsCalls = 0;
  const pendingLists: Array<{ signal: AbortSignal | null; resolve: (response: Response) => void }> = [];
  const appReads: Array<{ id: string; path: string }> = [];
  const unexpected: string[] = [];

  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const path = String(input);
    const url = new URL(path, "http://localhost");
    if (url.pathname.endsWith("/stream")) return new Response(new ReadableStream(), { headers: { "Content-Type": "text/event-stream" } });
    if (url.pathname === "/api/meta") return json({ read_only: false, operator: true, signed_in: true, actor: "operator (ui)", session: { id: "canonical-session", origin: "loopback", created: 1700000000, last_used: 1700000000, idle_expires_at: 4102444800, expires_at: 4102444800, user_agent: "test" } });
    if (url.pathname === "/api/human-threads/notifications") {
      humanThreadsCalls++;
      return json({
        error: "sign in to use human threads", code: "signed_out", revision: null,
      }, 401);
    }
    if (url.pathname === "/api/issues") return json({ issues: [] });
    if (path === "/api/projects") return json({ projects: [] });
    if (path === "/api/agents") return json({ daemon: "unreachable", agents: [], totals: null });
    if (path === "/api/health") return json({ ok: true, pm_present: false, projects: 0, issues: 0, daemon: "stub", embedded: true });
    if (path === "/api/update/banner") return json(null);
    if (path.startsWith("/api/threads/master")) return json({ thread: null, entries: [], more_before: false });
    if (url.pathname === "/api/master/state") return json({
      alias: "master", provider: "fixture", endpoint_kind: "fixture",
      live: false, turn: null, queued: 0,
    } satisfies MasterState);
    if (url.pathname === "/api/overview") return json({
      needs_me: [], drift: { matched: true, known: true, count: 0, commits: [] },
      projects: [], github: { state: "unavailable" }, main_ci: [],
      daemon: { reachable: false }, generated_at: 1700000000,
    } satisfies Overview);
    if (url.pathname === "/api/setup") return json({
      checks: ["state_dir", "tracker", "daemon", "master", "master_login"].map((check) => ({
        check, status: "ok" as const, detail: "Ready in fixture", fix: null,
        group: check.startsWith("master") ? "master" as const : "environment" as const,
      })),
      master: { providers: [] }, checked_at: 1700000000000,
      detect_only: true, ran_now: false, age_ms: 0, recheck_in_ms: 60000,
    } satisfies SetupReport);
    if (path === "/api/app-installations") {
      listCalls++;
      if (!holdLists) return json(backendList);
      // Deliberately allow late replies to aborted requests, too. The real
      // hook must ignore those; the test releases every response explicitly.
      return new Promise<Response>((resolve) => pendingLists.push({ signal: init?.signal ?? null, resolve }));
    }
    if (url.pathname.startsWith("/api/app-installations/")) {
      const id = decodeURIComponent(url.pathname.slice("/api/app-installations/".length).split("/")[0]);
      appReads.push({ id, path: url.pathname });
      const install = id === A.install_id ? A : id === B.install_id ? B : null;
      if (install && url.pathname === `/api/app-installations/${id}`) return json(install);
      if (install && url.pathname === `/api/app-installations/${id}/contexts`) return json({ contexts: [context(id)] });
      if (install && url.pathname.endsWith("/conversations")) return json({ alias: "crm", install_id: id, general: null, conversations: [] });
      if (url.pathname.endsWith("/bindings")) return json({ bindings: [] });
      if (url.pathname.endsWith("/effects")) return json({ effects: [] });
      return json({ error: "not found" }, 404);
    }
    if (url.pathname.startsWith("/api/connections")) return json({ connections: [] });
    if (url.pathname.startsWith("/api/app-runs")) return json({ runs: [] });
    // The screen's shared extras reader also reads this installation's retained
    // tool receipts (CAD-1177); a generic screen mount now shares that reader.
    if (url.pathname.startsWith("/api/app-screen-tools/results")) return json({ results: [] });
    if (path === "/api/app-favorites") return json({ favorites: [] });
    if (url.pathname.endsWith("/records")) return json({ records: [], truncated: false, next_cursor: null });
    if (url.pathname.endsWith("/segments/list")) return json({ segments: [] });
    if (url.pathname.endsWith("/exclusions/list")) return json({ exclusions: [] });
    if (url.pathname.endsWith("/suppressions/list")) return json({ suppressions: [] });
    if (url.pathname.endsWith("/content/campaigns/list")) return json({ contents: [] });
    if (url.pathname.endsWith("/content/proposals/list")) return json({ proposals: [] });
    if (url.pathname.startsWith("/api/settings")) return json({});
    unexpected.push(path);
    return json({ error: `Unexpected request ${path}` }, 500);
  }) as typeof fetch;

  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  // act drains React effects and their promise continuations. There are no
  // sleep/poll timers and held fetches cannot resolve during these drains.
  const drain = () => React.act(async () => { await Promise.resolve(); });
  async function settle(check: () => void) {
    for (let turn = 0; turn < 40; turn++) {
      await drain();
      try { check(); return; } catch { /* drain another microtask/effect turn */ }
    }
    check();
  }
  const go = (path: string) => React.act(async () => { navigate(path); });
  const refusal = () => host.textContent?.includes("App link retired") === true
    || host.textContent?.includes("App link unavailable") === true;
  const workspaceMounted = () => host.querySelector("main.workspace-app") !== null;
  const noReplacementReads = (phase: string) => equal(
    appReads.filter((read) => read.id === B.install_id), [],
    `${phase}: following A must never request replacement B, even transiently or with an aborted signal`,
  );

  try {
    localStorage.clear();
    sessionStorage.clear();
    await React.act(async () => {
      root.render(React.createElement(LocaleProvider, null, React.createElement(App)));
    });
    await settle(() => {
      assert(listCalls > 0, "real App requested the operator installation list");
      assert(host.textContent?.includes("Original CRM"), "App cached the verified A/crm installation before replacement");
    });
    equal(humanThreadsCalls, 0, "a loopback board never mounts the human-threads inbox, so it never polls the route it does not serve");
    equal(appReads, [], "warming the board did not mount either installation");

    // A is now gone server-side. Keep its previous successful list in the
    // real hook while an apps-changed refresh and legacy navigation overlap.
    backendList = [B];
    holdLists = true;
    const callsBeforeRefresh = listCalls;
    await React.act(async () => { notifyAppsChanged(); });
    await settle(() => assert(listCalls > callsBeforeRefresh, "apps-changed started a deliberately held refresh"));
    await go("/app-installations/install-a?ctx=ctx-a&conversation=conv-a&record=rec-a&keep=1#work");
    await drain();
    assert(pendingLists.some((request) => request.signal?.aborted !== true), "navigation has a live held installation-list response");
    noReplacementReads("while the replacement list is held");

    // Release *after* navigation has processed its effects. New requests
    // caused by a redirect also see B, not another stale A fixture.
    await React.act(async () => {
      holdLists = false;
      for (const request of pendingLists.splice(0)) request.resolve(json(backendList));
    });
    await drain();
    noReplacementReads("after the replacement becomes known");
    await settle(() => {
      noReplacementReads("while the real route finishes refusal");
      assert(refusal(), "the stale A link visibly retires or becomes unavailable once only B owns crm");
      assert(!workspaceMounted(), "replacement B never remains mounted behind the refusal");
    });
    equal(new URLSearchParams(location.search).get("ctx"), "ctx-a", "refusal does not transfer or discard A's context selector");
    equal(new URLSearchParams(location.search).get("conversation"), "conv-a", "refusal retains A's conversation selector");
    equal(new URLSearchParams(location.search).get("record"), "rec-a", "refusal retains A's record selector");

    // Positive identity path: with A active again, the real legacy guard
    // must carry A's immutable identity into the real canonical pin guard.
    await go("/");
    backendList = [A];
    const callsBeforeRestore = listCalls;
    await React.act(async () => { notifyAppsChanged(); });
    await settle(() => {
      assert(listCalls > callsBeforeRestore, "restoring A refreshed the list");
      assert(host.textContent?.includes("Original CRM"), "restored A is visible in the verified app menu");
    });
    await go("/app-installations/install-a?ctx=ctx-a&conversation=conv-a&record=rec-a&keep=1#work");
    await settle(() => equal(location.pathname, "/apps/crm", "active A uses the canonical route"));
    const canonical = new URLSearchParams(location.search);
    equal(canonical.get("install"), "install-a", "canonical redirect remains pinned to legacy installation A");
    equal(canonical.get("ctx"), "ctx-a", "canonical redirect preserves the context");
    equal(canonical.get("conversation"), "conv-a", "canonical redirect preserves the conversation");
    equal(canonical.get("record"), "rec-a", "canonical redirect preserves the record");
    equal(canonical.get("keep"), "1", "canonical redirect preserves unrelated query parameters");
    equal(location.hash, "#work", "canonical redirect preserves the fragment");
    await settle(() => assert(appReads.some((read) => read.id === A.install_id), "real AppShell read the same installation A"));
    noReplacementReads("after the successful same-installation redirect");

    // The path names A; an existing conflicting query pin must not win,
    // and must not be silently overwritten to make the link open A either.
    await go("/");
    const readsBeforeConflict = appReads.length;
    await go("/app-installations/install-a?install=install-b&ctx=ctx-a&conversation=conv-a");
    await settle(() => assert(refusal(), "legacy A with a conflicting B pin visibly refuses"));
    await drain();
    assert(!workspaceMounted(), "conflicting legacy pin mounts neither installation");
    equal(appReads.slice(readsBeforeConflict), [], "conflicting legacy pin reads neither A nor B");
    noReplacementReads("throughout every identity check");
    equal(unexpected, [], "all real-App requests use the declared HTTP fixture");
    console.log("canonical app identity checks passed");
  } finally {
    await React.act(async () => { root.unmount(); });
  }
}
void main();
