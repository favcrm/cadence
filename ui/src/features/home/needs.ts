import type { NeedsMe, ThreadRef } from "../../lib/types";

/**
 * The Home rail's "Needs you" (CAD-328), built from the overview's
 * `needs_me`. Rows the server resolved to the operator, plus the two
 * kinds the master adds (CAD-339, src/overview.rs): `plan` (a plan
 * awaiting approval, carrying `plan`) and `question` (an open question
 * the master escalated, carrying `question`, the master's `summary` and
 * `escalated_by`), and the worker loop's `merge_decision` (CAD-431,
 * carrying `merge`: the PR, the reviewed head and the PASS behind it),
 * which the rail offers as a Merge button. Rows are read through this
 * adapter, so a missing or malformed field degrades the row — a plan
 * without a valid epic falls back to its command — instead of breaking
 * the rail. Tested in tests/homeNeeds.test.ts.
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
  | {
      type: "merge";
      issue: string;
      /** `owner/repo#N`, else the PR link. */
      pr: string | null;
      /** The reviewed head the merge is pinned to. */
      sha: string | null;
      reviewer: string | null;
      verdict: string | null;
    }
  | { type: "command"; command: string }
  | {
      type: "permission";
      id: string;
      argv: string;
      cwd: string;
      reason: string;
      risk: string;
      /** A narrowed prefix the operator can always-allow, when one is safe. */
      prefix: string[] | null;
    };

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
  /** What the row is about (`issue`, `report`, …), when the server names
   *  one — the app page matches its runs by this (CAD-563). */
  subject: { kind: string; id: string } | null;
  /** The master's summary of an escalated question. */
  summary: string | null;
  /** Who escalated the question to the operator (the master). */
  escalatedBy: string | null;
  /** The row's fallback command — kept for the `…` menu's Copy (CAD-574). */
  command: string;
  /** Where the row points — a PR/issue link the server sent (Open). */
  link: string | null;
  /** The subject when it is an issue — Open lands on its page. */
  issue: string | null;
  /** The subject when it is an agent — Resume/Unfence act on it. */
  agent: string | null;
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
  master_permission: "permission",
  merge: "merge",
  merge_decision: "merge",
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
    merge?: {
      issue?: unknown;
      pr?: unknown;
      pr_ref?: unknown;
      sha?: unknown;
      owner?: unknown;
      reviewer?: unknown;
      verdict_summary?: unknown;
    };
    permission?: {
      id?: unknown;
      command?: unknown;
      cwd?: unknown;
      reason?: unknown;
      risk?: unknown;
      prefix?: unknown;
    };
    reason?: unknown;
  };
  const key = row.subject ? `${row.subject.kind}:${row.subject.id}` : `${row.kind}:${index}`;
  const base = {
    key,
    kind: row.kind,
    label: LABEL[row.kind] ?? row.kind.replace(/_/g, " "),
    title: row.title,
    age: row.age,
    subject: row.subject ?? null,
    summary: null as string | null,
    escalatedBy: null as string | null,
    command: row.command,
    link: str(row.link),
    issue: row.subject?.kind === "issue" ? row.subject.id : null,
    agent: row.subject?.kind === "agent" ? row.subject.id : null,
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
      escalatedBy: str(extra.escalated_by),
      action:
        issue && report && ID.test(issue)
          ? { type: "answer", issue, report, options, impact: str(q.impact), body: str(q.body) }
          : command,
    };
  }
  if (row.kind === "master_permission") {
    const p = extra.permission ?? {};
    const id = str(p.id);
    const prefix = Array.isArray(p.prefix) ? p.prefix.filter((x): x is string => !!str(x)) : [];
    return {
      ...base,
      owner: "master",
      summary: str(p.reason) ?? str(extra.reason),
      action: id
        ? {
            type: "permission",
            id,
            argv: str(p.command) ?? row.command,
            cwd: str(p.cwd) ?? "",
            reason: str(p.reason) ?? str(extra.reason) ?? "",
            risk: str(p.risk) ?? "medium",
            prefix: prefix.length > 0 ? prefix : null,
          }
        : command,
    };
  }
  if (row.kind === "merge_decision") {
    const m = extra.merge ?? {};
    const issue = str(m.issue) ?? (row.subject?.kind === "issue" ? row.subject.id : null);
    return {
      ...base,
      owner: str(m.owner) ?? row.owner ?? "—",
      action:
        issue && ID.test(issue)
          ? {
              type: "merge",
              issue,
              pr: str(m.pr_ref) ?? str(m.pr),
              sha: str(m.sha),
              reviewer: str(m.reviewer),
              verdict: str(m.verdict_summary),
            }
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

// ---- CAD-574: grouping, aging, Ask-master drafts, rail state ----

/** The rail's groups, in render order. */
export type NeedGroupKey = "decisions" | "prs" | "ready" | "inboxes" | "other";

export const NEED_GROUP_LABEL: Record<NeedGroupKey, string> = {
  decisions: "Decisions",
  prs: "PRs",
  ready: "Blocked → ready",
  inboxes: "Inboxes",
  other: "Other",
};

const GROUP_ORDER: readonly NeedGroupKey[] = ["decisions", "prs", "ready", "inboxes", "other"];

/** Kinds about a PR or its delivery — the PRs group. A row whose subject
 *  is a `pr` lands here whatever its kind. */
const PR_KINDS: ReadonlySet<string> = new Set([
  "merge_decision",
  "review_escalated",
  "review_unstaffed",
  "review_no_pr",
  "auto_merge_on",
  "delivery_unreadable",
  "delivery_stalled",
  "pr_no_verdict",
  "delivery_sync",
  "merge",
]);

/** Kinds that are the operator's call on an agent or a ticket. */
const DECISION_KINDS: ReadonlySet<string> = new Set([
  "plan",
  "question",
  "approval",
  "approval_menu",
  "fenced",
  "blocked",
  "stopped",
  "next_action",
  "merged_not_done",
  "effect_reconcile",
  "effect_unverified",
  "master_permission",
]);

/** One need → its group. A PR subject or kind is a PR row; a row's own
 *  kind then decides; anything unclassified is Other — a new server
 *  kind degrades to Other instead of disappearing. */
export function needGroup(need: HomeNeed): NeedGroupKey {
  if (need.subject?.kind === "pr" || PR_KINDS.has(need.kind)) return "prs";
  if (need.kind === "blocked_ready") return "ready";
  if (need.kind.startsWith("inbox")) return "inboxes";
  if (DECISION_KINDS.has(need.kind)) return "decisions";
  return "other";
}

/** Rows older than this collapse into the `Old (n)` group (brief §3). */
export const OLD_AFTER_SECS = 14 * 86400;

export interface NeedGroups {
  groups: { key: NeedGroupKey; label: string; needs: HomeNeed[] }[];
  /** Every row aged past `OLD_AFTER_SECS`, collapsed as `Old (n)`. */
  old: HomeNeed[];
}

/** Split the sorted rail into its groups plus the old bucket; a group's
 *  rows keep the rail's priority-then-age order. */
export function needGroups(needs: HomeNeed[]): NeedGroups {
  const old = needs.filter((n) => n.age >= OLD_AFTER_SECS);
  const fresh = needs.filter((n) => n.age < OLD_AFTER_SECS);
  return {
    groups: GROUP_ORDER.map((key) => ({
      key,
      label: NEED_GROUP_LABEL[key],
      needs: fresh.filter((n) => needGroup(n) === key),
    })).filter((g) => g.needs.length > 0),
    old,
  };
}

/** What "Ask master" seeds: the row's title as the draft's ask and its
 *  subject as the `refs` the send attaches. Never auto-sends — the
 *  operator reviews and presses Enter. */
export function askDraft(need: HomeNeed): { text: string; refs: ThreadRef[] } {
  const text = `${need.title} — what should we do?`;
  const refs = need.subject ? [{ kind: need.subject.kind, id: need.subject.id }] : [];
  return { text, refs };
}

/**
 * The reconcile statuses `agent unfence` accepts (CAD-574 r1). The
 * rail's Unfence asks which one the fenced turns settled to — the list
 * is the whole vocabulary and nothing is a default: reconciling
 * unknowns is the operator's call, said out loud, never a hidden one.
 */
export interface UnfenceChoice {
  status: "interrupted" | "completed" | "failed";
  /** One line on what the status records about the unknown turns. */
  blurb: string;
}
export const UNFENCE_CHOICES: readonly UnfenceChoice[] = [
  { status: "interrupted", blurb: "the turn was cut off mid-work — resume picks it up" },
  { status: "completed", blurb: "the turn finished its work — record it done" },
  { status: "failed", blurb: "the turn died — record it failed" },
];

/** The collapsed rail's persistence (per viewer; brief §1). */
const RAIL_KEY = "cadence:needs-rail";

export function readRailCollapsed(): boolean {
  try {
    return globalThis.localStorage?.getItem(RAIL_KEY) === "collapsed";
  } catch {
    return false;
  }
}

export function writeRailCollapsed(collapsed: boolean): void {
  try {
    globalThis.localStorage?.setItem(RAIL_KEY, collapsed ? "collapsed" : "open");
  } catch {
    /* a viewer without storage just loses the preference */
  }
}
