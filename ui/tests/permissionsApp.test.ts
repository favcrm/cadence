export {};
/** The actual App must prove the current session before mounting protected rules. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/settings/permissions" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "SVGElement", "navigator", "MutationObserver", "Event", "MouseEvent", "location", "history", "sessionStorage", "localStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
// Keep the real app's poll and stream inert; refreshes below use its header control.
Object.defineProperty(globalThis, "setInterval", { value: () => 1, configurable: true });
Object.defineProperty(globalThis, "clearInterval", { value: () => undefined, configurable: true });
Object.defineProperty(globalThis, "EventSource", { value: class {
  readyState = 0;
  addEventListener() {}
  close() {}
}, configurable: true });
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
const { setSessionKey } = require("../src/lib/sessionKey") as typeof import("../src/lib/sessionKey");
const json = (data: unknown, status = 200) => new Response(JSON.stringify(data), { status, headers: { "Content-Type": "application/json" } });
const identity = (id: string, operator: boolean | null) => ({
  signed_in: true, read_only: false, operator, actor: id,
  session: { id, user: { name: id, email: `${id}@example.test`, role: id === "a" ? "operator" : "member" } },
});
let current = "a", permissionReads = 0, holdMeta = false;
const metaRequests: { proof: boolean; id: string }[] = [];
const pending: { resolve: (response: Response) => void; proof: boolean; id: string }[] = [];
globalThis.fetch = async (input) => {
  const url = new URL(String(input), "http://localhost");
  if (url.pathname === "/api/meta") {
    const proof = url.searchParams.get("operator") === "1", id = current;
    metaRequests.push({ proof, id });
    if (holdMeta) return new Promise<Response>((resolve) => pending.push({ resolve, proof, id }));
    return json(identity(id, proof ? id === "a" : null));
  }
  if (url.pathname === "/api/master/permissions") {
    permissionReads++;
    return current === "a" ? json({ rules: [{ id: "a-rule", effect: "allow", scope: "exact", argv: ["git", "status"], tail: [], cwd: "/private/session-a", by: "operator", at: 1700000000 }], requests: [] }) : json({ error: "Operator required" }, 403);
  }
  if (url.pathname === "/api/health") return json({ daemon: "reachable" });
  if (url.pathname === "/api/issues") return json({ issues: [] });
  if (url.pathname === "/api/projects") return json({ projects: [] });
  if (url.pathname === "/api/agents") return json({ agents: [], by_issue: {}, daemon: "reachable" });
  if (url.pathname === "/api/update/banner") return json(null);
  throw new Error(`Unexpected app read ${url.pathname}`);
};
const assert: (value: unknown, why: string) => asserts value = (value, why) => { if (!value) throw new Error(why); };
const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const flush = async () => { for (let i = 0; i < 3; i++) await React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); }); };
const text = () => host.textContent ?? "";
const refresh = async () => {
  const control = host.querySelector('[aria-label="Refresh board"]');
  assert(control, "Actual app refresh control exists");
  await React.act(async () => control.dispatchEvent(new MouseEvent("click", { bubbles: true })));
  await flush();
};
const resolve = async (index: number, id: string, operator: boolean | null) => {
  await React.act(async () => pending[index].resolve(json(identity(id, operator))));
  await flush();
};
async function run() {
  setSessionKey("key-a");
  await React.act(async () => root.render(React.createElement(App)));
  await flush();
  assert(permissionReads === 1 && text().includes("/private/session-a"), "Proven session A reads its rules");
  await refresh();
  assert(metaRequests.at(-1)?.proof === false, "Ordinary polls reuse the same session's proof");

  // Keep the credential unchanged to isolate metadata session identity, as when a
  // server reports a new session while both responses still say signed_in=true.
  current = "b";
  await refresh();
  assert(Number(permissionReads) === 1, "Session B must never inherit A's operator proof and request protected rules");
  assert(metaRequests.at(-1)?.proof === true, "Same-signed-in identity change re-proves operator capability");
  assert(!text().includes("/private/session-a"), "Member session hides A's saved rules");

  current = "a";
  await refresh();
  assert(Number(permissionReads) === 2 && text().includes("/private/session-a"), "Returning operator session is re-proven and reads again");

  // A real second sign-in replaces the tab key. Hide A's data immediately,
  // before the new session's slow metadata response can arrive.
  current = "b";
  setSessionKey("key-b");
  holdMeta = true;
  await refresh();
  const switched = pending.length - 1;
  assert(pending[switched].proof, "New credential always requests operator proof");
  assert(!text().includes("/private/session-a"), "Credential change clears protected data while metadata is pending");
  await resolve(switched, "b", false);
  assert(Number(permissionReads) === 2, "Proven member session makes no protected request");

  // Two refreshes may overlap under one credential. A late old operator
  // response must not replace the newer member response or mount protected UI.
  await refresh();
  const old = pending.length - 1;
  await refresh();
  const latest = pending.length - 1;
  await resolve(latest, "b", null);
  await resolve(old, "a", true);
  assert(Number(permissionReads) === 2 && !text().includes("/private/session-a"), "Late metadata cannot overwrite the newest session/proof");

  // Nor may an in-flight proof from B survive a subsequent credential switch.
  await refresh();
  const formerKey = pending.length - 1;
  current = "a";
  setSessionKey("key-a-2");
  await refresh();
  const newKey = pending.length - 1;
  await resolve(newKey, "a", true);
  await resolve(formerKey, "b", false);
  assert(Number(permissionReads) === 3 && text().includes("/private/session-a"), "Former credential response cannot revoke the current operator view");
  await React.act(async () => root.unmount());
  await win.happyDOM.close();
  console.log("permissionsApp: current-session proof, immediate access loss and metadata races passed");
}
run().catch(async (error) => {
  await React.act(async () => root.unmount());
  await win.happyDOM.close();
  console.error(error);
  throw error;
});
