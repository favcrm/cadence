import {
  appApprovalChip,
  addMissing,
  appHref,
  appNeeds,
  appPurpose,
  approvalPending,
  approveBlock,
  appFieldLabel,
  appWorkflowRow,
  connectionRows,
  distinctNote,
  distinctProblem,
  doctorFindings,
  filterCounts,
  needIssue,
  newRunHref,
  outboxHref,
  outputsOf,
  primaryAction,
  publishTarget,
  runFilter,
  runStages,
  runState,
  runStatus,
  runTitle,
  runsSummary,
  slugFromTopic,
  slugProblem,
  sourceLabel,
  startLabel,
  stepLabel,
  stepRows,
  teamFromLastRun,
  teamInputs,
  teamRole,
  unboundSlots,
  usedSlots,
} from "../src/features/apps/apps";
import type { HomeNeed } from "../src/features/home/needs";
import type { AppDetail, AppRow, AppRun, AppWorkflow } from "../src/lib/types";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

const row: AppRow = {
  project: "cadence",
  name: "studio",
  title: "Studio",
  version: "0.1.0",
  approval: "approved",
  connections: [{ slot: "publish", bound: "local" }],
  workflows: ["do-check"],
};

// Routes: the detail under /apps; a published item deep-links the Outbox.
equal(appHref("cadence", "studio"), "/apps/cadence/studio", "app detail href");
equal(outboxHref("ef-1"), "/outbox?item=ef-1", "outbox item href");
equal(outboxHref("a/b"), "/outbox?item=a%2Fb", "outbox item href encodes");

// Approval chips — the four states a row reports.
equal(appApprovalChip(row).text, "approved", "approved chip");
equal(appApprovalChip({ ...row, approval: "changed" }).text, "changed since approval", "changed chip");
equal(appApprovalChip({ ...row, approval: "unapproved" }).text, "unapproved", "unapproved chip");
equal(appApprovalChip({ ...row, approval: "unknown" }).text, "approval unknown", "unknown chip");
equal(appApprovalChip({ ...row, error: "broken" }).text, "broken", "error row chip");

// Approve is offered only while approval is pending and only to the operator.
equal(approvalPending(row), false, "approved is not pending");
equal(approvalPending({ ...row, approval: "changed" }), true, "changed is pending");
equal(approvalPending({ ...row, approval: "unapproved" }), true, "unapproved is pending");
equal(approvalPending({ ...row, error: "symlink" }), false, "a broken row has no approve");
const operator = { readOnly: false, operator: true };
equal(approveBlock({ ...row, approval: "unapproved" }, operator), null, "operator may approve");
equal(approveBlock({ ...row, approval: "unapproved" }, { readOnly: true, operator: true }) !== null, true, "read-only blocked");
equal(approveBlock({ ...row, approval: "unapproved" }, { readOnly: false, operator: false }) !== null, true, "unproven blocked");
equal(approveBlock(row, operator), null, "approved row: no button either way");

// Slots: unbound are the ones doctor flags.
equal(unboundSlots(row), [], "bound slot is not unbound");
equal(
  unboundSlots({ ...row, connections: [{ slot: "publish", bound: null }] }),
  ["publish"],
  "null bound is unbound",
);

// Source: a git install names url + pinned sha; a path install names the path.
equal(
  sourceLabel({ ...row, source: { kind: "git", url: "https://x/y", sha: "0123456789abcdef" } }),
  "git https://x/y @ 0123456789ab",
  "git source",
);
equal(sourceLabel({ ...row, source: { kind: "path", path: "apps/studio" } }), "path apps/studio", "path source");
equal(sourceLabel({ ...row, source: null }), null, "no source");

