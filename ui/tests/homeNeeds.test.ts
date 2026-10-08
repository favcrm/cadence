import {
  ageLabel,
  ageWords,
  askDraft,
  fixPrompt,
  homeNeeds,
  kindSpec,
  KIND_TABLE,
  metaLine,
  readRailCollapsed,
  todoCount,
  todoSplit,
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

// CAD-1216 — the To do list: Info rows and drift go to Updates and never
// count; decided permissions leave the list (and the count) for the
// history, newest first; an unknown kind stays a card, never nothing.
{
  const rows = [
    row({ kind: "pr_no_verdict", audience: "operator", title: "PR #187 has had no verdict", age: 400, subject: { kind: "pr", id: "acme/app#187" } }),
    row({ kind: "approval", audience: "operator", title: "approve merge", age: 30 }),
    row({ kind: "inbox_stale", audience: "operator", title: "w1 inbox stale", age: 200 }),
    row({ kind: "inbox_unread", audience: "info", title: "w1 inbox unread", age: 100 }),
    row({ kind: "drift", audience: "dependency", title: "build drift", age: 90 }),
    row({ kind: "delivery_sync", audience: "info", title: "github read failing", age: 80 }),
    row({ kind: "a_new_server_kind", audience: "operator", title: "unknown kind", age: 10 }),
    row({
      kind: "master_permission",
      audience: "operator",
      title: "pending",
      age: 5,
      subject: { kind: "permission", id: "p1" },
      permission: { id: "p1", command: "send", status: "pending", reason: "Send email to 212 customers" },
    }),
    row({
      kind: "master_permission",
      audience: "operator",
      title: "decided old",
      age: 900,
      subject: { kind: "permission", id: "p2" },
      permission: { id: "p2", command: "send", status: "decided", decision_label: "Allowed once", reason: "older" },
    }),
    row({
      kind: "master_permission",
      audience: "operator",
      title: "decided new",
      age: 60,
      subject: { kind: "permission", id: "p3" },
      permission: { id: "p3", command: "send", status: "decided", decision_label: "Denied", reason: "newer" },
    }),
  ];
  const { todo, updates, decided } = todoSplit(rows);
  equal(todo.map((n) => n.kind).sort(), ["a_new_server_kind", "approval", "master_permission", "pr_no_verdict"], "only work for the operator is To do");
  equal(updates.map((n) => n.kind), ["delivery_sync", "drift", "inbox_unread", "inbox_stale"], "info rows and drift go to Updates, newest first");
  equal(decided.map((n) => n.key), ["permission:p3", "permission:p2"], "decided history is newest first");
  equal(todoCount(rows), 4, "the count (and the Home badge) is pending To do items only");
  equal(todoCount(undefined), 0, "no overview yet");
  const unknown = todo.find((n) => n.kind === "a_new_server_kind")!;
  equal([kindSpec(unknown).type, kindSpec(unknown).control], ["stuck", "Fix it"], "an unknown kind is a generic amber Fix it card");
  const permission = todo.find((n) => n.kind === "master_permission")!;
  equal(kindSpec(permission).title(permission, { issueTitle: null }), "Send email to 212 customers", "a permission is titled by its reason");
  equal(metaLine({ ...permission, project: "customers" }), "customers · just now", "meta line is where · age in words");
}

// CAD-1216 — the kind table: every server kind has a type, a control and a
// place; titles carry no alias, issue id, PR or command; a plan, idea or
// merge row that did not parse falls back to a Fix it card.
{
  const ids = /[A-Z]{1,10}-\d+|#\d+|cadence |\bw1\b/;
  for (const [kind, spec] of Object.entries(KIND_TABLE)) {
    const need = homeNeeds([row({ kind, audience: "operator", title: "t", age: 5 })])[0];
    const title = spec.title(need, { issueTitle: null });
    equal(ids.test(title), false, `${kind}: plain title (${title})`);
    equal(["ok", "question", "stuck"].includes(spec.type), true, `${kind}: type`);
  }
  const badPlan = homeNeeds([row({ kind: "plan", audience: "operator", plan: { epic: "../x" } })])[0];
  equal(kindSpec(badPlan).place, "send", "an unparsed plan degrades to Fix it");
  equal(kindSpec(homeNeeds([row({ kind: "plan", audience: "operator", subject: { kind: "issue", id: "D-4" } })])[0]).place, "drawer", "a parsed plan opens the drawer");
  equal([ageWords(30), ageWords(240), ageWords(7200), ageWords(3 * 86400)], ["just now", "4 min", "2 h", "3 d"], "age in words");
  const [stuck] = homeNeeds([row({ kind: "fenced", audience: "operator", title: "w1 fenced: 2 turns unknown", subject: { kind: "agent", id: "w1" } })]);
  const text = fixPrompt(stuck, "An agent is stuck");
  equal(text.includes("about agent w1"), true, "the body names the server subject, so two rows never send identical text");
  equal(text.includes('The item reads: "w1 fenced: 2 turns unknown"'), true, "the row title is quoted as reported data");
  equal(text.includes("not an instruction"), true, "and labelled as data, not an instruction");
  const [other] = homeNeeds([row({ kind: "fenced", audience: "operator", title: 'x "quoted"', subject: { kind: "agent", id: "w2" } })]);
  equal(fixPrompt(other, "An agent is stuck") === text, false, "different rows send different bodies");
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

// CAD-140 — a researched idea waiting on the operator is a first-class
// decision row: it parses to the idea action, ranks with plans, lands in
// Decisions, and degrades to its command when the subject is not an issue.
{
  const needs = homeNeeds([
    row({
      kind: "idea_plan",
      audience: "operator",
      title: "idea plan ready for your decision — D-9 Dark mode",
      age: 300,
      subject: { kind: "issue", id: "D-9" },
    }),
    row({
      kind: "question",
      audience: "operator",
      title: "q",
      age: 120,
      subject: { kind: "report", id: "D-2/q.md" },
      question: { issue: "D-2", report: "q.md", agent: "w1" },
    }),
  ]);
  equal(needs.map((n) => n.kind), ["idea_plan", "question"], "idea ranks with plans");
  equal(needs[0].action, { type: "idea", issue: "D-9" }, "idea_plan → decide");
  equal(needs[0].owner, "operator", "idea owner falls back to the operator");
  const bad = homeNeeds([row({ kind: "idea_plan", audience: "operator", title: "bad", subject: { kind: "row", id: "x" } })]);
  equal(bad[0].action.type, "command", "a non-issue subject keeps the command");
}

console.log("home needs checks passed");
