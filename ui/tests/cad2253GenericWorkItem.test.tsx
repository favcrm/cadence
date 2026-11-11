/**
 * CAD-2253: domain-neutral work item UI for non-repository projects, recorded
 * approval heads + staged actions, and symmetric follow-up link rendering.
 */
import type { ResourceState } from "../src/lib/cache";
import type { IssueDetail } from "../src/lib/types";
import type { StagedWrite } from "../src/features/decisions/stagedWrite";

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
  const win = new Window({ url: "http://localhost/projects/demo-company/issues/DEMOCOMPANY-1" });
  for (const name of [
    "window",
    "document",
    "Node",
    "Element",
    "HTMLElement",
    "HTMLDialogElement",
    "HTMLInputElement",
    "HTMLTextAreaElement",
    "HTMLSelectElement",
    "SVGElement",
    "navigator",
    "MutationObserver",
    "ResizeObserver",
    "Event",
    "MouseEvent",
    "KeyboardEvent",
    "location",
    "history",
    "localStorage",
    "sessionStorage",
  ]) {
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  }
  for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"]) {
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  }
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

  const loader = require("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, arguments);
  };

  const React = require("react") as typeof import("react");
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof import("../screen-ui/src/navigation");
  const { hostNavigation } = require("../src/ui/hostNavigation") as typeof import("../src/ui/hostNavigation");
  const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
  const {
    hasCodeDelivery,
    hostedAssignmentText,
    matchingStagedActions,
    shownLinks,
  } = require("../src/features/issues/model") as typeof import("../src/features/issues/model");
  const IssuePage = (require("../src/features/issues/IssuePage") as typeof import("../src/features/issues/IssuePage")).default;
  const WorkItemDetail = (require("../src/features/work-items/WorkItemDetail") as typeof import("../src/features/work-items/WorkItemDetail")).default;

  const baseIssue = (over: Partial<IssueDetail> = {}): IssueDetail => ({
    id: "DEMOCOMPANY-1",
    project: "demo-company",
    title: "Finalize Q3 enterprise pricing sheet",
    status: "review",
    status_source: "direct",
    priority: "P1",
    owner: "priya",
    blocked_by: [],
    relates: ["DEMOCOMPANY-2"],
    refs: [],
    paths: [],
    tags: [],
    container: false,
    ready: true,
    blocked: false,
    created: "2026-11-11T08:00:00Z",
    rev: "wrev_demo1",
    checks: { done: 2, total: 2 },
    work: { type: "task", type_source: "implicit", weight: 1 },
    frontmatter: {},
    body: "Prepare enterprise tier pricing.\n\n## Acceptance\n- [x] Legal sign-off\n- [x] Finance margin check",
    path: "issue.md",
    links: {
      children: [],
      blocked_by: [],
      blocks: [],
      relates: [
        { id: "DEMOCOMPANY-2", title: "Publish Q3 pricing FAQ", status: "doing", missing: false },
        { id: "DEMOCOMPANY-3", title: "Sales enablement deck", status: "backlog", missing: false },
      ],
      duplicates: [],
    },
    approvals: {
      version: "hosted-cadence-issue-approvals.v1",
      scope: "project",
      available: true,
      reason: null,
      complete: true,
      heads: [],
    },
    local_assignment: {
      available: true,
      reason: null,
      assignments: [
        {
          target: "priya",
          state: "completed",
          updated_at: "2026-11-11T09:00:00Z",
          text: "Uploaded final Q3 pricing matrix.",
          deliverable: { type: "document", title: "Pricing memo v2", ref: "doc-q3-pricing" },
        },
      ],
    },
    comments: [],
    notes_chain: [],
    artifacts: [],
    activity: [],
    ...over,
  });

  // 1. Pure model invariants
  const nonDev = baseIssue();
  equal(hasCodeDelivery(nonDev, []), false, "non-repo project without code evidence hides code delivery chrome");
  equal(hasCodeDelivery(nonDev, null, true), false, "hosted item hides code delivery chrome while projects are still loading");
  equal(hasCodeDelivery(nonDev, [{ remote: "favcrm/cadence" }]), true, "repo-backed project shows code delivery chrome");
  equal(hasCodeDelivery(nonDev, undefined), true, "standalone callers without projectRepos keep task default");
  equal(
    hasCodeDelivery(baseIssue({ work: { type: "epic", type_source: "explicit", weight: 1 } }), [{ remote: "favcrm/cadence" }]),
    false,
    "epics without code evidence omit code delivery chrome",
  );

  const codeHosted = baseIssue({
    id: "CAD-2244",
    project: "cadence",
    refs: [{ kind: "pr", url: "https://github.com/favcrm/cadence/pull/1700" }],
    approvals: {
      version: "hosted-cadence-issue-approvals.v1",
      scope: "project",
      available: true,
      reason: null,
      complete: true,
      heads: [
        {
          repo: "favcrm/cadence",
          pr: 1700,
          head_sha: "e2444a22320289abcdef",
          state: "in-force",
          recorded_at: "2026-11-11T08:15:00Z",
          source: "operator",
          action: "audit-approve",
        },
      ],
    },
  });
  equal(hasCodeDelivery(codeHosted, []), true, "hosted PR/approval-bearing issue keeps code delivery chrome even when repos=[]");

  equal(
    shownLinks(nonDev.links, nonDev.relates).map((r) => [r.id, r.unlinkKind, r.status]),
    [
      ["DEMOCOMPANY-2", "relates", "doing"],
      ["DEMOCOMPANY-3", null, "backlog"],
    ],
    "outgoing relates unlinks; incoming symmetric relates has unlinkKind null",
  );

  equal(hostedAssignmentText(nonDev, (s) => s), "priya · completed", "hosted assignment summary reads local_assignment");

  const stagedRows: StagedWrite[] = [
    {
      id: "act-1",
      kind: "app_call",
      status: "approved",
      createdAt: Date.parse("2026-11-11T08:30:00Z"),
      decidedAt: Date.parse("2026-11-11T08:35:00Z"),
      title: "Approve Q3 enterprise pricing",
      app: "CRM",
      target: { kind: "deal", label: "Enterprise pricing", id: "DEMOCOMPANY-1" },
      changes: [],
      unchangedCount: 0,
      changesComplete: true,
      reason: null,
      consent: null,
      effects: [],
      raw: "{}",
      origin: { via: "chat", kind: "agent", label: "Atlas" },
      result: { ok: true },
      recordLink: null,
    },
    {
      id: "act-other",
      kind: "create_issue",
      status: "pending",
      createdAt: Date.parse("2026-11-11T08:40:00Z"),
      decidedAt: null,
      title: "Follow up on DEMOCOMPANY-10",
      app: null,
      target: null,
      changes: [],
      unchangedCount: 0,
      changesComplete: true,
      reason: null,
      consent: null,
      effects: [],
      raw: '{"ref":"DEMOCOMPANY-10"}',
      origin: null,
      result: null,
      recordLink: null,
    },
  ];
  equal(
    matchingStagedActions("DEMOCOMPANY-1", stagedRows).map((r) => r.id),
    ["act-1"],
    "bounded token match picks DEMOCOMPANY-1 and never DEMOCOMPANY-10",
  );

  // 2. Hosted non-dev IssuePage never calls /lane and omits dev-only chrome
  const detailsById: Record<string, IssueDetail> = {
    "DEMOCOMPANY-1": nonDev,
    "CAD-2244": codeHosted,
  };
  let laneRequests = 0;
  globalThis.fetch = async (input) => {
    const path = new URL(String(input), "http://localhost").pathname;
    if (path.endsWith("/lane")) {
      laneRequests += 1;
      return new Response(JSON.stringify({ error: "Not found" }), { status: 404 });
    }
    if (path.endsWith("/history")) {
      return new Response(JSON.stringify({ history: [] }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }
    const match = /^\/api\/issues\/([^/]+)$/.exec(path);
    if (match && detailsById[match[1]]) {
      return new Response(JSON.stringify(detailsById[match[1]]), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }
    throw new Error(`Unexpected fetch in CAD-2253 test: ${path}`);
  };

  const opened: string[] = [];
  {
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    await React.act(async () => {
      root.render(
        React.createElement(
          LocaleProvider,
          null,
          React.createElement(NavigationProvider, {
            adapter: hostNavigation,
            children: React.createElement(IssuePage, {
              project: "demo-company",
              id: "DEMOCOMPANY-1",
              tab: "pr", // hidden on non-dev items; must fall back to overview
              tabHref: (tab) => `/projects/demo-company/issues/DEMOCOMPANY-1?tab=${tab}`,
              issues: [],
              agents: null,
              readOnly: false,
              writeBlock: null,
              kickoffBlock: null,
              hosted: true,
              projectRepos: [],
              stagedActions: stagedRows,
              planHref: "/home",
              onWrite: () => {},
              onError: () => {},
              onOpen: (id) => opened.push(id),
              onToast: () => {},
            }),
          }),
        ),
      );
    });
    await React.act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });

    equal(laneRequests, 0, "hosted IssuePage never fetches /api/issues/:id/lane");
    const text = host.textContent ?? "";
    assert(!text.includes("Lane could not be loaded"), "no broken lane error notice on hosted board");
    assert(!text.includes("Kick off"), "hosted board hides local daemon Kick off button");
    assert(!text.includes("No merge recorded"), "non-dev item omits Merge verification row");
    assert(!text.includes("No deployment evidence"), "non-dev item omits Deployment verification row");
    assert(text.includes("Assignment"), "hosted overview shows Assignment label");
    assert(text.includes("priya · completed"), "hosted overview shows recorded assignment state");
    assert(text.includes("Pricing memo v2"), "hosted Result surfaces assignment deliverable");
    assert(text.includes("Approve Q3 enterprise pricing"), "hosted Approvals row surfaces matching staged action");
    assert(text.includes("Related & follow-ups"), "hosted Verification surfaces follow-up links");

    const tabLinks = Array.from(host.querySelectorAll(".section-nav-link")).map((a) => a.textContent?.trim());
    equal(tabLinks, ["Overview", "Activity", "Evidence"], "non-dev hosted IssuePage shows only domain-neutral tabs");

    const followUpBtn = Array.from(host.querySelectorAll("button")).find((b) => b.textContent?.trim() === "DEMOCOMPANY-2");
    assert(followUpBtn, "follow-up link button renders in WorkResult");
    await React.act(() => followUpBtn.click());
    equal(opened, ["DEMOCOMPANY-2"], "clicking follow-up triggers onOpen");

    const unlinkLabels = Array.from(host.querySelectorAll('aside button[aria-label^="Unlink "]')).map(
      (b) => b.getAttribute("aria-label"),
    );
    equal(
      unlinkLabels,
      ["Unlink Related DEMOCOMPANY-2"],
      "only outgoing DEMOCOMPANY-2 can be unlinked; incoming symmetric DEMOCOMPANY-3 cannot",
    );

    await React.act(async () => root.unmount());
    host.remove();
  }

  // 3. Hosted software PR IssuePage keeps Pull request tab, Merge/Deployment, and renders approval heads
  {
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    await React.act(async () => {
      root.render(
        React.createElement(
          LocaleProvider,
          null,
          React.createElement(NavigationProvider, {
            adapter: hostNavigation,
            children: React.createElement(IssuePage, {
              project: "cadence",
              id: "CAD-2244",
              tab: "overview",
              tabHref: (tab) => `/projects/cadence/issues/CAD-2244?tab=${tab}`,
              issues: [],
              agents: null,
              readOnly: false,
              writeBlock: null,
              kickoffBlock: null,
              hosted: true,
              projectRepos: [],
              planHref: "/home",
              onWrite: () => {},
              onError: () => {},
              onOpen: () => {},
              onToast: () => {},
            }),
          }),
        ),
      );
    });
    await React.act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    const text = host.textContent ?? "";
    const tabLinks = Array.from(host.querySelectorAll(".section-nav-link")).map((a) => a.textContent?.trim());
    equal(tabLinks, ["Overview", "Activity", "Pull request", "Evidence"], "PR-bearing hosted issue keeps Pull request tab");
    assert(text.includes("favcrm/cadence #1700 @ e2444a223202"), "WorkResult renders recorded approval head");
    assert(text.includes("in-force"), "WorkResult renders in-force approval state");
    assert(text.includes("Merge"), "PR-bearing issue keeps Merge row");
    await React.act(async () => root.unmount());
    host.remove();
  }

  // 4. Shared WorkItemDetail drawer: non-dev tab filtering, follow-up links, and ApprovalsPanel
  {
    const readState: ResourceState<IssueDetail> = {
      data: nonDev,
      status: "ok",
      error: null,
      asOf: Date.now(),
      inFlight: false,
    };
    const drawerOpened: string[] = [];
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    await React.act(async () => {
      root.render(
        React.createElement(
          LocaleProvider,
          null,
          React.createElement(WorkItemDetail, {
            id: "DEMOCOMPANY-1",
            origin: "threads",
            detail: nonDev,
            readState,
            href: "/projects/demo-company/issues/DEMOCOMPANY-1",
            onClose: () => {},
            readOnly: false,
            onOpen: (id) => drawerOpened.push(id),
            projectRepos: [],
            stagedActions: stagedRows,
          }),
        ),
      );
    });
    await React.act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });

    const tabs = Array.from(host.querySelectorAll('[role="tab"]')).map((b) => b.textContent?.trim());
    equal(tabs, ["Overview", "Evidence", "Approvals", "Activity", "Audit"], "WorkItemDetail omits Code tab on non-repo item");
    assert(host.textContent?.includes("Publish Q3 pricing FAQ"), "Overview renders linked follow-up title");
    const linkBtn = Array.from(host.querySelectorAll("button")).find((b) => b.textContent?.trim() === "DEMOCOMPANY-2");
    assert(linkBtn, "Overview renders clickable follow-up id");
    await React.act(() => linkBtn.click());
    equal(drawerOpened, ["DEMOCOMPANY-2"], "WorkItemDetail follow-up button opens linked item");

    const approvalsTab = Array.from(host.querySelectorAll('[role="tab"]')).find(
      (b) => b.textContent?.trim() === "Approvals",
    ) as HTMLButtonElement | undefined;
    assert(approvalsTab, "Approvals tab exists");
    await React.act(() => approvalsTab.click());
    assert(host.textContent?.includes("Approve Q3 enterprise pricing"), "Approvals tab lists matching staged/decided action");
    assert(host.textContent?.includes("Atlas (agent)"), "Approvals tab shows staged action origin");

    await React.act(async () => root.unmount());
    host.remove();
  }

  // 5. Shared WorkItemDetail ApprovalsPanel renders detail.approvals.heads + zh-TW locale
  {
    win.localStorage.setItem("cadence-locale", "zh-TW");
    const readState: ResourceState<IssueDetail> = {
      data: codeHosted,
      status: "ok",
      error: null,
      asOf: Date.now(),
      inFlight: false,
    };
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    await React.act(async () => {
      root.render(
        React.createElement(
          LocaleProvider,
          null,
          React.createElement(WorkItemDetail, {
            id: "CAD-2244",
            origin: "home",
            detail: codeHosted,
            readState,
            href: "/projects/cadence/issues/CAD-2244",
            onClose: () => {},
            readOnly: false,
            onOpen: () => {},
            projectRepos: [],
          }),
        ),
      );
    });
    await React.act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    const tabs = Array.from(host.querySelectorAll('[role="tab"]')) as HTMLButtonElement[];
    equal(tabs.length, 6, "PR/approval-bearing WorkItemDetail keeps all 6 tabs including Code");
    assert(host.textContent?.includes("相關與跟進項目"), "zh-TW translates Related & follow-ups");
    const approvalsTab = tabs.find((b) => b.textContent?.trim() === "批准記錄");
    assert(approvalsTab, "zh-TW translates Approvals tab");
    await React.act(() => approvalsTab.click());
    assert(host.textContent?.includes("favcrm/cadence #1700"), "ApprovalsPanel renders recorded repo and PR");
    assert(host.textContent?.includes("e2444a223202 · 生效中"), "ApprovalsPanel renders head SHA and translated in-force state");

    win.localStorage.removeItem("cadence-locale");
    await React.act(async () => root.unmount());
    host.remove();
  }

  console.log("CAD-2253 generic work item, approvals, and follow-up checks passed");
}

void main();