// Doctor findings: the slots that resolve are the connection rows' own
// (shown once, with their verification), so only what needs the
// operator lands here.
equal(doctorFindings(null), [], "no doctor row");
equal(
  doctorFindings({
    project: "cadence",
    app: "studio",
    slots_ok: [
      { slot: "publish", connection: "local" },
      { slot: "cdn", connection: "edge", verified: "sha256:abc" },
    ],
    unbound: ["logs"],
    unknown_connection: [{ slot: "cms", connection: "gone" }],
    stray_bindings: ["extra"],
  }).map((f) => f.text),
  [
    "cdn → edge — verification sha256:abc",
    "logs — unbound",
    "cms → gone — unknown connection",
    "extra — bound but not declared",
  ],
  "findings",
);
equal(
  connectionRows({
    project: "cadence",
    app: "studio",
    slots_ok: [{ slot: "publish", connection: "local" }],
  }).map((c) => c.text),
  ["publish → local"],
  "connection rows",
);

// The shared run form's row, built from an app detail's workflow summary
// (CAD-563): the whole-app approval is the gate, the app names the row,
// and a workflow that fails its checks carries its errors.
const detail: AppDetail = {
  ...row,
  name: "studio",
  workflows: undefined,
  approved: false,
  approval: "unapproved",
};
equal(
  appWorkflowRow("cadence", detail, {
    name: "studio/do-check",
    ok: true,
    title: "Do check",
    tickets: 2,
    inputs: [{ name: "title", ask: "What change?" }],
  }),
  {
    project: "cadence",
    name: "studio/do-check",
    app: "studio",
    title: "Do check",
    tickets: 2,
    inputs: [{ name: "title", ask: "What change?" }],
    approved: false,
  },
  "workflow row from the detail",
);
equal(
  appWorkflowRow("cadence", { ...detail, approved: true }, {
    name: "studio/do-check",
    ok: false,
    errors: ["ticket 2 has no acceptance"],
  }).error,
  "ticket 2 has no acceptance",
  "a failing workflow carries its errors",
);

// Runs: the plain-language state, in the order an operator meets it.
const run = (over: Partial<AppRun> = {}, plan: Partial<AppRun["plan"]> = {}): AppRun => ({
  epic: "D-1",
  title: "Do check",
  status: "backlog",
  workflow: "studio/do-check",
  plan: {
    state: "proposed",
    tickets: [
      { id: "D-2", title: "one", status: "backlog" },
      { id: "D-3", title: "two", status: "backlog" },
    ],
    progress: { done_weight: 0, total_weight: 6, ratio: 0, counts: {} },
    ...plan,
  },
  ...over,
});
const need = (over: Partial<HomeNeed>): HomeNeed =>
  ({
    key: "issue:D-2",
    kind: "plan",
    label: "plan",
    title: "t",
    owner: "master",
    age: 60,
    subject: { kind: "issue", id: "D-2" },
    summary: null,
    escalatedBy: null,
    action: { type: "plan", epic: "D-2" },
    ...over,
  }) as HomeNeed;

