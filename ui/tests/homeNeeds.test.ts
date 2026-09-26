import {
  ageLabel,
  askDraft,
  homeNeeds,
  needGroups,
  readRailCollapsed,
  UNFENCE_CHOICES,
  writeRailCollapsed,
} from "../src/features/home/needs";
import type { NeedsMe } from "../src/lib/types";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

const row = (r: Partial<NeedsMe> & Record<string, unknown>): NeedsMe =>
  ({ kind: "x", title: "t", age: 60, project: "demo", command: "cadence x", ...r }) as NeedsMe;

// The rail: operator rows plus the master's plan and question kinds,
// plans first, then questions, oldest first within; team rows stay out.
{
  const needs = homeNeeds([
    row({ kind: "stalled", audience: "team", title: "team work" }),
    row({ kind: "approval", audience: "operator", title: "approve merge", age: 30, owner: "pm" }),
    row({
      kind: "question",
      audience: "operator",
      title: "D-2 question from w1 — blocks D-2",
      age: 120,
      subject: { kind: "report", id: "D-2/q.md" },
      question: { issue: "D-2", report: "q.md", agent: "w1", options: ["hourly", "every 15m", ""], impact: "blocks D-2", body: "which?" },
      summary: "Cost policy: 4x requests.",
      escalated_by: "master",
    }),
    row({
      kind: "plan",
      audience: "operator",
      title: "D-1 plan proposed by master",
      age: 600,
      subject: { kind: "issue", id: "D-1" },
      plan: { epic: "D-1", proposed_by: "master", tickets: ["D-3"] },
    }),
  ]);
  equal(needs.map((n) => n.kind), ["plan", "question", "approval"], "order and filter");
  equal(needs[0].action, { type: "plan", epic: "D-1" }, "plan → review the card");
  equal([needs[0].owner, ageLabel(needs[0].age)], ["master", "10m"], "plan owner and age");
  equal(
    needs[1].action,
    { type: "answer", issue: "D-2", report: "q.md", options: ["hourly", "every 15m"], impact: "blocks D-2", body: "which?" },
    "question → answer, empty options dropped",
  );
  equal(
    [needs[1].owner, needs[1].summary, needs[1].escalatedBy],
    ["w1", "Cost policy: 4x requests.", "master"],
    "asker, the master's summary, who escalated",
  );
  equal(needs[0].escalatedBy, null, "only questions are escalated");
  equal(needs[2].action, { type: "command", command: "cadence x" }, "others → the command");
  equal(needs[2].owner, "pm", "owner");
}

// Degrade, don't break: a plan/question row missing the newer fields
// falls back to its command; a bad epic id is not trusted.
{
  const needs = homeNeeds([
    row({ kind: "plan", title: "old plan row" }),
    row({ kind: "question", title: "q", question: { issue: "D-2" } }),
    row({ kind: "plan", title: "bad", plan: { epic: "../x" } }),
  ]);
  equal(needs.map((n) => n.action.type), ["command", "command", "command"], "fallbacks");
  equal(needs[0].owner, "—", "unknown owner");
  equal(homeNeeds(undefined), [], "no overview yet");
  const withSubject = homeNeeds([row({ kind: "plan", subject: { kind: "issue", id: "D-4" } })]);
  equal(withSubject[0].action, { type: "plan", epic: "D-4" }, "epic from the subject");
}

equal([ageLabel(5), ageLabel(7200), ageLabel(90000)], ["5s", "2h", "1d"], "age labels");



// CAD-431/433: a merge decision is a Merge button pinned to the reviewed
// head; a row without a valid issue falls back to its command.
{
  const needs = homeNeeds([
    row({
      kind: "merge_decision",
      audience: "operator",
      title: "merge? D-2 acme/app#7 by w1 — PASS by r1: ok (+12 −3, 2 files)",
      owner: "w1",
      subject: { kind: "issue", id: "D-2" },
      merge: {
        issue: "D-2",
        pr: "https://github.com/acme/app/pull/7",
        pr_ref: "acme/app#7",
        sha: "b".repeat(40),
        owner: "w1",
        reviewer: "r1",
        verdict_summary: "PASS: ok",
      },
    }),
    row({ kind: "merge_decision", audience: "operator", title: "odd", merge: { issue: "not an id" } }),
  ]);
  equal(needs[0].label, "merge", "chip label");
  equal(
    needs[0].action,
    { type: "merge", issue: "D-2", pr: "acme/app#7", sha: "b".repeat(40), reviewer: "r1", verdict: "PASS: ok" },
    "merge_decision → merge",
  );
  equal(needs[0].owner, "w1", "the worker owns it");
  equal(needs[1].action, { type: "command", command: "cadence x" }, "bad issue → command");
}

