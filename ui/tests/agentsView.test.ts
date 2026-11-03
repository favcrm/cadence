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
const { profileMatchesFilter, profileMatchesSearch, profileWork } = require("../src/features/agents/agentView") as typeof import("../src/features/agents/agentView");
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
  group_root: false,
  tasks: [],
  running: 0,
  queued: 0,
  unknown: 0,
  parked: 0,
  fenced: false,
  on: [],
};
const fresh = { by_issue: {}, profiles: [], refresh_error: false, snapshot_age_secs: 0, stale: false };
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
        job_state: "running",
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
const threadReads: string[] = [];
let threadExtra: Record<string, unknown> = {};
let viewerIsOperator: boolean | null = true;
let viewerHosted = false;
globalThis.fetch = async (input, init) => {
  if (init?.method && init.method !== "GET")
    throw new Error("Agents must not write");
  const path = new URL(String(input), "http://localhost").pathname;
  if (path.startsWith("/api/threads/")) {
    threadReads.push(path);
    return new Response(JSON.stringify({ entries: [], cursor: 0, more_before: false, ...threadExtra }), {
      headers: { "Content-Type": "application/json" },
    });
  }
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
    } as AgentDetail["agent"],
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
  ...fresh,
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
        key: `${viewerIsOperator}`,
        operator: viewerIsOperator,
        onRetryAccess: () => {},
        hostedBoard: viewerHosted,
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
  return Array.from(host.querySelectorAll(".rc-seats .rc-seat .t"))
    .filter((b) => !b.closest(".ag-folded"))
    .map((b) => b.textContent);
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
  assert(!host.querySelector(".ag-seg"), "the sidebar's groups replace the status filters");
  assert(
    aliases().join() === "blocked,owner,dev,foreign",
    "current agents list attention first, then working, in one flat grid",
  );
  // CAD-1772: a plain Idle is grey; the master reads Ready (green) and always leads the roster.
  assert(seatStatus({ ...base, alias: "idler" }, (x: string) => x).tone === "ink", "plain Idle is the ink tone");
  assert(seatStatus({ ...base, alias: "master" }, (x: string) => x).text === "Ready", "the master at rest is Ready");
  state = loaded({ daemon: "reachable", agents: [...agents, { ...base, alias: "master" }], totals: null, ...fresh });
  await render();
  assert(aliases()[0] === "master", "the master is always the first seat");
  state = loaded({ daemon: "reachable", agents, totals: null, ...fresh });
  await render();
  // CAD-1772: compact seat cards, no stat tiles, no project scope.
  assert(host.querySelectorAll(".agents-triage-cell").length === 0, "the five stat tiles are gone");
  assert(host.querySelector(".ag-toolbar .ag-search"), "narrow widths keep a search above the cards");
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
  // CAD-2010: a profile card is one status line; its commands live in details.
  const profile = { id: "ap_1", alias: "Developer", role: "worker", team_role: "worker", catalogue: null, look: null, job: "Ship it", model: "inherit", runtimes: [] };
  const card = (over: Partial<IssueCard>) => ({ ...issues[0], owner: "Developer", container: false, ...over }) as IssueCard;
  assert(profileWork(profile, []) === null, "no owned issue names no work");
  assert(
    profileWork(profile, [card({ id: "DEMO-1", status: "done", created: "2026-01-01" }), card({ id: "DEMO-2", status: "backlog", created: "2026-01-02" }), card({ id: "DEMO-3", status: "done", created: "2026-01-03" }), card({ id: "DEMO-4", status: "doing", created: "2026-01-04", owner: "someone" })])?.id === "DEMO-2",
    "the newest open owned issue wins over a newer finished one and another owner's",
  );
  const finished = profileWork(profile, [card({ id: "DEMO-1", status: "done", created: "2026-01-01" }), card({ id: "DEMO-3", status: "dropped", created: "2026-01-03" })]);
  assert(finished?.id === "DEMO-3" && finished.state === "last", "with nothing open the newest closed issue is the last work");
  const owned = [card({ id: "DEMO-2", status: "doing", created: "2026-01-02", title: "Owned" })];
  const claimed = (state: string, id = "DEMO-9") => ({ ...profile, latest_claim: { issue_id: id, state } });
  assert(profileWork(claimed("running"), owned)?.state === "working", "a running claim wins over an open owned issue");
  assert(profileWork(claimed("blocked"), owned)?.state === "blocked", "a blocked claim reads blocked");
  assert(profileWork(claimed("queued"), owned)?.state === "waiting", "a queued claim reads waiting");
  assert(profileWork(claimed("result"), owned)?.id === "DEMO-2", "an ended claim does not outrank open owned work");
  const ended = profileWork(claimed("failed"), []);
  assert(ended?.id === "DEMO-9" && ended.state === "last" && ended.title === undefined, "an ended claim outside the list is the last work, by id alone");
  for (const bad of [{ state: "running" }, { issue_id: 7, state: "running" }, { issue_id: "", state: "running" }, "x", 7, [], {}, { issue_id: "DEMO-9", state: "paused" }, { issue_id: "DEMO-9", state: "constructor" }]) {
    const malformed = { ...profile, latest_claim: bad } as unknown as typeof profile;
    assert(profileWork(malformed, []) === null, `a malformed or unknown claim names no work: ${JSON.stringify(bad)}`);
    assert(profileWork(malformed, owned)?.state === "open", `owned work still shows beside a bad claim: ${JSON.stringify(bad)}`);
    assert(profileMatchesFilter(malformed, "idle"), `a bad claim doesn't move the filter: ${JSON.stringify(bad)}`);
  }
  const inFilter = (state: string, filter: Parameters<typeof profileMatchesFilter>[1]) => profileMatchesFilter(claimed(state), filter);
  assert(inFilter("running", "working") && !inFilter("running", "idle"), "a running claim files under Working, not Idle");
  assert(inFilter("blocked", "attention") && !inFilter("blocked", "idle"), "a blocked claim files under Needs attention");
  assert(!inFilter("queued", "idle") && inFilter("queued", "current"), "a waiting claim is not idle and stays in Current");
  assert(inFilter("result", "idle") && !inFilter("result", "working"), "an ended claim reads idle");
  assert(profileMatchesSearch(claimed("running", "DEMO-9"), "demo-9"), "search finds the claimed issue id printed on the card");
  assert(profileWork(claimed("running", "DEMO-2"), owned)?.title === "Owned", "a claim takes its title from the issue list");
  state = loaded({ daemon: "reachable", agents, totals: null, ...fresh, profiles: [profile] });
  await render();
  const seat = () => Array.from(host.querySelectorAll(".rc-seat")).find((row) => row.textContent?.includes("Developer"))!;
  assert(seat().textContent?.includes("Idle · no runtime"), "a profile with no owned work reads Idle");
  state = loaded({ daemon: "reachable", agents, totals: null, ...fresh, profiles: [{ ...profile, latest_claim: { issue_id: "DEMO-9", state: "running" } }] });
  await render();
  assert(seat().textContent?.includes("Working on DEMO-9"), "a running claim reads on the card");
  const sideProfile = Array.from(host.querySelectorAll(".ag-side-row")).find((row) => row.textContent?.includes("Developer"));
  assert(sideProfile?.textContent?.includes("Working on DEMO-9"), "the sidebar reads a live claim as the card does");
  assert(
    sideProfile?.parentElement?.querySelector(".ag-side-group")?.textContent?.startsWith("Working"),
    "a working profile sits under Working, not Not running",
  );
  state = loaded({ daemon: "reachable", agents, totals: null, ...fresh, profiles: [profile] });
  await render();
  assert(!host.querySelector(".rc-seats .rc-connect") && !host.querySelector(".rc-seats")?.textContent?.includes("cadence "), "the card prints no commands");
  await click("Open agent Developer");
  await render();
  assert(open === "Developer", "a profile opens on the same agent route as any agent");
  assert(!host.querySelector("dialog.rc-sheet"), "the drawer is gone");
  const gate = host.querySelector(".ag-chat-gate");
  assert(gate?.textContent?.includes("can’t reply here yet"), "the page says why nothing can answer");
  assert(gate?.textContent?.includes("cadence agent context ap_1"), "the reason sits with the connect commands");
  assert(!host.querySelector("[data-composer]"), "no composer pretends to send");
  assert(!reads.includes("/api/agents/Developer"), "a profile is not read as a registered agent");
  await click("Details");
  const panel = host.querySelector(".ag-chat-panel");
  assert(panel?.textContent?.includes("Ship it") && panel?.textContent?.includes("Model"), "details hold the job and model");
  open = null;
  state = loaded({ daemon: "reachable", agents, totals: null, ...fresh });
  await render();
  const sideRows = () => Array.from(host.querySelectorAll(".ag-side-row .t")).map((t) => t.textContent);
  assert(sideRows().length === 7, "the sidebar keeps stopped workers and mailboxes discoverable, under Overview");
  const foldedGroups = Array.from(host.querySelectorAll(".ag-side details summary")).map((g) => g.textContent);
  assert(foldedGroups.join().includes("Stopped") && foldedGroups.join().includes("Mailboxes"), "stopped agents and mailboxes fold");
  assert(!Array.from(host.querySelectorAll(".ag-side-group")).some((g) => g.textContent?.startsWith("Idle")), "an empty group does not appear");
  await search("SETTINGS");
  assert(aliases().join() === "dev", "task search is case insensitive");
  await search("does not exist");
  assert(
    host.textContent?.includes("No agents match “does not exist”."),
    "no match copy is explicit",
  );
  await search("");
  // Agents are the team's, not a project's: a project in the URL state does not filter them.
  await render();
  assert(aliases().length === 4, "the roster lists the team regardless of project");
  await click("Open agent blocked");
  await render();
  assert(
    host.querySelector("main.ag-chat h1")?.textContent === "blocked" && host.querySelector("[data-composer]"),
    "an agent opens on its conversation with a composer",
  );
  assert(
    host.querySelector("main.ag-chat")?.textContent?.includes("Some details aren’t available") &&
      !host.querySelector("main.ag-chat [role='alert']"),
    "a detail failure is a quiet line, not an alert",
  );
  tab = "settings";
  await render();
  assert(host.querySelector("main.agents-page [role='tablist']"), "the profile keeps its tabs");
  assert(!host.querySelector(".ap-stats"), "unknown stats show no tiles of dashes");
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
  await click("Retry");
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
  assert((tab as string) === "overview", "Escape on a profile section returns to the conversation");
  await render();
  await React.act(() =>
    document.body.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    ),
  );
  assert(open === null, "Escape on the conversation leaves the agent page");
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
  assert(
    host.textContent?.includes("stale") && aliases().length === 4,
    "stale payload stays visible",
  );
  state = loaded({ daemon: "reachable", agents: [agents[3], agents[4]], totals: null, ...fresh });
  await render();
  assert(host.textContent?.includes("No current agents") && aliases().length === 0, "only stopped agents and mailboxes yield a clear current-view empty state");
  state = loaded({ daemon: "reachable", agents, totals: null, ...fresh });
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
  // CAD-2054 review: who may see a conversation, and what each page claims.
  failDetail = false;
  issueState = loaded(issues);
  const dock = () => host.querySelector("[data-chat-dock]")?.textContent ?? "";
  state = loaded({ daemon: "reachable", agents, totals: null, ...fresh });
  viewerIsOperator = false;
  open = "dev";
  tab = "overview";
  threadReads.length = 0;
  await render();
  assert(!host.querySelector("[data-composer]"), "a non-operator gets no composer");
  assert(dock().includes("for the operator"), "a non-operator is told why");
  assert(threadReads.length === 0, "a non-operator does not read an agent's conversation, as on Threads");
  viewerHosted = true;
  await render();
  assert(dock().includes("Your assistant is on Home"), "a hosted member is sent to their assistant");
  viewerHosted = false;
  viewerIsOperator = true;
  state = { ...state, data: null, status: "loading" };
  threadReads.length = 0;
  await render();
  assert(!host.querySelector("[data-composer]") && threadReads.length === 0, "nothing is read or offered before the roster answers");
  state = loaded({ daemon: "reachable", agents, totals: null, ...fresh });
  open = "inbox";
  await render();
  assert(dock().includes("mailbox") && !host.querySelector("[data-composer]"), "a mailbox has no composer");
  const withProfile = { id: "ap_2", alias: "Writer", role: "worker", team_role: "worker", catalogue: null, look: null, job: "Write", model: "inherit", runtimes: [] } as unknown as import("../src/lib/types").HiredProfile;
  state = loaded({ daemon: "reachable", agents, totals: null, ...fresh, profiles: [withProfile] });
  open = "Writer";
  tab = "settings";
  reads.length = 0;
  await render();
  assert(host.querySelector(".ag-chat-gate") && !host.textContent?.includes("Loading agent details"), "a section link on a profile lands on its page, not a stuck read");
  assert(!reads.includes("/api/agents/Writer"), "a profile is still not read as an agent");
  tab = "overview";
  const cloudAgent = { ...base, alias: "cloudy", provider: "agenticos-cloud", endpoint_kind: "cloud" } as Agent;
  state = loaded({ daemon: "reachable", agents: [...agents, cloudAgent], totals: null, ...fresh });
  open = "cloudy";
  await render();
  await click("Details");
  const cloudPanel = host.querySelector(".ag-chat-panel")?.textContent ?? "";
  assert(!cloudPanel.includes("Doing now") && !cloudPanel.includes("MCP & tools"), "a conversational-only cloud agent claims no local work panels");
  threadExtra = { cloud_withheld: "denied" };
  open = "dev";
  state = loaded({ daemon: "reachable", agents, totals: null, ...fresh });
  await render();
  assert(host.textContent?.includes("Cloud content unavailable") && !host.textContent?.includes("No messages with dev"), "a withheld thread does not read as empty");
  threadExtra = {};
  viewerIsOperator = null;
  threadReads.length = 0;
  await render();
  assert(dock().includes("couldn’t confirm your access") && !dock().includes("for the operator"), "an unanswered access check is not a non-operator");
  assert(!host.querySelector("[data-composer]") && threadReads.length === 0, "an unanswered access check grants nothing");
  viewerIsOperator = true;
  // A roster that failed on a fresh mount claims nothing about the agent.
  failDetail = true;
  await React.act(() => root.unmount());
  const freshHost = document.createElement("div");
  document.body.append(freshHost);
  const freshRoot = createRoot(freshHost);
  const failedRoster = { data: null, status: "failed", error: "Unavailable", asOf: null, inFlight: false } as unknown as typeof state;
  const mountFresh = async (alias: string, section: typeof tab) => {
    await React.act(() =>
      freshRoot.render(
        React.createElement(LocaleProvider, null, React.createElement(Agents, {
          key: `${alias}:${section}`,
          operator: true,
          onRetryAccess: () => {},
          hostedBoard: false,
          state: failedRoster,
          issues: issueState,
          open: alias,
          tab: section,
          recruit: false,
          ask: false,
          onAsk: () => {},
          onOpenAgent: () => {},
          onTab: () => {},
          onRecruit: () => {},
          onOpenIssue: () => {},
          onRetry: () => {},
        })),
      ),
    );
    await flush();
    await flush();
  };
  await mountFresh("owner", "work");
  assert(!freshHost.textContent?.includes("Loading agent details"), "a failed roster does not leave details loading forever");
  assert(freshHost.textContent?.includes("Some details aren’t available"), "a failed roster still reports the detail failure, with its Retry");
  assert(!freshHost.textContent?.includes("Nothing right now") && !freshHost.textContent?.includes("Nothing queued"), "a failed roster claims nothing about an agent's work");
  await mountFresh("foreign", "overview");
  assert(freshHost.textContent?.includes("Could not load agents") && !freshHost.textContent?.includes("Loading agent details"), "the page gives one answer, with no loading line beside the failure");
  const detailsButton = Array.from(freshHost.querySelectorAll("button")).find((b) => b.textContent?.trim() === "Details");
  assert(detailsButton, "the details panel can be opened");
  await React.act(() => detailsButton.click());
  await flush();
  const failedPanel = freshHost.querySelector(".ag-chat-panel")?.textContent ?? "";
  assert(failedPanel.includes("Agent details are unavailable."), "the panel says its details could not be read");
  for (const empty of ["Nothing queued.", "No results yet.", "No events yet."])
    assert(!failedPanel.includes(empty), `the panel claims nothing about unread details: ${empty}`);
  await React.act(() => freshRoot.unmount());
  console.log("agents view checks passed");
}
main().catch(async (error) => {
  await React.act(() => root.unmount());
  throw error;
});
export {};