equal(runState(run(), []), { text: "waiting approval", cls: "bg-warn/10 text-warn", needsYou: true }, "proposed waits on the operator");
equal(
  runState(run({}, { state: "approved" }), []),
  { text: "running", cls: "bg-accent/15 text-accent", needsYou: false },
  "approved runs",
);
equal(
  runState(run({}, { state: "approved", progress: { done_weight: 6, total_weight: 6, ratio: 1, counts: {} } }), []),
  { text: "done", cls: "bg-ok/15 text-ok", needsYou: false },
  "every ticket done reads done",
);
equal(runState(run({ status: "done" }, { state: "approved" }), []).text, "done", "the rolled-up epic reads done");
equal(runState(run({}, { state: "rejected" }), []).text, "rejected", "rejected is not running");
// Needs you: a row about a ticket of the run (the question's action names
// the issue, not its `report` subject), or about the epic itself.
equal(
  runState(run({}, { state: "approved" }), [
    need({ kind: "question", subject: { kind: "report", id: "D-2/q.md" }, action: { type: "answer", issue: "D-2", report: "q.md", options: [], impact: null, body: null } }),
  ]).text,
  "needs you",
  "a question about a ticket is needs you",
);
equal(
  runState(run({}, { state: "approved" }), [need({ subject: { kind: "issue", id: "D-9" }, action: { type: "command", command: "cadence x" } })]).text,
  "running",
  "another issue's row is not this run's",
);
equal(
  runState(run({}, { state: "approved" }), [need({ kind: "merge_decision", action: { type: "merge", issue: "D-3", pr: null, sha: null, reviewer: null, verdict: null } })]).text,
  "needs you",
  "a merge decision on a ticket is needs you",
);
// needIssue reads the action first, then an issue subject; a report
// subject is never an issue id.
equal(needIssue(need({})), "D-2", "plan action's epic");
equal(
  needIssue(need({ kind: "blocked", subject: { kind: "issue", id: "D-7" }, action: { type: "command", command: "c" } })),
  "D-7",
  "issue subject",
);
equal(
  needIssue(need({ subject: { kind: "report", id: "D-2/q.md" }, action: { type: "command", command: "c" } })),
  null,
  "a report subject is not an issue",
);

// ---- CAD-563 r2: the plain-language layer ----

// The purpose line: the summary when declared, else the title.
equal(appPurpose({ name: "studio", title: "Content studio" }), "Content studio", "purpose falls back to title");
equal(
  appPurpose({ name: "studio", title: "Content studio", summary: "Get a post published." }),
  "Get a post published.",
  "purpose is the summary",
);
equal(newRunHref("cadence", "studio", "studio/do-check"), "/apps/cadence/studio?new=studio%2Fdo-check", "new-run href");

// The primary action names the workflow's human label.
const wf: AppWorkflow = {
  name: "studio/do-check",
  ok: true,
  title: "Do check: {{title}}",
  label: "New post",
  tickets: 2,
  inputs: [
    { name: "topic", ask: "What should the post be about?" },
    { name: "slug", ask: "Folder name" },
    { name: "writer", ask: "Who writes" },
    { name: "reviewer", ask: "Who reviews" },
  ],
  steps: [
    { title: "Brief: topic", agent: "writer", size: "S" },
    { title: "Review: topic", agent: "reviewer" },
  ],
  distinct: ["writer", "reviewer"],
  uses: ["publish"],
};
equal(primaryAction({ ...detail, workflows: [wf] })?.label, "New post", "action label from the workflow");
equal(
  primaryAction({ ...detail, workflows: [{ ...wf, label: null }] })?.label,
  "New run",
  "no label falls back",
);
equal(primaryAction({ ...detail, workflows: [] }), null, "no workflow, no action");
// CAD-571 N5: the action is the workflow the LIST CARD's primary names
// — even when the detail's workflow list is ordered the other way —
// so a multi-workflow app's card and page never open different runs.
const second: AppWorkflow = { ...wf, name: "studio/aa-second", label: "New alpha" };
equal(
  primaryAction({
    ...detail,
    primary: { workflow: "aa-second", label: "New alpha" },
    workflows: [wf, second],
  })?.wf.name,
  "studio/aa-second",
  "the named primary wins over list order",
);
equal(
  primaryAction({
    ...detail,
    primary: { workflow: "studio/aa-second", label: "New alpha" },
    workflows: [wf, second],
  })?.wf.name,
  "studio/aa-second",
  "an app-qualified primary also matches",
);
equal(
  primaryAction({ ...detail, primary: { workflow: "gone" }, workflows: [wf, second] })?.wf.name,
  "studio/do-check",
  "an unknown primary falls back to the first",
);

// Steps: the plain label is the part before the colon.
equal(stepLabel("Brief: my topic"), "Brief", "step label before the colon");
equal(stepLabel("Review the PR"), "Review the PR", "no colon keeps the title");

