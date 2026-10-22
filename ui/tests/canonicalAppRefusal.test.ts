import { streamFixture } from "./streamFixture";
export {};
/**
 * CAD-1438 acceptance check — independently authored (not the implementer).
 *
 * The retired `/app-installations/<id>` route must answer for itself:
 * it may replace the history entry with the SAME installation's
 * canonical `/apps/<key>` route — only when exactly one ACTIVE install
 * owns both the id and the key — and must refuse every other mapping
 * with the retired page instead of silently resolving another install:
 *
 *   1. unknown id            -> "App link retired", URL keeps the id
 *   2. removed id            -> retired, even when a replacement install
 *                              now owns the same package key
 *   3. duplicate-key id      -> retired, never one of the duplicates
 *   4. a good id             -> history-REPLACED onto /apps/<key> with
 *                              the deep-link query carried verbatim
 *   5. a signed-in NON-operator -> "App link unavailable" WITHOUT ever
 *                              requesting the protected
 *                              `/api/app-installations` list, and no app
 *                              data read is attempted for the id
 *   6. a canonical `/apps/<key>?screen=native&install=<removed-id>` —
 *      exactly what the real `connectReturnTo` emitter mints — must
 *      refuse when the pin names the REMOVED install and only the
 *      replacement owns the key: retired page, and no per-installation
 *      read runs for either the removed id or the replacement
 *   7. the same pin minted for a context/conversation link
 *      (`?install=<old>&ctx=…&conversation=…`) mismatched against the
 *      install the key resolves to now refuses the same way
 *
 * It mounts the real `App` component so the actual route guards
 * (`LegacyAppInstall` and `WorkspaceAppEntry`'s install-pin check in
 * src/App.tsx) and the real `/api/meta?operator=1` access proof run —
 * no re-implementation of the helper logic on this side.
 */
declare function require(name: string): any;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}

async function main() {
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage", "localStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  if (id === "react-markdown") return { __esModule: true, default: () => null };
  if (id === "remark-gfm") return { __esModule: true, default: () => undefined };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const App = (require("../src/App") as typeof import("../src/App")).default;
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
// The real emitter: the OAuth return URL a connect flow mints. Using the
// production helper (not a hand-spelled string) means the fixture breaks
// if the emitter's shape ever drifts from what the route refuses.
const { connectReturnTo } = require("../src/features/workspace-apps/socialConnect") as typeof import("../src/features/workspace-apps/socialConnect");

type Install = Record<string, unknown>;
const mk = (id: string, name: string, title: string, removed: number | null = null): Install => ({
  install_id: id, title, name, version: "0.1.0",
  summary: "", digest: `digest-${id}`, catalog_generation: "gen-1", approved: true,
  storage_kind: "workspace", project_link: null, executable: true,
  removed,
  approval: { state: "approved" }, guide: "", files: [], capabilities: null, connection_slots: [],
});

/** The operator's install list covers every refusal shape:
 *  - install-ok      unique active key `crm`            (good redirect)
 *  - install-old     REMOVED, key `social-content` — a live replacement
 *                    `install-new` owns the same key: the retired id
 *                    must not be forwarded onto the replacement
 *  - install-dup-a / install-dup-b share the active key `dupe-app`:
 *                    a key that cannot name one install is no canonical
 *                    target for either id */
const INSTALLS: Install[] = [
  mk("install-ok", "crm", "CRM"),
  mk("install-old", "social-content", "Social (old)", 1700000000),
  mk("install-new", "social-content", "Social Content"),
  mk("install-dup-a", "dupe-app", "Dupe A"),
  mk("install-dup-b", "dupe-app", "Dupe B"),
];

/** One active context on the good install so the deep-link's `ctx`
 *  selector is real, not stale. */
const CTX_A = {
  id: "ctx-a", install_id: "install-ok", revision: 1, digest: "ctx-digest",
  state: "active", config: { schema: 1, label: "Main", input_defaults: {} },
};

const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });

/** This session is a member, not the operator: the `?operator=1` proof
 *  answers false and no protected app read may ever be issued for it. */
let operatorSession = true;
let listCalls = 0;
const appReads: string[] = [];
const requested: string[] = [];

globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  const path = String(input);
  const url = new URL(path, "http://localhost");
  requested.push(url.pathname);
  if (url.pathname.endsWith("/stream")) return streamFixture(path);
  if (path.startsWith("/api/meta")) {
    const role = operatorSession ? "operator" : "member";
    return json({ read_only: false, operator: operatorSession, signed_in: true, actor: `${role} (ui)`,
      session: { id: `refusal-session-${role}`, origin: "public", created: 1700000000, last_used: 1700000000, idle_expires_at: 4102444800, expires_at: 4102444800, user_agent: "test", user: { sub: role, role } } });
  }
  if (path === "/api/issues" || path.startsWith("/api/issues?")) return json({ issues: [] });
  if (path === "/api/projects") return json({ projects: [] });
  if (path === "/api/agents") return json({ daemon: "unreachable", agents: [], totals: null });
  if (path === "/api/health") return json({ ok: true, pm_present: false, projects: 0, issues: 0, daemon: "stub", embedded: true });
  if (path === "/api/update/banner") return json(null);
  if (path.startsWith("/api/threads/master")) return json({ thread: null, entries: [], more_before: false });
  if (path === "/api/app-installations") { listCalls++; return json(INSTALLS); }
  if (path.startsWith("/api/app-installations/")) {
    const id = decodeURIComponent(url.pathname.slice("/api/app-installations/".length)).split("/")[0];
    const install = INSTALLS.find((i) => i.install_id === id);
    // Authorized shell summaries do not mount a workspace. Keep all reads
    // counted for members, removed ids, and every detail/context/action route.
    if (operatorSession && install?.removed === null && url.pathname === `/api/app-installations/${id}/conversations` && (init?.method ?? "GET") === "GET")
      return json({ alias: install.name, install_id: id, general: null, conversations: [] });
    appReads.push(id);
    if (install && url.pathname === `/api/app-installations/${id}`) return json(install);
    if (install && url.pathname === `/api/app-installations/${id}/contexts`) return json({ contexts: id === "install-ok" ? [CTX_A] : [] });
    if (url.pathname.endsWith("/bindings")) return json({ bindings: [] });
    if (url.pathname.endsWith("/effects")) return json({ effects: [] });
    if (install) return json({ error: "not stubbed" }, 404);
    return json({ error: "unknown installation" }, 404);
  }
  if (path === "/api/connections" || path.startsWith("/api/connections")) return json({ connections: [] });
  if (path.startsWith("/api/app-runs")) return json({ runs: [] });
  if (path === "/api/app-favorites") return json({ favorites: [] });
  if (url.pathname.endsWith("/records")) return json({ records: [], truncated: false, next_cursor: null });
  if (url.pathname.endsWith("/segments/list")) return json({ segments: [] });
  if (url.pathname.endsWith("/exclusions/list")) return json({ exclusions: [] });
  if (url.pathname.endsWith("/suppressions/list")) return json({ suppressions: [] });
  if (url.pathname.endsWith("/content/campaigns/list")) return json({ contents: [] });
  if (url.pathname.endsWith("/content/proposals/list")) return json({ proposals: [] });
  if (url.pathname.startsWith("/api/settings")) return json({});
  throw new Error(`Unexpected read ${path}`);
}) as typeof fetch;

const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
async function settle(check: () => void) {
  for (let i = 0; i < 60; i++) {
    await flush();
    try { check(); return; } catch { /* keep polling */ }
  }
  await flush();
  check();
}
const { navigate } = require("../src/lib/useLocation") as typeof import("../src/lib/useLocation");
const go = (path: string) => React.act(async () => { navigate(path); });
const workspaceMounted = () => host.querySelector("main.workspace-app") !== null;
const retired = () => host.textContent?.includes("App link retired") === true;

const mount = () =>
  React.act(async () => {
    root.render(React.createElement(LocaleProvider, null, React.createElement(App)));
  });

win.localStorage.clear();
win.sessionStorage.clear();
history.pushState(null, "", "/");
await mount();
await settle(() => assert(listCalls > 0, "operator session fetched the verified installation list"));

// (1) Unknown install id: retired page, the URL keeps the id (no silent
// redirect to anything), no app mounts, no per-installation read runs.
const readsBefore = appReads.length;
await go("/app-installations/install-ghost?ctx=ctx-x&conversation=conv-x");
await settle(() => assert(retired(), "unknown id answers the retired page"));
equal(location.pathname, "/app-installations/install-ghost", "unknown id: URL is not rewritten");
assert(!workspaceMounted(), "unknown id mounts no workspace app");
equal(appReads.length, readsBefore, "unknown id issues no per-installation read");

// (2) Removed install id: the replacement owns the same key now, but the
// retired route maps this id to nothing — never to install-new.
await go("/app-installations/install-old");
await settle(() => assert(retired(), "removed id answers the retired page"));
equal(location.pathname, "/app-installations/install-old", "removed id: no redirect onto the replacement's key");
assert(!workspaceMounted(), "removed id mounts no workspace app");
equal(appReads.filter((id) => id === "install-old" || id === "install-new").length, 0, "removed id touches neither the old nor the replacement install's data");