// CAD-574 — the rail groups: PRs by kind or pr-subject, Blocked→ready,
// Inboxes, Decisions, the rest Other; rows past 14d fold into Old (n)
// and leave their group. Tested through homeNeeds → needGroups.
{
  const needs = homeNeeds([
    row({ kind: "pr_no_verdict", audience: "operator", title: "PR #187 has had no verdict", age: 400, subject: { kind: "pr", id: "acme/app#187" } }),
    row({ kind: "approval", audience: "operator", title: "approve merge", age: 30 }),
    row({ kind: "blocked_ready", audience: "operator", title: "D-3 unblocked", age: 90 }),
    row({ kind: "inbox_stale", audience: "operator", title: "w1 inbox stale", age: 200 }),
    row({ kind: "a_new_server_kind", audience: "operator", title: "unknown kind", age: 10 }),
    row({ kind: "approval", audience: "operator", title: "ancient approval", age: 15 * 86400 }),
  ]);
  const { groups, old } = needGroups(needs);
  equal(
    groups.map((g) => g.key),
    ["decisions", "prs", "ready", "inboxes", "other"],
    "group order",
  );
  equal(groups[0].needs.map((n) => n.title), ["approve merge"], "decisions");
  equal(groups[1].needs.map((n) => n.title), ["PR #187 has had no verdict"], "prs");
  equal(groups[2].needs.map((n) => n.title), ["D-3 unblocked"], "blocked → ready");
  equal(groups[3].needs.map((n) => n.title), ["w1 inbox stale"], "inboxes");
  equal(groups[4].needs.map((n) => n.title), ["unknown kind"], "other");
  equal(old.map((n) => n.title), ["ancient approval"], "14d+ folds into Old");
}

// CAD-574 — Ask master: the draft is the row's ask and its subject goes
// as the structured ref; a subjectless row sends none. The draft never
// sends by itself — the composer only fills.
{
  const [need] = homeNeeds([
    row({
      kind: "pr_no_verdict",
      audience: "operator",
      title: "PR #187 has had no verdict for 24 days",
      age: 24 * 86400 - 1,
      subject: { kind: "pr", id: "acme/app#187" },
    }),
  ]);
  const d = askDraft(need);
  equal(d.text, "PR #187 has had no verdict for 24 days — what should we do?", "draft text");
  equal(d.refs, [{ kind: "pr", id: "acme/app#187" }], "subject → ref");

  const [plain] = homeNeeds([row({ kind: "approval", audience: "operator", title: "pick one", age: 1 })]);
  equal(askDraft(plain).refs, [], "no subject → no refs");
}

// CAD-574 — the collapsed rail persists per viewer; without storage it
// just defaults open (a node run has no localStorage).
{
  equal(readRailCollapsed(), false, "unset → open");
  writeRailCollapsed(true);
  equal(readRailCollapsed(), typeof globalThis.localStorage === "undefined" ? false : true, "write then read");
  writeRailCollapsed(false);
}

// CAD-574 r1 — Unfence's reconcile statuses: exactly the daemon's
// vocabulary, each with a one-line explanation, and none flagged as a
// default — the choice is the operator's.
{
  equal(
    UNFENCE_CHOICES.map((c) => c.status),
    ["interrupted", "completed", "failed"],
    "the whole reconcile vocabulary",
  );
  equal(
    UNFENCE_CHOICES.every((c) => c.blurb.length > 0 && c.blurb.length < 90),
    true,
    "every choice explains itself in one line",
  );
  equal(
    UNFENCE_CHOICES.some((c) => "default" in c),
    false,
    "no preselected default",
  );
}

console.log("home needs checks passed");