// The team comes from the last run's owners, mapped by step.
equal(teamInputs(wf), ["writer", "reviewer"], "team inputs are the steps' agents");
equal(
  teamFromLastRun(wf, [
    run({}, { tickets: [
      { id: "D-2", title: "Brief: old", status: "done", owner: "w-old" },
      { id: "D-3", title: "Review: old", status: "done", owner: "r-old" },
    ] }),
    run({ epic: "D-4" }, { tickets: [
      { id: "D-5", title: "Brief: new", status: "doing", owner: "w-new" },
      { id: "D-6", title: "Review: new", status: "backlog", owner: "r-new" },
    ] }),
  ]),
  { writer: "w-new", reviewer: "r-new" },
  "the last run's owners",
);
equal(teamFromLastRun(wf, []), {}, "no runs, no team");
// CAD-571 N6: the mapping is by the step's label, never the ticket's
// index — a workflow whose steps were reordered (or that gained one)
// still prefills the right agent for each role.
const reordered: AppWorkflow = {
  ...wf,
  steps: [
    { title: "Review: topic", agent: "reviewer" },
    { title: "Brief: topic", agent: "writer" },
  ],
};
equal(
  teamFromLastRun(reordered, [
    run({}, { tickets: [
      { id: "D-2", title: "Brief: old", status: "done", owner: "w-old" },
      { id: "D-3", title: "Review: old", status: "done", owner: "r-old" },
    ] }),
  ]),
  { writer: "w-old", reviewer: "r-old" },
  "labels, not positions",
);
const grew: AppWorkflow = {
  ...wf,
  steps: [
    { title: "Brief: topic", agent: "writer" },
    { title: "Review: topic", agent: "reviewer" },
    { title: "Publish: topic", agent: "publisher" },
  ],
  inputs: [...(wf.inputs ?? []), { name: "publisher", ask: "Who publishes" }],
};
equal(
  teamFromLastRun(grew, [
    run({}, { tickets: [
      { id: "D-2", title: "Brief: old", status: "done", owner: "w-old" },
      { id: "D-3", title: "Review: old", status: "done", owner: "r-old" },
    ] }),
  ]),
  { writer: "w-old", reviewer: "r-old" },
  "a new step takes nothing from an older run",
);
equal(
  teamFromLastRun(wf, [
    run({}, { tickets: [
      { id: "D-2", title: "Untitled work", status: "done", owner: "x" },
      { id: "D-3", title: "Brief: old", status: "done", owner: "w-old" },
    ] }),
  ]),
  { writer: "w-old" },
  "an unmapped ticket is skipped",
);

// The slug a topic suggests.
equal(slugFromTopic("Cadence vs Conductor!"), "cadence-vs-conductor", "slug from topic");
equal(slugFromTopic("  --  "), "", "punctuation only");
equal(slugFromTopic("x".repeat(80)).length, 60, "slug capped");

// Stage rows: the run's own tickets, each with its state — finished
// (ticked), the one in flight (highlighted), the rest waiting. The
// first open step is current when nothing is reported in flight yet.
const output = { effect_id: "ef-1", project: "cadence", title: "A post", published_at: "2026-09-26T01:00:00Z", runs: ["D-1"] };
const send = { effect_id: "ef-2", state: "waiting", title: "Another", runs: ["D-1"] };
equal(
  runStages(run({}, { tickets: [
    { id: "D-2", title: "Brief: t", status: "done" },
    { id: "D-3", title: "Review: t", status: "doing" },
    { id: "D-4", title: "Publish: t", status: "backlog" },
  ] })),
  [
    { label: "Brief", tone: "done" },
    { label: "Review", tone: "current" },
    { label: "Publish", tone: "waiting" },
  ],
  "a run in flight",
);
equal(
  runStages(run({}, { tickets: [
    { id: "D-2", title: "Brief: t", status: "done" },
    { id: "D-3", title: "Review: t", status: "ready" },
  ] })),
  [
    { label: "Brief", tone: "done" },
    { label: "Review", tone: "current" },
  ],
  "nothing in flight: the first open step is current",
);
equal(outputsOf(run(), [output], [send]), { items: [output], pending: [send] }, "outputs by run");
equal(outputsOf(run({ epic: "D-9" }), [output], [send]), { items: [], pending: [] }, "another run's outputs stay out");

