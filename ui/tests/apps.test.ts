import {
  appApprovalChip,
  appHref,
  approvalPending,
  approveBlock,
  appWorkflowRow,
  doctorFindings,
  needIssue,
  outboxHref,
  runState,
  sourceLabel,
  unboundSlots,
} from "../src/features/apps/apps";
import type { HomeNeed } from "../src/features/home/needs";
import type { AppDetail, AppRow, AppRun } from "../src/lib/types";

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

// Doctor findings flatten to labeled lines.
equal(doctorFindings(null), [], "no doctor row");
equal(
  doctorFindings({
    project: "cadence",
    app: "studio",
    slots_ok: [{ slot: "publish", connection: "local" }],
    unbound: ["logs"],
    unknown_connection: [{ slot: "cdn", connection: "gone" }],
    stray_bindings: ["extra"],
  }).map((f) => f.text),
  [
    "publish → local",
    "logs — unbound",
    "cdn → gone — unknown connection",
    "extra — bound but not declared",
  ],
  "findings",
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

console.log("apps checks passed");
