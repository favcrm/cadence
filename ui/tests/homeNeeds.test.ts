import { ageLabel, homeNeeds } from "../src/features/home/needs";
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
  equal([needs[1].owner, needs[1].summary], ["w1", "Cost policy: 4x requests."], "asker and the master's summary");
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

console.log("home needs checks passed");