// One status and action per card.
equal(runTitle("Blog post: Cadence vs Conductor: one team"), "Cadence vs Conductor: one team", "the topic without the prefix");
equal(runTitle("A post with no prefix"), "A post with no prefix", "no prefix, no change");
equal(
  runStatus(run(), [], { items: [output], pending: [] }),
  { text: "Published", cls: "bg-ok/15 text-ok", action: { label: "View", href: "/outbox?item=ef-1" } },
  "published",
);
equal(
  runStatus(run({}, { state: "approved" }), [], { items: [], pending: [send] }),
  { text: "Ready to publish", cls: "bg-warn/10 text-warn", action: { label: "Review & publish", href: "/" } },
  "a send waiting for the release",
);
equal(
  runStatus(run(), [], { items: [], pending: [] }),
  { text: "Needs you", cls: "bg-warn/10 text-warn", action: { label: "Approve plan", href: "/" } },
  "a plan waiting for approval",
);
equal(
  runStatus(run({}, { state: "approved", tickets: [{ id: "D-2", title: "Draft: t", status: "doing" }] }), [], { items: [], pending: [] }),
  { text: "Draft in progress…", cls: "bg-accent/15 text-accent" },
  "the step in flight",
);
equal(
  runStatus(run({}, { state: "approved", tickets: [{ id: "D-2", title: "Draft: t", status: "done" }] }), [], { items: [], pending: [] }),
  { text: "Done", cls: "bg-ok/15 text-ok" },
  "every step done",
);
equal(
  runStatus(run({}, { state: "rejected" }), [], { items: [], pending: [] }),
  { text: "Not approved", cls: "bg-fail/10 text-fail" },
  "a rejected plan",
);

// The drawer's plain words (r3).
equal(teamRole("strategist"), "Researcher", "strategist reads Researcher");
equal(teamRole("topic"), null, "the topic is not a role");
equal(appFieldLabel("slug", "Folder name under posts/ (lowercase, hyphens)"), "Folder", "the folder by its plain name");
equal(appFieldLabel("keyword", "Main search phrase"), "Keyword", "the keyword by its plain name");
equal(appFieldLabel("reviewer", "Agent that reviews"), "Reviewer", "the role, not the ask");
equal(appFieldLabel("topic", "What should the post be about?"), "What should the post be about?", "the ask stands");
equal(startLabel("New post"), "Start post", "the primary button");
equal(startLabel(null), "Start", "no label, plain Start");
equal(slugProblem("cadence-vs-conductor"), null, "a good folder name");
equal(slugProblem("") , null, "empty is the form's own message");
equal(slugProblem("My Post") !== null, true, "spaces and caps are refused inline");
equal(slugProblem("x".repeat(61)) !== null, true, "too long");
// CAD-571: the drawer's missing-inputs ask is plain words — the topic
// names the folder name too while the topic is empty, and the team is
// one clause of its own — never "missing required inputs: topic, slug".
equal(
  addMissing(["topic", "slug"], "topic", "slug"),
  "Add a topic.",
  "the topic covers the folder name it derives",
);
equal(addMissing(["slug"], "topic", "slug"), "Add a folder name.", "a cleared folder name names itself");
equal(
  addMissing(["topic", "slug", "strategist", "writer", "reviewer"], "topic", "slug"),
  "Add a topic and choose the team.",
  "the team is one clause",
);
equal(addMissing(["strategist"], "topic", "slug"), "Choose the team.", "team only");
equal(addMissing([], "topic", "slug"), "Fill in the run's inputs.", "nothing named");
equal(
  addMissing(["keyword"], "topic", "slug"),
  "Add a keyword.",
  "any other input by its name",
);
const trio = {
  ...wf,
  inputs: [...(wf.inputs ?? []), { name: "designer", ask: "Who draws" }],
  distinct: ["writer", "designer", "reviewer"],
};
equal(distinctProblem(trio, { writer: "a", designer: "b", reviewer: "c" }), null, "a distinct team passes");
equal(
  distinctProblem(trio, { writer: "a", designer: "a", reviewer: "c" }),
  "Reviewer can't be the writer or designer.",
  "a doubled team names the reviewer",
);
equal(
  distinctProblem(trio, { writer: "a", designer: "b", reviewer: "a" }),
  "Reviewer can't be the writer or designer.",
  "the reviewer doubling as the writer",
);

