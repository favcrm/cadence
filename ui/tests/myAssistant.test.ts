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
const { locationHref, readLocation } = require("../src/lib/router") as typeof import("../src/lib/router");
const { allowPrivateReads, resetPrivateSession } = require("../src/lib/privateSession") as typeof import("../src/lib/privateSession");
const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof import("../screen-ui/src/navigation");
const { hostNavigation } = require("../src/ui/hostNavigation") as typeof import("../src/ui/hostNavigation");
type Agent = import("../src/lib/types").Agent;
type AgentsPayload = import("../src/lib/types").AgentsPayload;
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
let aliasReads = 0;
let masterReads = 0;
let threadReads: string[] = [];
let directoryReads = 0;
let directoryAvailable = true;
globalThis.fetch = async (input) => {
  const path = new URL(String(input), "http://localhost").pathname;
  if (path === "/api/team/members") {
    directoryReads += 1;
    if (!directoryAvailable) return json({ error: "unavailable" }, 503);
    return json({ members: [
      { userId: "viewer", name: "Lee Chen", role: "member", joinedAt: "2026-11-16" },
      { userId: "teammate", name: "Maya Park", email: "maya@example.test", role: "owner", joinedAt: "2026-11-16" },
      { userId: "unnamed", name: "hidden@example.test", email: "hidden@example.test", role: "member", joinedAt: "2026-11-16" },
    ] });
  }
  if (path === "/api/member-assistant/state") {
    memberStateReads += 1;
    return json({ assistant: { alias: "assistant-1", identity: { name: "Nova", tone: null, look: "robot", revision: 1 } }, conversation_id: "c1" });
  }
  // CAD-2354: an API that predates the instructions routes.
  if (path === "/api/member-assistant/instructions" || path === "/api/master/instructions") return json({ error: "not found" }, 404);
  if (path.startsWith("/api/threads/")) {
    threadReads.push(path);
    return json({ entries: [], cursor: 0, more_before: false });
  }
  if (path.startsWith("/api/agents/")) {
    const alias = decodeURIComponent(path.split("/").pop()!);
    if (alias === "assistant") aliasReads += 1;
    if (alias === "master") masterReads += 1;
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
let viewerSub: string | null = "viewer";
let profiles: unknown[] = [];
let rosterFailed = false;
let rosterLoading = false;
/** Reads the route from the real URL, as the app does. */
function route() {
  const r = readLocation(location.pathname, location.search).route;
  return r.screen === "agents" ? r : { alias: null, profileId: undefined, tab: undefined };
}
async function render() {
  const { alias: open, profileId: openProfileId, tab = "overview" } = route();
  const viewer = { viewerSub: hostedBoard ? viewerSub : null };
  await React.act(() =>
    root.render(
      React.createElement(NavigationProvider, { adapter: hostNavigation, children:
      React.createElement(LocaleProvider, null, React.createElement(Agents, {
        key: `${operator}${hostedBoard}`,
        operator,
        onRetryAccess: () => {},
        hostedBoard,
        ...viewer,
        openProfileId,
        state: rosterFailed
          ? { ...loaded<AgentsPayload | null>(null), status: "failed", error: "Offline" }
          : rosterLoading
            ? { ...loaded<AgentsPayload | null>(null), status: "loading", inFlight: true }
            : loaded({ ...payload, profiles } as AgentsPayload),
        issues: loaded([]),
        open,
        tab,
        recruit: false,
        ask: false,
        onAsk: () => {},
        // The app's own handler: a navigation, read back from the URL.
        onOpenAgent: (alias, to, profileId) => {
          const loc = readLocation(location.pathname, location.search);
          navigate(locationHref({ ...loc, route: { screen: "agents", alias, profileId, tab: to === "overview" ? undefined : to } }));
        },
        onTab: () => {},
        onToast: () => {},
        onRecruit: () => {},
        onOpenIssue: () => {},
        onRetry: () => {},
      })),
      }),
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
  allowPrivateReads();
  const checkDirectory = () => {
    const cards = [...host.querySelectorAll(".rc-seats .rc-seat")];
    const maya = cards.find((card) => card.textContent?.includes("Maya Park"));
    assert(maya, "a teammate has a private assistant card");
    assert(maya.textContent?.includes("Personal assistant") && maya.textContent.includes("Private"), "the card describes privacy, not status");
    assert(!maya.matches("a, button, [tabindex], [role=button], [role=link]") && !maya.querySelector("a, button, [tabindex]"), "teammate card has no way to open it");
    assert(maya.querySelector(".agent-avatar") !== null, "teammate card wears an assistant avatar");
    assert(!sideRows().some((s) => s.includes("Maya Park")), "teammate is not a sidebar destination");
    assert(!host.textContent?.includes("Lee Chen") && !host.textContent?.includes("@example.test"), "viewer and email fallback are not rendered");
    assert(cards.some((card) => card.textContent?.includes("Teammate") && card.textContent.includes("Private")), "a missing public name still has a generic private card");
  };
  // A member on a hosted board: My assistant first, no Master row in the cards or the sidebar.
  payload = hostedRoster();
  operator = false;
  await at("/agents");
  assert(seats()[0].includes("Nova · Your assistant"), "member: My assistant is the first card");
  assert(!seats().some((s) => s.includes("master") || s.includes("Acme")), "member: no Master card");
  assert(sideRows()[1].includes("Nova · Your assistant"), "member: My assistant is the first sidebar row after Overview");
  assert(!sideRows().some((s) => s.includes("Acme") || s.includes("master")), "member: no Master sidebar row");
  assert(host.querySelector(".rc-seats .rc-seat svg, .rc-seats .rc-seat [class*=avatar]") !== null, "the card wears a face");
  checkDirectory();
  const search = host.querySelector<HTMLInputElement>("#agents-search");
  assert(search, "Agents offers search");
  await React.act(() => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(search, "PARK   MAYA");
    search.dispatchEvent(new Event("input", { bubbles: true }));
  });
  assert(seats().length === 1 && seats()[0].includes("Maya Park"), "Agents search matches teammate names without an empty-state message");
  assert(!host.textContent?.includes("No agents match"), "teammate match is a roster result");
  await React.act(() => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(search, "assistant   NOVA");
    search.dispatchEvent(new Event("input", { bubbles: true }));
  });
  assert(seats().length === 1 && seats()[0].includes("Nova · Your assistant"), "personal name and alias match in either order");

  // An owner: My assistant first, then Master labelled company and owners-only (workspace name as the title).
  operator = true;
  await render();
  assert(seats()[0].includes("Nova · Your assistant"), "owner: My assistant first");
  assert(seats()[1].includes("Acme") && seats()[1].includes("Master · company, owners only"), "owner: Master follows, labelled");
  assert(sideRows().some((s) => s.includes("Master · company, owners only")), "owner: the sidebar row is labelled too");
  checkDirectory();
  const ownerSearch = host.querySelector<HTMLInputElement>("#agents-search");
  assert(ownerSearch, "owner: Agents offers search");
  await React.act(() => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(ownerSearch, "ACME master");
    ownerSearch.dispatchEvent(new Event("input", { bubbles: true }));
  });
  assert(seats().length === 1 && seats()[0].includes("Acme"), "owner: search matches the displayed Master name and alias");
  assert(sideRows().slice(1).length === 1 && sideRows()[1].includes("Acme"), "owner: search finds Master in the sidebar by display name too");
  await React.act(() => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(ownerSearch, "");
    ownerSearch.dispatchEvent(new Event("input", { bubbles: true }));
  });
  const priorReads = directoryReads;
  viewerSub = null;
  await render();
  assert(!host.textContent?.includes("Maya Park") && directoryReads === priorReads, "an unidentified viewer does not read or render the directory");
  directoryAvailable = false;
  viewerSub = "viewer";
  await render();
  assert(!host.textContent?.includes("Maya Park"), "a failed read does not display earlier directory data");
  directoryAvailable = true;

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
  await at("/agents/crm-action-review");
  assert(host.textContent?.includes("Conversations with agents here are for the workspace operator."), "member: a hired agent receives the general chat refusal");
  assert(!host.textContent?.includes("Chat with the company Master is for owners.") && host.querySelector("textarea") === null, "the Master refusal is private to the Master route, and the hired chat remains blocked");

  const priorMasterReads = masterReads;
  rosterLoading = true;
  await at("/agents/master?tab=instructions");
  assert(masterReads === priorMasterReads && host.querySelector(".ap-hero") === null, "loading hosted roster never mounts or reads Master");
  rosterLoading = false;
  rosterFailed = true;
  await render();
  assert(masterReads === priorMasterReads && host.textContent?.includes("Could not load agents"), "failed hosted roster never reads Master and offers retry");
  rosterFailed = false;

  // An initial roster failure is not proof that a hosted alias is a hired agent.
  rosterFailed = true;
  await at("/agents/assistant?tab=instructions");
  assert(aliasReads === 0, "failed hosted roster never reads the reserved alias");
  assert(host.textContent?.includes("Could not load agents") && host.querySelector("main [role=alert] button"), "a failed roster offers retry");
  rosterFailed = false;

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
  const homeLink = [...host.querySelectorAll("a")].find((link) => link.textContent === "Open Home");
  assert(homeLink?.getAttribute("href") === "/?chat=assistant", "Open Home offers the owner's own assistant chat URL");
  assert(host.querySelector("[aria-current='page']")?.textContent?.includes("Your assistant"), "the personal row is the current one");
  await click(homeLink);
  assert(String(location.pathname) === "/" && String(location.search) === "?chat=assistant", "Open Home navigates to the owner's own assistant chat");
  await at("/agents");
  await click(host.querySelector(".rc-seats [aria-label='Open agent assistant']"));
  assert(String(location.pathname) === "/agents/profile/ap_1", "a hired assistant opens its own ID route instead of the caller's assistant");
  assert(host.querySelector("h1")?.textContent === "assistant" && aliasReads === 0, "the profile stays separate without an agent alias read");
  assert(host.querySelectorAll("[aria-current='page']").length === 1 && host.querySelector("[aria-current='page']")?.textContent?.startsWith("assistant"), "only the hired profile row is current");
  await at("/agents/profile/ap_1?tab=instructions");
  assert(host.querySelector("[role=tab][aria-selected=true]")?.textContent === "Instructions" && aliasReads === 0, "a hired profile ID deep link keeps the requested tab");
  await at("/agents");
  await click([...host.querySelectorAll(".ag-side-row")].find((row) => row.textContent?.startsWith("assistant")));
  assert(String(location.pathname) === "/agents/profile/ap_1", "the hired sidebar row opens the same ID route");
  await at("/agents/profile/ap_missing");
  assert(host.textContent?.includes("Agent details are unavailable.") && aliasReads === 0, "an unknown profile ID never falls back to the personal route");
  await at("/agents/assistant");
  // Without a personal row (an older API) the same route is the hired profile, as today.
  payload = roster();
  await render();
  assert(host.querySelector("h1")?.textContent === "assistant", "an older API: /agents/assistant is the hired profile");
  profiles = [];

  // An older API (no scope, no personal_assistant) renders as today, Master shown to everyone.
  payload = roster();
  const legacyReads = directoryReads;
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
  assert(directoryReads === legacyReads, "legacy and native boards do not read the directory");
  await React.act(() => root.unmount());
  resetPrivateSession();
  console.log("my assistant checks passed");
}
main().catch(async (error) => {
  await React.act(() => root.unmount());
  resetPrivateSession();
  throw error;
});
export {};
