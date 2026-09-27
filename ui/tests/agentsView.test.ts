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
  { ...base, alias: "owner", state: "waiting_input", queued: 1 },
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
let project = "all";
let open: string | null = null;
let openedIssue: string | null = null;
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
      React.createElement(Agents, {
        state,
        issues: issueState,
        project,
        open,
        onOpenAgent: (alias) => {
          open = alias;
        },
        onOpenIssue: (id) => {
          openedIssue = id;
        },
        onRetry: () => {},
      }),
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
  return Array.from(host.querySelectorAll(".agents-table .agent-name")).map(
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
    button("All6").getAttribute("aria-pressed") === "true",
    "All is default",
  );
  assert(aliases().length === 6, "stopped workers and inboxes remain in All");
  assert(
    !host.querySelector("[role='button']"),
    "cards do not wrap nested issue controls in simulated buttons",
  );
  assert(
    host.querySelectorAll(".agents-table th[scope='col']").length === 5,
    "five semantic columns",
  );
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
  await click("Clear filters");
  await search("SETTINGS");
  assert(aliases().join() === "dev", "task search is case insensitive");
  await search("does not exist");
  assert(
    host.textContent?.includes("No agents match these filters."),
    "no match copy is explicit",
  );
  await click("Clear filters");
  project = "demo";
  await render();
  assert(
    aliases().join() === "owner,dev",
    "project keeps exact dispatch and active ownership only",
  );
  assert(button("All2"), "counts reflect scoped agents");
  await click("Working1");
  assert(
    aliases().join() === "dev",
    "state filter cannot leak foreign-project work",
  );
  await click("Clear filters");
  await React.act(() =>
    host.querySelector<HTMLButtonElement>(".agents-table .lnk")!.click(),
  );
  assert(
    openedIssue === "DEMO-13" && open === null,
    "issue opens without opening the agent",
  );
  project = "all";
  await render();
  const trigger = button("Open agent blocked");
  trigger.focus();
  await click("Open agent blocked");
  await render();
  assert(
    host.querySelector("dialog[open][aria-labelledby='agent-detail-title']"),
    "native labeled modal opens",
  );
  assert(
    host
      .querySelector("[role='alert']")
      ?.textContent?.includes("Detail unavailable"),
    "detail failure is explicit",
  );
  const text = host.querySelector("dialog")!.textContent!;
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
    host.querySelector("dialog")?.textContent?.includes("session-blocked") &&
      !host.querySelector("[role='alert']"),
    "retry replaces failure with details",
  );
  await React.act(() =>
    host
      .querySelector("dialog")!
      .dispatchEvent(new Event("cancel", { cancelable: true })),
  );
  assert(open === null, "native Escape cancel invokes close");
  await render();
  assert(
    document.activeElement === trigger,
    "closing returns focus to opening control",
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
  holdDetail = false;
  await render();
  assert(
    host.querySelector("dialog h2")?.textContent === "stopped",
    "switching alias resets drawer identity",
  );
  await React.act(async () => releaseDetail!());
  await flush();
  assert(
    host.querySelector("dialog")?.textContent?.includes("session-stopped") &&
      !host.querySelector("dialog")?.textContent?.includes("session-dev"),
    "late old detail cannot cross into another alias",
  );
  open = null;
  state = { ...state, status: "stale", error: "Refresh unavailable" };
  await render();
  assert(
    host.textContent?.includes("stale") && aliases().length === 6,
    "stale payload stays visible",
  );
  project = "empty-project";
  await render();
  assert(
    !host.textContent?.includes("No agents match these filters."),
    "empty project differs from no filter match",
  );
  project = "demo";
  issueState = {
    ...issueState,
    data: null,
    status: "failed",
    error: "Issues unavailable",
  };
  await render();
  assert(
    aliases().length === 0 &&
      host.textContent?.includes("assignments could not be loaded") &&
      !host.querySelector(".agents-unassigned"),
    "missing issue index does not guess projects or claim agents are unassigned",
  );
  await React.act(() => root.unmount());
  console.log("agents view checks passed");
}
main().catch(async (error) => {
  await React.act(() => root.unmount());
  throw error;
});
export {};