// Filters: published first, then needs you, else in progress.
equal(runFilter(run(), [], true), "published", "published");
equal(runFilter(run({}, { state: "approved" }), [need({ action: { type: "plan", epic: "D-1" } })], false), "needs_you", "a need about the epic");
equal(runFilter(run({}, { state: "approved" }), [], false), "in_progress", "in progress");
equal(runsSummary([], []), null, "no runs, no summary");
equal(
  runsSummary([run({}, { state: "approved" }), run({ epic: "D-4" }, { state: "proposed" })], []),
  "1 in progress · 1 needs you",
  "the card's live line",
);
// The Posts tab's counts follow the filters: published first, then the
// operator's rows, then the rest — the header and the chips agree.
equal(
  filterCounts(
    [
      run({}, { state: "approved" }),
      run({ epic: "D-4" }, { state: "proposed" }),
      run({ epic: "D-7" }, { state: "approved" }),
    ],
    [],
    [{ ...output, runs: ["D-7"] }],
    [],
  ),
  { in_progress: 1, needs_you: 1, published: 1 },
  "the Posts counts",
);

// Settings/How-it-works: publish target and the kept-apart rule.
equal(publishTarget({ ...detail, connections: [{ slot: "publish", bound: "local" }] }, "publish"), "Local outbox", "local reads as the outbox");
equal(publishTarget({ ...detail, connections: [{ slot: "publish", bound: null }] }, "publish"), "not connected yet", "unbound");
equal(usedSlots({ ...detail, workflows: [wf], connections: [{ slot: "publish", bound: "local" }, { slot: "unused", bound: "x" }] }), ["publish"], "only the slots the workflows use");
equal(distinctNote(wf), "Kept apart: writer, reviewer.", "the distinct rule in words");
equal(distinctNote({ ...wf, distinct: [] }), null, "no distinct rule");
equal(
  stepRows(wf, { writer: "cs-writer" }),
  [
    { label: "Brief", who: "cs-writer" },
    { label: "Review", who: null },
  ],
  "steps with the last run's team",
);

// The Needs-you strip: approval only when pending, releases and questions.
equal(appNeeds(detail, [], [], []), [{ kind: "approve", text: "Approve studio — its contents changed" }], "approval pending");
equal(appNeeds({ ...detail, approval: "approved" }, [], [], []), [], "approved: nothing to do");
equal(
  appNeeds({ ...detail, approval: "approved" }, [run({}, { state: "approved" })], [], [send]),
  [{ kind: "release", text: "Another — ready to publish", run: "D-1" }],
  "a staged send is the release row",
);
equal(
  appNeeds({ ...detail, approval: "approved" }, [run({}, { state: "approved" })], [
    need({ kind: "question", title: "D-2 question from w1", action: { type: "answer", issue: "D-2", report: "q.md", options: [], impact: null, body: null } }),
  ], []),
  [{ kind: "question", text: "D-2 question from w1", run: "D-1" }],
  "a question about a ticket",
);

console.log("apps checks passed");