// (3) Duplicate-key install id: `dupe-app` names two active installs, so
// it is not a canonical target — the id refuses rather than pick one.
await go("/app-installations/install-dup-a?ctx=ctx-dup");
await settle(() => assert(retired(), "duplicate-key id answers the retired page"));
equal(location.pathname, "/app-installations/install-dup-a", "ambiguous id: URL is not rewritten");
assert(!workspaceMounted(), "duplicate-key id mounts no workspace app");
equal(appReads.filter((id) => id === "install-dup-a" || id === "install-dup-b").length, 0, "ambiguous id reads no duplicate's data");

// (4) The one mapping that may survive: a unique active install id whose
// key names exactly that install. The history entry is REPLACED with the
// canonical `/apps/<key>` and the deep-link query rides along verbatim.
await go("/app-installations/install-ok?ctx=ctx-a&conversation=conv-9&record=rec-1&keep=1");
await settle(() => equal(location.pathname, "/apps/crm", "unique active id redirects onto its canonical key route"));
const landed = new URLSearchParams(location.search);
equal(landed.get("ctx"), "ctx-a", "redirect preserved the context selector");
equal(landed.get("conversation"), "conv-9", "redirect preserved the conversation selector");
equal(landed.get("record"), "rec-1", "redirect preserved the record selector");
equal(landed.get("keep"), "1", "redirect preserved the unrelated query param");
equal(history.state, null, "redirect replaced the history entry (no push)");
await settle(() => assert(host.textContent?.includes("CRM"), "canonical route opens the same installation"));
assert(location.pathname !== "/app-installations/install-ok", "the legacy URL did not remain an entry point");

// (6) The connect-flow return URL the REAL emitter mints —
// `/apps/<key>?screen=native&install=<id>` — pinned to the REMOVED
// install while only the replacement owns the key. The route must
// refuse the mismatched pin: retired page, no mount, and no read of
// either install's data. (Generated through `connectReturnTo` so the
// check exercises the emitter+route contract, not a hand-written URL.)
const staleReturn = connectReturnTo("http://localhost", "social-content", "install-old");
assert(staleReturn === "http://localhost/apps/social-content?screen=native&install=install-old", "emitter minted the expected pinned return");
const readsBeforePin = appReads.length;
await go(staleReturn.slice("http://localhost".length));
await settle(() => assert(retired(), "canonical route with a removed-install pin answers the retired page"));
equal(location.pathname, "/apps/social-content", "pinned canonical route keeps its path (no rewrite to the retired id route)");
assert(!workspaceMounted(), "mismatched pin mounts no workspace app");
equal(
  appReads.slice(readsBeforePin).filter((id) => id === "install-old" || id === "install-new").length,
  0,
  "removed-pin refusal reads neither the removed install nor the replacement",
);

// (7) The same pin carried on a contextual deep link
// (`?install=<old>&ctx=…&conversation=…`): the pinned install's context
// cannot silently move to the replacement that owns the key now.
const contextualReturn =
  "/apps/social-content?install=install-old&ctx=ctx-old&conversation=conv-old&record=rec-old";
const readsBeforeCtx = appReads.length;
await go(contextualReturn);
await settle(() => assert(retired(), "contextual link pinned to the removed install refuses"));
assert(!workspaceMounted(), "mismatched contextual pin mounts no workspace app");
equal(
  appReads.slice(readsBeforeCtx).filter((id) => id === "install-old" || id === "install-new").length,
  0,
  "contextual refusal reads neither the removed install nor the replacement",
);

await React.act(async () => { root.unmount(); });

// (5) A signed-in NON-operator (the `?operator=1` proof answers false)
// lands on the same retired route. The route must NOT resolve the id,
// must NEVER request the protected `/api/app-installations` list (that
// read is operator-only), and no app may mount or be read for it. A fresh
// root is a fresh page load: the member credential is what the proof
// sees from the start — there is no stale operator answer to reuse.
operatorSession = false;
const listBefore = listCalls;
const readsBeforeMember = appReads.length;
const root2 = createRoot(host);
history.pushState(null, "", "/app-installations/install-ok?ctx=ctx-a&conversation=conv-9");
await React.act(async () => {
  root2.render(React.createElement(LocaleProvider, null, React.createElement(App)));
});
await settle(() => assert(
  host.textContent?.includes("App link unavailable") || host.textContent?.includes("Sign in as the operator"),
  "non-operator gets the unavailable page on the retired route",
));
// Settle once more past every pending effect before counting reads.
await flush();
await flush();
equal(location.pathname, "/app-installations/install-ok", "non-operator: the id is not resolved into a canonical URL");
assert(!workspaceMounted(), "non-operator mounts no workspace app");
equal(listCalls, listBefore, "non-operator never fetched the protected /api/app-installations list");
equal(appReads.length, readsBeforeMember, "non-operator ran no per-installation read");
assert(!host.textContent?.includes("Opening the app"), "non-operator is never told a redirect is coming");

await React.act(async () => { root2.unmount(); });
console.log("canonical app refusal checks passed");
}
void main();
