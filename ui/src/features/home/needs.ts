import type { NeedsMe } from "../../lib/types";

/**
 * The Home rail's "Needs you" (CAD-328), built from the overview's
 * `needs_me`. Rows the server resolved to the operator, plus the two
 * kinds the master adds (CAD-339): `plan` (a plan awaiting approval,
 * carrying `plan`) and `question` (an open question the master
 * escalated, carrying `question` and its `summary`). Everything the
 * newer daemon adds is read through this adapter, so a missing field
 * degrades the row — a plan without its epic falls back to the
 * command — instead of breaking the rail. Tested in
 * tests/homeNeeds.test.ts.
 */

export type NeedAction =
  | { type: "plan"; epic: string }
  | {
      type: "answer";
      issue: string;
      report: string;
      options: string[];
      impact: string | null;
      body: string | null;
    }
  | { type: "command"; command: string };

export interface HomeNeed {
  key: string;
  kind: string;
  /** Short chip label. */
  label: string;
  title: string;
  /** Who the item belongs to / who is waiting. */
  owner: string;
  /** Seconds waiting. */
  age: number;
  /** The master's summary of an escalated question. */
  summary: string | null;
  action: NeedAction;
}

/** A row the operator must act on. */
function forOperator(row: NeedsMe): boolean {
  return row.kind === "plan" || row.kind === "question" || row.audience === "operator";
}

function str(v: unknown): string | null {
  return typeof v === "string" && v.trim() !== "" ? v : null;
}

const ID = /^[A-Z][A-Z0-9]{0,9}-\d+$/;

const LABEL: Record<string, string> = {
  plan: "plan",
  question: "question",
  approval: "approval",
  merge: "merge",
};

/** One needs row → a rail item. */
export function homeNeed(row: NeedsMe, index = 0): HomeNeed {
  const extra = row as NeedsMe & {
    plan?: { epic?: unknown; proposed_by?: unknown };
    question?: {
      issue?: unknown;
      report?: unknown;
      agent?: unknown;
      options?: unknown;
      impact?: unknown;
      body?: unknown;
    };
    summary?: unknown;
    escalated_by?: unknown;
  };
  const key = row.subject ? `${row.subject.kind}:${row.subject.id}` : `${row.kind}:${index}`;
  const base = {
    key,
    kind: row.kind,
    label: LABEL[row.kind] ?? row.kind.replace(/_/g, " "),
    title: row.title,
    age: row.age,
    summary: null as string | null,
  };
  const command: NeedAction = { type: "command", command: row.command };
  if (row.kind === "plan") {
    const epic = str(extra.plan?.epic) ?? (row.subject?.kind === "issue" ? row.subject.id : null);
    return {
      ...base,
      owner: str(extra.plan?.proposed_by) ?? row.owner ?? "—",
      action: epic && ID.test(epic) ? { type: "plan", epic } : command,
    };
  }
  if (row.kind === "question") {
    const q = extra.question ?? {};
    const issue = str(q.issue);
    const report = str(q.report);
    const options = Array.isArray(q.options) ? q.options.filter((o): o is string => !!str(o)) : [];
    return {
      ...base,
      owner: str(q.agent) ?? row.owner ?? "—",
      summary: str(extra.summary),
      action:
        issue && report && ID.test(issue)
          ? { type: "answer", issue, report, options, impact: str(q.impact), body: str(q.body) }
          : command,
    };
  }
  return { ...base, owner: row.owner ?? "operator", action: command };
}

/** The rail: operator rows, plans and questions first, oldest first within. */
export function homeNeeds(rows: NeedsMe[] | null | undefined): HomeNeed[] {
  const rank = (n: HomeNeed) => (n.kind === "plan" ? 0 : n.kind === "question" ? 1 : 2);
  return (rows ?? [])
    .map((row, i) => [row, i] as const)
    .filter(([row]) => forOperator(row))
    .map(([row, i]) => homeNeed(row, i))
    .sort((a, b) => rank(a) - rank(b) || b.age - a.age);
}

/** `74` → `1m`, `7200` → `2h`. */
export function ageLabel(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}
