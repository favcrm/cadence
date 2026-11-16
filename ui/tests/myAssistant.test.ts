/** Exercise the Agents screen, scoped views and detail read failures with real API shapes. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/agents" });
for (const name of [
  "window",
  "document",
  "Node",
  "Element",
  "HTMLElement",
  "HTMLInputElement",
  "HTMLSelectElement",
  "SVGElement",
  "navigator",
  "MutationObserver",
  "Event",
  "MouseEvent",
  "KeyboardEvent",
  "location",
  "history",
]) {
  Object.defineProperty(globalThis, name, {
    value: name === "window" ? win : win[name],
    configurable: true,
    writable: true,
  });
}
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, {
    value: win[name].bind(win),
    configurable: true,
  });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module");
const originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons")
    return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } =
  require("react-dom/client") as typeof import("react-dom/client");
const Agents = (
  require("../src/features/agents/Agents") as typeof import("../src/features/agents/Agents")
).default;
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const { navigate } = require("../src/lib/useLocation") as typeof import("../src/lib/useLocation");
const { readLocation } = require("../src/lib/router") as typeof import("../src/lib/router");
type Agent = import("../src/lib/types").Agent;
type AgentsPayload = import("../src/lib/types").AgentsPayload;
type AgentTab = import("../src/features/agents/agentTabModel").AgentTab;
const base: Agent = {
  alias: "dev",
  provider: "pi",
  endpoint_kind: "pty",
  role: "Developer",
  state: "idle",
  group: "pm",
  group_root: false,
  tasks: [],
  running: 0,
  queued: 0,
  unknown: 0,
  parked: 0,
  fenced: false,
  on: [],
};
const fresh = { by_issue: {}, refresh_error: false, snapshot_age_secs: 0, stale: false, daemon: "reachable", totals: null } as const;
const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
const personalRow = { scope: "personal", opens: "home", name: "Nova", look: "robot", bootstrap_state: "complete", conversation_id: "c1" } as const;
const crm: Agent = { ...base, alias: "crm-action-review", running: 1, state: "busy" };
const roster = (over: Partial<AgentsPayload> & { master?: Partial<Agent> } = {}): AgentsPayload => {
  const { master, ...rest } = over;
  return { ...fresh, profiles: [], agents: [{ ...base, alias: "master", ...master }, crm], ...rest } as AgentsPayload;
};
const hostedRoster = (extra: Partial<AgentsPayload> = {}) =>
  roster({ master: { scope: "workspace", name: "Acme" }, personal_assistant: personalRow, ...extra });
let memberStateReads = 0;
let threadReads: string[] = [];
globalThis.fetch = async (input) => {
  const path = new URL(String(input), "http://localhost").pathname;
  if (path === "/api/member-assistant/state") {
    memberStateReads += 1;
    return json({ assistant: { alias: "assistant-1", identity: { name: "Nova", tone: null, look: "robot", revision: 1 } }, conversation_id: "c1" });
  }
  if (path.startsWith("/api/threads/")) {
    threadReads.push(path);
    return json({ entries: [], cursor: 0, more_before: false });
  }
  if (path.startsWith("/api/agents/")) {
    const alias = decodeURIComponent(path.split("/").pop()!);
    if (alias === "assistant") throw new Error("the personal assistant must never read /api/agents/assistant");
    return json({ agent: { alias, provider: "pi", endpoint_kind: "pty", instructions: "x" }, queued: 0, unknown: 0, running: [], events: [], fenced: false, on: [] });
  }
  return json({ error: "unavailable" }, 503);
};
function assert(value: unknown, what: string): asserts value {
  if (!value) throw new Error(what);
}
const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const loaded = <T>(data: T): import("../src/lib/cache").ResourceState<T> => ({
  data, status: "ok", error: null, asOf: Date.now(), inFlight: false,
});
const flush = async () => {
  await React.act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
};
let payload: AgentsPayload = roster();
let hostedBoard = true;
let operator: boolean | null = true;
let profiles: unknown[] = [];
/** Reads the route from the real URL, as the app does. */
function route(): { open: string | null; tab: AgentTab } {
  const r = readLocation(location.pathname, location.search).route;
  return r.screen === "agents" ? { open: r.alias, tab: r.tab ?? "overview" } : { open: null, tab: "overview" };
}
async function render() {
  const { open, tab } = route();
  await React.act(() =>
    root.render(
      React.createElement(LocaleProvider, null, React.createElement(Agents, {
        key: `${operator}${hostedBoard}`,
        operator,
        onRetryAccess: () => {},
        hostedBoard,
        state: loaded({ ...payload, profiles } as AgentsPayload),
        issues: loaded([]),
        open,
        tab,
        recruit: false,
        ask: false,
        onAsk: () => {},
        // The app's own handler: a navigation, read back from the URL.
        onOpenAgent: (alias, to) => {
          navigate(alias ? `/agents/${encodeURIComponent(alias)}${to && to !== "overview" ? `?tab=${to}` : ""}` : "/agents");
        },
        onTab: () => {},
        onToast: () => {},
        onRecruit: () => {},
        onOpenIssue: () => {},
        onRetry: () => {},
      })),
    ),
  );
  await flush();
}
async function at(path: string) {
  navigate(path);
  await render();
}
const seats = () => Array.from(host.querySelectorAll(".rc-seats .rc-seat")).map((b) => b.textContent ?? "");
const sideRows = () => Array.from(host.querySelectorAll(".ag-side-row")).map((b) => b.textContent ?? "");
async function click(el: Element | null | undefined) {
  assert(el, "the element to click exists");
  await React.act(() => (el as HTMLElement).click());
  await render();
}
async function main() {
  // A member on a hosted board: My assistant first, no Master row in the cards or the sidebar.
  payload = hostedRoster();
  operator = false;
  await at("/agents");
  assert(seats()[0].includes("Nova · Your assistant"), "member: My assistant is the first card");
  assert(!seats().some((s) => s.includes("master") || s.includes("Acme")), "member: no Master card");
  assert(sideRows()[1].includes("Nova · Your assistant"), "member: My assistant is the first sidebar row after Overview");
  assert(!sideRows().some((s) => s.includes("Acme") || s.includes("master")), "member: no Master sidebar row");
  assert(host.querySelector(".rc-seats .rc-seat svg, .rc-seats .rc-seat [class*=avatar]") !== null, "the card wears a face");

  // An owner: My assistant first, then Master labelled company and owners-only (workspace name as the title).
  operator = true;
  await render();
  assert(seats()[0].includes("Nova · Your assistant"), "owner: My assistant first");
  assert(seats()[1].includes("Acme") && seats()[1].includes("Master · company, owners only"), "owner: Master follows, labelled");
  assert(sideRows().some((s) => s.includes("Master · company, owners only")), "owner: the sidebar row is labelled too");

  // Clicking Master (card, then sidebar row) navigates to the Master page, not a chat.
  await at("/agents");
  await click(host.querySelector(".rc-seats [aria-label='Open agent master']"));
  assert(location.pathname === "/agents/master" && location.search === "?tab=instructions", "the Master card navigates to its page");
  assert(host.querySelector(".ap-hero h1")?.textContent === "master", "the Master page, not a chat, opens");
  assert(host.querySelector(".ag-chat") === null, "no chat opens for the company Master");
  await at("/agents");
  const row = Array.from(host.querySelectorAll<HTMLElement>(".ag-side-row")).find((b) => b.textContent?.includes("Master · company"));
  await click(row);
  assert(location.pathname === "/agents/master", "the Master sidebar row navigates");
  // A hired role's card still opens its chat.
  await at("/agents");
  await click(host.querySelector(".rc-seats [aria-label='Open agent crm-action-review']"));
  assert(String(location.pathname) === "/agents/crm-action-review" && host.querySelector(".ag-chat") !== null, "another agent still opens its chat");

  // A member who types /agents/master reads the owners-only gate; the assistant is on Home.
  operator = false;
  await at("/agents/master");
  assert(
    host.textContent?.includes("Chat with the company Master is for owners. Your assistant is on Home."),
    "member: the gate says who Master is for",
  );
  assert(host.querySelector("textarea") === null, "member: no composer for the company Master");

  // /agents/assistant is the personal page, even beside a hired profile named assistant.
  profiles = [{ id: "ap_1", alias: "assistant", role: "worker", team_role: "worker", catalogue: null, look: null, job: "x", model: "inherit", runtimes: [] }];
  operator = true;
  await at("/agents/assistant");
  assert(host.querySelector(".ap-hero h1")?.textContent === "My assistant", "the personal page wins over a hired `assistant`");
  assert(host.textContent?.includes("Nova · Your assistant"), "the header names the assistant");
  assert(memberStateReads > 0, "the header reads /api/member-assistant/state");
  assert(host.textContent?.includes("not edited here yet"), "Instructions is the honest read-only note");
  assert(!host.textContent?.includes("Manage profile"), "no fake controls");
  assert(host.querySelectorAll("[aria-current='page']").length === 1, "one sidebar row is current");
  assert(host.querySelector("[aria-current='page']")?.textContent?.includes("Your assistant"), "the personal row is the current one");
  // Without a personal row (an older API) the same route is the hired profile, as today.
  payload = roster();
  await render();
  assert(host.querySelector("h1")?.textContent === "assistant", "an older API: /agents/assistant is the hired profile");
  profiles = [];

  // An older API (no scope, no personal_assistant) renders as today, Master shown to everyone.
  payload = roster();
  for (const who of [false, true]) {
    operator = who;
    await at("/agents");
    assert(seats().length === 2 && seats()[0].startsWith("master"), "old roster: Master leads, as today");
    assert(!seats().some((s) => s.includes("Your assistant")), "old roster: no personal card");
    assert(!seats()[0].includes("owners only"), "old roster: Master keeps its old label");
    assert(!sideRows().some((s) => s.includes("Your assistant") || s.includes("owners only")), "old roster: sidebar as today");
  }
  // A native board never hides or relabels Master.
  hostedBoard = false;
  operator = false;
  await at("/agents");
  assert(seats()[0].startsWith("master"), "native: Master is unchanged");
  await React.act(() => root.unmount());
  console.log("my assistant checks passed");
}
main().catch(async (error) => {
  await React.act(() => root.unmount());
  throw error;
});
export {};
