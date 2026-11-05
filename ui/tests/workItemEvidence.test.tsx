/**
 * CAD-1811 review regression: the shared work-item detail never offers
 * artifact authoring. Evidence is recorded-facts only; uploads stay on the
 * canonical issue page, so only My work's bounded comment/status writes are
 * reachable from the shared detail.
 */
import type { ResourceState } from "../src/lib/cache";
import type { IssueCard, IssueDetail, Project } from "../src/lib/types";
import type ProjectsOverviewType from "../src/features/projects/ProjectsOverview";

declare function require(name: string): any;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLDialogElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLSelectElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history"])
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"])
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
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
  const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
  const Detail = (require("../src/features/work-items/WorkItemDetail") as typeof import("../src/features/work-items/WorkItemDetail")).default;
  type DetailProps = import("../src/features/work-items/WorkItemDetail").WorkItemDetailProps;

  const detail: IssueDetail = {
    id: "D-1", project: "demo", title: "Ship the rail", status: "doing", status_source: "direct", priority: "P2", owner: "operator",
    blocked_by: [], relates: [], refs: [], paths: [], tags: [], container: false, ready: true, blocked: false,
    created: "2026-09-29T00:00:00Z", rev: "fnv1a:0123456789abcdef",
    checks: { done: 0, total: 1 },
    work: { type: "task", type_source: "implicit", weight: 1 },
    frontmatter: {}, body: "## Acceptance\n\n- [ ] one", path: "issue.md", comments: [], notes_chain: [], activity: [],
    artifacts: [{ name: "evidence.txt", size: 12 }],
    links: { children: [], blocked_by: [], blocks: [], relates: [], duplicates: [] },
  };
  const loaded: ResourceState<IssueDetail> = { data: detail, status: "ok", error: null, asOf: Date.now(), inFlight: false };
  const drafts = new Map();
  const myWork = (membership: "assigned" | "checking" | "unassigned"): DetailProps["myWork"] => ({
    owner: membership === "assigned" ? "operator" : "",
    assignment: null,
    identityKey: "operator",
    drafts,
    draftKey: `operator:D-1`,
    membership,
    onWrite: () => {},
    onError: () => {},
    writeBlock: null,
    onDraftChange: () => {},
  });

  async function render(props: Partial<DetailProps>) {
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    const readState: ResourceState<IssueDetail> = props.readState ?? loaded;
    await React.act(async () => root.render(
      React.createElement(LocaleProvider, null, React.createElement(Detail, {
        id: "D-1",
        origin: "issue",
        detail,
        readState,
        href: "/projects/demo/issues/D-1",
        onClose: () => {},
        readOnly: false,
        onOpen: () => {},
        ...props,
      })),
    ));
    await React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
    const buttons = () => Array.from(host.querySelectorAll("button")) as HTMLButtonElement[];
    const tab = (label: string) => buttons().find((b) => b.textContent?.trim() === label);
    const done = async () => { await React.act(async () => { root.unmount(); }); host.remove(); };
    return { host, tab, done };
  }

  const uploads = (host: ParentNode) =>
    host.querySelectorAll('input[type="file"]').length > 0 ||
    Array.from(host.querySelectorAll("button")).some((b) => b.textContent?.trim() === "Attach");

  const staleState: ResourceState<IssueDetail> = { data: detail, status: "stale", error: "offline", asOf: Date.now(), inFlight: false };

  for (const origin of ["home", "threads", "issue"] as const) {
    const view = await render({ origin });
    assert(!view.host.querySelector("textarea"), `${origin}: no comment box without the My work opt-in`);
    await React.act(() => view.tab("Evidence")?.click());
    assert(!uploads(view.host), `${origin}: the shared Evidence tab offers no upload`);
    assert(view.host.textContent?.includes("evidence.txt"), `${origin}: recorded evidence is still listed`);
    await view.done();
  }

  {
    const view = await render({ origin: "myWork", myWork: myWork("unassigned") });
    assert(!view.host.querySelector("textarea"), "myWork/unassigned: quick actions are off");
    await React.act(() => view.tab("Evidence")?.click());
    assert(!uploads(view.host), "myWork/unassigned: no upload");
    await view.done();
  }
  {
    const view = await render({ origin: "myWork", myWork: myWork("checking") });
    assert(view.host.textContent?.includes("Confirming this is still assigned to you"), "myWork/checking: actions are paused with a reason");
    await React.act(() => view.tab("Evidence")?.click());
    assert(!uploads(view.host), "myWork/checking: no upload");
    await view.done();
  }
  {
    const view = await render({ origin: "myWork", myWork: myWork("assigned"), readState: staleState });
    const textarea = view.host.querySelector("textarea") as HTMLTextAreaElement | null;
    assert(textarea?.disabled, "myWork/stale: the bounded comment write is disabled");
    await React.act(() => view.tab("Evidence")?.click());
    assert(!uploads(view.host), "myWork/stale: no upload");
    await view.done();
  }
  {
    const view = await render({ origin: "myWork", myWork: myWork("assigned") });
    const textarea = view.host.querySelector("textarea") as HTMLTextAreaElement | null;
    assert(textarea && !textarea.disabled, "myWork/assigned: the bounded comment write stays available");
    await React.act(() => view.tab("Evidence")?.click());
    assert(!uploads(view.host), "myWork/assigned: Evidence still offers no upload");
    await view.done();
  }

  {
    const business: IssueDetail = {
      ...detail,
      status: "done",
      checks: { done: 0, total: 0 },
      body: "## Acceptance\n- [x] Research drafted\n- [ ] Owner accepted",
      comments: [{ name: "comment", author: "member", at: "2026-09-29T00:00:00Z", body: "Research is ready for review." }],
      agent_run: { state: "working", agent_name: "Research agent", stop_owed: true },
      local_assignment: {
        version: "hosted-cadence-local-assignment-read.v1",
        source: "local_assignment_coordinator",
        scope: "participant",
        available: true,
        truncated: false,
        assignments: [{
          assignment_id: "private-assignment-id",
          state: "result",
          kind: "research",
          text: "Research draft submitted.",
          deliverable: { type: "artifact", ref: "evidence.txt" },
          deliverable_check: { found: true },
        }],
      },
      review_state: { artifacts: [
        { name: "evidence.txt", passed: true, reviews: [{ digest: "a".repeat(64), decision: "pass", reviewerName: "Editor" }] },
        { name: "draft.txt", passed: false, reviews: [] },
      ] },
    };
    const readState: ResourceState<IssueDetail> = { ...loaded, data: business };
    const view = await render({ detail: business, readState });
    const text = view.host.textContent ?? "";
    assert(text.includes("1/2 checks"), "loaded acceptance text wins over stale card counts");
    assert(text.includes("Started; completion not yet observed"), "a closed item still shows an unfinished hosted run");
    assert(text.includes("Stop not confirmed"), "an unconfirmed stop is visible");
    assert(text.includes("Result reported; acceptance not confirmed"), "a result is not represented as acceptance");
    assert(text.includes("Research draft submitted.") && text.includes("Available in Evidence"), "the participant can read the reported result and artifact location");
    assert(!text.includes("private-assignment-id"), "internal assignment identity is not displayed");
    assert(!view.tab("Code"), "repo-less business work does not show a developer-only Code tab");
    await React.act(() => view.tab("Approvals")?.click());
    assert(view.host.textContent?.includes("1 of 2 artifacts in this review store have a passing review."), "artifact review coverage is shown separately from acceptance");
    assert(view.host.textContent?.includes(`SHA-256 ${"a".repeat(64)}`), "artifact review shows its exact reviewed revision");
    assert(view.host.textContent?.includes("Editor") && view.host.textContent?.includes("No passing review recorded"), "individual review evidence and unreviewed artifacts stay distinct");
    assert(view.host.textContent?.includes("Review evidence is unavailable."), "missing report data is not presented as an empty review ledger");
    assert(view.host.textContent?.includes("Approval requirements and current decisions are unavailable here."), "missing approval data is not treated as permission to deliver");
    await React.act(() => view.tab("Activity")?.click());
    assert(view.host.textContent?.includes("Research is ready for review."), "separately returned hosted comments remain visible");
    await view.done();
  }
  {
    const software: IssueDetail = {
      ...detail,
      approvals: {
        version: "hosted-cadence-issue-approvals.v1",
        scope: "hosted-project-record",
        available: false,
        heads: [
          { head_sha: "a".repeat(40), repo: "example/project", pr: 42, state: "in-force", verified: false, recorded_via: "hosted-owner-login", source: "Owner decision", subject_id: "private-subject" },
          { head_sha: "b".repeat(40), repo: "example/project", pr: 42, state: "missing" },
          { head_sha: "c".repeat(40), repo: "example/project", pr: 42, state: "unavailable" },
        ],
      },
    };
    const view = await render({ detail: software, readState: { ...loaded, data: software } });
    assert(view.tab("Code"), "recorded PR approval evidence exposes the Code tab even when refs were not copied");
    await React.act(() => view.tab("Approvals")?.click());
    const text = view.host.textContent ?? "";
    assert(text.includes("Hosted approval in force for this recorded head") && text.includes("hosted-owner-login") && text.includes("Approval verification: unverified"), "the exact hosted state and its unverified assurance are visible");
    assert(text.includes("No approval record for this recorded head"), "an answered empty lookup differs from an unavailable lookup");
    assert(text.includes("Approval record unavailable for this recorded head"), "an unavailable lookup remains unknown");
    assert(text.includes("Current head, approval requirements, revocation and authority are not evaluated here."), "a record is not presented as current approval authority");
    assert(!text.includes("private-subject"), "approval subject identifiers are not exposed as names");
    await view.done();
  }
  {
    const unavailable: IssueDetail = {
      ...detail,
      body: "",
      acceptance: { available: false },
      agent_run: { state: "constructor" },
      local_assignment: { version: "hosted-cadence-local-assignment-read.v1", source: "local_assignment_coordinator", scope: "participant", available: false, assignments: [] },
    };
    const view = await render({ detail: unavailable, readState: { ...loaded, data: unavailable } });
    assert(view.host.textContent?.includes("Hosted run status is unavailable or not recorded."), "an invalid hosted state stays unknown");
    assert(view.host.textContent?.includes("Local assignment status is unavailable."), "a failed assignment read is not displayed as no assignments");
    assert(view.host.textContent?.includes("Acceptance count unavailable"), "uncounted acceptance is not presented as zero checks");
    await view.done();
  }

  {
    const ProjectsOverview = (require("../src/features/projects/ProjectsOverview") as { default: typeof ProjectsOverviewType }).default;
    const projection = {
      version: "hosted-cadence-local-assignment-read.v1",
      source: "local_assignment_coordinator",
      scope: "participant",
      available: true,
      truncated: false,
      assignments: [{ assignment_id: "assignment", state: "running" }],
    };
    const card: IssueCard = {
      ...detail,
      status: "done",
      counts: { comments: 0, artifacts: 0, refs: 0 },
      local_assignment: projection,
    };
    const project: Project = {
      key: "demo", prefix: "D", components: [], repos: [], issues: 1,
      local_assignment: { ...projection, live: 1, observed_at: "2026-09-29T00:00:00Z" },
    };
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    let opened = "";
    await React.act(async () => root.render(React.createElement(LocaleProvider, null,
      React.createElement(ProjectsOverview, {
        project: "demo", projects: [project],
        issues: { data: [card], status: "ok", error: null, asOf: Date.now(), inFlight: false },
        onOpenIssue: (id: string) => { opened = id; }, onRetry: () => {},
      }),
    )));
    assert(host.textContent?.includes("1 open local assignments visible to you"), "project count is explicitly participant-scoped");
    assert(host.textContent?.includes("Closed items with visible open assignments"), "a closed ticket with a recorded open assignment remains reconcilable");
    assert(host.textContent?.includes("Execution not verified"), "project assignment counts do not claim live execution");
    const reconcile = Array.from(host.querySelectorAll("button")).find((button) => button.textContent?.includes("D-1"));
    await React.act(() => reconcile?.click());
    assert(opened === "D-1", "the reconciliation entry opens the affected issue");
    await React.act(() => root.unmount());
    host.remove();
  }

  console.log("work-item evidence checks passed");
}

void main();
