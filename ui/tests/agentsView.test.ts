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
const { seatStatus } = require("../src/features/agents/SeatCard") as typeof import("../src/features/agents/SeatCard");
const Agents = (
  require("../src/features/agents/Agents") as typeof import("../src/features/agents/Agents")
).default;
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
type Agent = import("../src/lib/types").Agent;
type AgentDetail = import("../src/lib/types").AgentDetail;
type IssueCard = import("../src/lib/types").IssueCard;
const base: Agent = {
  alias: "dev",
  provider: "pi",
  endpoint_kind: "pty",
  role: "Developer",
  state: "idle",
  group: "pm",
  running: 0,
  queued: 0,
  unknown: 0,
  parked: 0,
  fenced: false,
  on: [],
};
const agents: Agent[] = [
  {
    ...base,
    alias: "dev",
    running: 1,
    state: "busy",
    last_activity: 1790562108.64,
    on: ["DEMO-12"],
    tasks: [
      {
        task: "t12",
        issue: "DEMO-12",
        title: "Account settings",
        task_state: "running",
        job: "j12",
      },
    ],
  },
  {
    ...base,
    alias: "blocked",
    fenced: true,
    unknown: 1,
    recovery:
      "Outcome unknown. Inspect before reconciling. Do not resume twice.",
    resume: "cadence agent resume blocked",
    model_reported: "reported-model",
    model_configured: "configured-model",
    model_selection: { source: "role_default", revision: 0, lookup_role: "qa" },
    effort_reported: "high",
    quota: { state: "available", remaining: 0, limit: 100 },
  },
  { ...base, alias: "owner", state: "waiting_input", queued: 1, last_activity: "not-a-time" },
  { ...base, alias: "stopped", state: "stopped" },
  { ...base, alias: "inbox", provider: "inbox", inbox: true, queued: 840 },
  { ...base, alias: "foreign", running: 1, on: ["SITE-8"] },
];
const issues: IssueCard[] = [
  {
    id: "DEMO-12",
    project: "demo",
    title: "Account settings",
    status: "doing",
    owner: "dev",
  },
  {
    id: "DEMO-13",
    project: "demo",
    title: "Review",
    status: "review",
    owner: "owner",
  },
  {
    id: "SITE-8",
    project: "site",
    title: "Content",
    status: "doing",
    owner: "foreign",
  },
] as IssueCard[];
let failDetail = true;
let releaseDetail: (() => void) | null = null;
let holdDetail = false;
const reads: string[] = [];
globalThis.fetch = async (input, init) => {
  if (init?.method && init.method !== "GET")
    throw new Error("Agents must not write");
  const path = new URL(String(input), "http://localhost").pathname;
  reads.push(path);
  if (holdDetail)
    await new Promise<void>((resolve) => {
      releaseDetail = resolve;
    });
  if (failDetail)
    return new Response(JSON.stringify({ error: "Detail unavailable" }), {
      status: 503,
    });
  const alias = decodeURIComponent(path.split("/").pop()!);
  const a = agents.find((a) => a.alias === alias)!;
  const { role, tasks, ...runtime } = a;
  const detail: AgentDetail = {
    agent: {
      ...runtime,
      role: role ?? undefined,
      tasks: tasks?.map((t) => t.task),
      enabled: true,
      session_id: `session-${alias}`,
    },
    queued: a.queued,
    unknown: a.unknown,
    running: [],
    events: [],
    fenced: a.fenced,
    recovery: a.recovery,
    resume: a.resume,
    on: a.on,
  };
  return new Response(JSON.stringify(detail), {
    headers: { "Content-Type": "application/json" },
  });
};
function assert(value: unknown, what: string): asserts value {
  if (!value) throw new Error(what);
}
const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const loaded = <T>(data: T): import("../src/lib/cache").ResourceState<T> => ({
  data,
  status: "ok",
  error: null,
  asOf: Date.now(),
  inFlight: false,
});
let open: string | null = null;
let tab: import("../src/features/agents/agentTabModel").AgentTab = "overview";
let recruiting = false;
let state = loaded<import("../src/lib/types").AgentsPayload>({
  daemon: "reachable",
  agents,
  totals: null,
});
let issueState = loaded(issues);
const flush = async () => {
  await React.act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
};
async function render() {
  await React.act(() =>
    root.render(
      React.createElement(LocaleProvider, null, React.createElement(Agents, {
        state,
        issues: issueState,
        open,
        tab,
        recruit: recruiting,
        ask: false,
        onAsk: () => {},
        onOpenAgent: (alias) => {
          open = alias;
          tab = "overview";
        },
        onTab: (next) => {
          tab = next;
        },
        onRecruit: (on) => {
          recruiting = on;
        },
        onOpenIssue: () => {},
        onRetry: () => {},

      })),
    ),
  );
  await flush();
}
function button(label: string) {
  const b = Array.from(host.querySelectorAll("button")).find(
    (b) =>
      b.textContent?.trim() === label || b.getAttribute("aria-label") === label,
  );
  assert(b, `button ${label} exists`);
  return b;
}
async function click(label: string) {
  await React.act(() => button(label).click());
  await flush();
}
function aliases() {
  return Array.from(host.querySelectorAll(".rc-seats .rc-seat .t")).map(
    (b) => b.textContent,
  );
}
async function search(value: string) {
  const input = host.querySelector<HTMLInputElement>("#agents-search");
  assert(input, "native search exists");
  const setter = Object.getOwnPropertyDescriptor(
    win.HTMLInputElement.prototype,
    "value",
  )!.set!;
  await React.act(() => {
    setter.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
async function main() {
  await render();
  assert(
    button("Current4").getAttribute("aria-pressed") === "true",
    "Current is the default when the roster contains stopped agents and mailboxes",
  );
  assert(
    aliases().join() === "blocked,owner,dev,foreign",
    "current agents list attention first, then working, in one flat grid",
  );
  // CAD-1772: a plain Idle is grey; the master reads Ready (green) and always leads the roster.
  assert(seatStatus({ ...base, alias: "idler" }, (x: string) => x).tone === "ink", "plain Idle is the ink tone");
  assert(seatStatus({ ...base, alias: "master" }, (x: string) => x).text === "Ready", "the master at rest is Ready");
  state = loaded({ daemon: "reachable", agents: [...agents, { ...base, alias: "master" }], totals: null });
  await render();
  assert(aliases()[0] === "master", "the master is always the first seat");
  state = loaded({ daemon: "reachable", agents, totals: null });
  await render();
  // CAD-1772: compact seat cards, no stat tiles, no project scope.
  assert(host.querySelectorAll(".agents-triage-cell").length === 0, "the five stat tiles are gone");
  assert(host.querySelector(".ag-toolbar .ag-seg") && host.querySelector(".ag-toolbar .ag-search"), "one compact toolbar row");
  assert(!Array.from(host.querySelectorAll("button")).some((b) => b.textContent?.trim() === "Message"), "seat cards carry no inline Message button");
  assert(
    Array.from(host.querySelectorAll(".rc-seat")).find((row) => row.textContent?.includes("owner"))?.textContent?.includes("1 queued"),
    "the queue rides the status row as the one small badge",
  );
  assert(
    Array.from(host.querySelectorAll(".rc-seat")).find((row) => row.textContent?.includes("dev"))?.textContent?.includes("Working on DEMO-12"),
    "a working seat names its issue in the one-line status",
  );
  assert(
    Array.from(host.querySelectorAll(".rc-seat")).find((row) => row.textContent?.includes("blocked"))?.textContent?.includes("Attention"),
    "a fenced seat reads Attention in its status line",
  );
  await click("All6");
  assert(aliases().length === 6, "All keeps stopped workers and mailboxes discoverable");
  await click("Stopped1");
  assert(aliases().join() === "stopped", "stopped agents have an explicit view");
  await click("Needs attention2");
  assert(
    aliases().join() === "blocked,owner",
    "waiting and fenced agents filter accurately",
  );
  await click("Mailboxes1");
  assert(
    aliases().join() === "inbox",
    "840 inbox messages do not count as working",
  );
  await click("Current4");
  await search("SETTINGS");
  assert(aliases().join() === "dev", "task search is case insensitive");
  await search("does not exist");
  assert(
    host.textContent?.includes("No agents match these filters."),
    "no match copy is explicit",
  );
  await search("");
  // Agents are the team's, not a project's: a project in the URL state does not filter them.
  await render();
  assert(aliases().length === 4, "the roster lists the team regardless of project");
  await click("All6");
  await click("Open agent blocked");
  await render();
  assert(
    host.querySelector("main.agents-page h1")?.textContent === "blocked" &&
      host.querySelector("[role='tablist']"),
    "the agent page opens at its alias with its tabs",
  );
  assert(
    host
      .querySelector("[role='alert']")
      ?.textContent?.includes("Detail unavailable"),
    "detail failure is explicit",
  );
  tab = "settings";
  await render();
  const text = host.querySelector("main.agents-page")!.textContent!;
  for (const evidence of [
    agents[1].recovery!,
    agents[1].resume!,
    "reported-model",
    "configured configured-model",
    "role default · lookup qa · revision 0",
    "effort high",
    "blocked",
    "0 remaining",
  ]) {
    assert(
      text.includes(evidence),
      `failed detail retains observed evidence: ${evidence}`,
    );
  }
  assert(
    reads.at(-1) === "/api/agents/blocked",
    "detail reads exact alias route",
  );
  failDetail = false;
  await click("Retry details");
  assert(
    host.querySelector("main.agents-page")?.textContent?.includes("session-blocked") &&
      !host.querySelector("[role='alert']"),
    "retry replaces failure with details",
  );
  await React.act(() =>
    document.body.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    ),
  );
  assert(open === null, "Escape leaves the agent page");
  await render();
  assert(
    button("Open agent blocked") && host.querySelector(".rc-seats"),
    "leaving the page returns to the roster",
  );
  holdDetail = true;
  await click("Open agent dev");
  await render();
  assert(
    host
      .querySelector("[role='status']")
      ?.textContent?.includes("Loading agent details"),
    "detail loading is announced",
  );
  open = "stopped";
  tab = "settings";
  holdDetail = false;
  await render();
  assert(
    host.querySelector("main.agents-page h1")?.textContent === "stopped",
    "switching alias resets the page identity",
  );
  await React.act(async () => releaseDetail!());
  await flush();
  assert(
    host.querySelector("main.agents-page")?.textContent?.includes("session-stopped") &&
      !host.querySelector("main.agents-page")?.textContent?.includes("session-dev"),
    "late old detail cannot cross into another alias",
  );
  open = null;
  state = { ...state, status: "stale", error: "Refresh unavailable" };
  await render();
  await click("Current4");
  assert(
    host.textContent?.includes("stale") && aliases().length === 4,
    "stale payload stays visible",
  );
  state = loaded({ daemon: "reachable", agents: [agents[3], agents[4]], totals: null });
  await render();
  assert(host.textContent?.includes("No current agents") && aliases().length === 0, "only stopped agents and mailboxes yield a clear current-view empty state");
  state = loaded({ daemon: "reachable", agents, totals: null });
  issueState = {
    ...issueState,
    data: null,
    status: "failed",
    error: "Issues unavailable",
  };
  await render();
  assert(
    aliases().length === 4 && !host.textContent?.includes("assignments could not be loaded"),
    "a missing issue index never hides the team: agents are not project-scoped",
  );
  state = { ...state, data: null, status: "loading" };
  await render();
  assert(
    host.textContent?.includes("Loading agents…") &&
      !host.textContent?.includes("0 across all projects"),
    "unloaded agent collection is not an empty collection",
  );
  await React.act(() => root.unmount());
  console.log("agents view checks passed");
}
main().catch(async (error) => {
  await React.act(() => root.unmount());
  throw error;
});
export {};
