import type { AssignmentContext, MyWorkIssue, WriteResp } from "./api";
import type { IssueCard, IssueDetail } from "./types";
import type { HomeNeed } from "../features/home/needs";

/**
 * CAD-1811 (first slice): one issue-backed work item, opened from Home, My
 * work and Threads. This module is the presentation model over the sources
 * the board already reads — `IssueDetail`, a `MyWorkIssue` row and an
 * issue-backed `HomeNeed`. It adds no store and no fabricated field: every
 * getter answers from an authorized payload, and a missing value stays
 * missing so the detail can label it.
 */
export type WorkItemOrigin = "home" | "myWork" | "threads" | "issue";

export type WorkItemTabId = "overview" | "code" | "evidence" | "approvals" | "activity" | "audit";

/** One selected work item: the issue id plus where it was opened from. */
export interface WorkItemRef {
  id: string;
  origin: WorkItemOrigin;
}

/** Which surface opened the detail; closing returns to it. */
export function workItemOriginLabel(origin: WorkItemOrigin): string {
  switch (origin) {
    case "home": return "From Home";
    case "myWork": return "From My work";
    case "threads": return "From Threads";
    default: return "From the board";
  }
}

/** Unsaved comment/status input the App keeps per issue+identity across
 *  close/reopen. `status` is the pending selection — only an explicit
 *  Save commits it; the field is `null` once saved. */
export interface MyWorkDraft {
  comment: string;
  status: string | null;
  /** Shared submit guard survives closing/reopening the same issue mid-request. */
  pending?: { token: symbol; kind: "comment" | "status" };
}

/** The App's read of whether this issue is still personally assigned.
 *  `checking` covers loading/stale/inFlight/failed — unknown is not a grant. */
export type MyWorkMembership = "checking" | "assigned" | "unassigned";

/**
 * Opt-in My work actions for the shared detail. Absent ⇒ the read-only
 * detail. The App owns `drafts` (a stable map cleared on
 * `onPrivateSessionReset`); `draftKey` is `identityKey` + issue id.
 */
export interface MyWorkWorkItemProps {
  /** The verified My work owner — "" while membership is `checking`. */
  owner: string;
  /** This issue's recorded handoff, or null when provenance is unavailable. */
  assignment: AssignmentContext | null;
  /** Verified session identity the drafts are keyed to. */
  identityKey: string;
  /** App-owned draft map — mutate in place, never replace. */
  drafts: Map<string, MyWorkDraft>;
  /** `identityKey` + issue id for this mounted item. */
  draftKey: string;
  membership: MyWorkMembership;
  onWrite: (resp: WriteResp, verb: string) => void;
  onError: (error: unknown, verb: string) => void;
  writeBlock: string | null;
  /** Notify other mounted views only when a shared submission settles/starts. */
  onDraftChange: () => void;
}

/**
 * A Home decision (plan, idea or merge) hosted in the shared detail. The
 * rail builds it with the reading queue as it stood when the detail
 * opened, so "n of m" never recounts mid-run and the decision routes keep
 * their own guards. `next` marks the card decided and hands back the next
 * reading item, if any.
 */
export interface HomeDecisionOpen {
  need: HomeNeed;
  index: number;
  total: number;
  /**
   * The identity of this opening (a new value on every open and every queue
   * advance). A late reply is fenced to its own instance, not to a subject
   * that may have been reopened, re-headed or replaced.
   */
  token: number;
  /** The reviewed head a merge decision is pinned to, when it names one. */
  reviewHead: string | null;
  next: (text: string) => HomeDecisionOpen | null;
  /** Hand the item to Master as a composer draft; the detail then closes. */
  ask: (lead: string) => void;
}

let decisionTokenSeq = 0;

/** Allocate the identity of one decision opening (or queue advance). */
export function nextDecisionToken(): number {
  decisionTokenSeq += 1;
  return decisionTokenSeq;
}

/** The reviewed head a decision is pinned to, when it names one. */
export function decisionReviewHead(need: HomeNeed): string | null {
  return need.action.type === "merge" && need.action.sha ? need.action.sha : null;
}

/**
 * What a completed Home decision does to the host's selection. `done.next`
 * records the decision on its own queue either way; only a decision that is
 * still selected may advance the shared detail or close it. A late reply for
 * a decision the operator already dismissed (or replaced with another item)
 * is recorded and then ignored, so it can never close or replace the newer
 * selection.
 */
export type DecisionDone =
  | { kind: "ignored" }
  | { kind: "advanced"; next: HomeDecisionOpen }
  | { kind: "closed" };

/** What the host is showing right now, as the fence sees it. */
export interface DecisionSelection {
  decision: HomeDecisionOpen | null;
  issueId: string | null;
  origin: string;
}

export function applyDecisionDone(
  done: HomeDecisionOpen,
  text: string,
  selection: DecisionSelection,
): DecisionDone {
  const next = done.next(text);
  const current = selection.decision;
  // Only the exact instance that opened may close or advance the detail:
  // same opening token, same issue, same origin and same reviewed head.
  if (current === null || current.token !== done.token) return { kind: "ignored" };
  if (selection.origin !== "home" || selection.issueId !== homeNeedIssueId(done.need)) return { kind: "ignored" };
  if (current.reviewHead !== done.reviewHead) return { kind: "ignored" };
  return next === null ? { kind: "closed" } : { kind: "advanced", next };
}

/** The issue behind a Home reading item, or null when it has none. */
export function homeNeedIssueId(need: HomeNeed): string | null {
  const action = need.action;
  if (action.type === "plan") return action.epic || null;
  if (action.type === "idea" || action.type === "merge") return action.issue || null;
  return null;
}

/**
 * The recorded facts one detail renders. Every field is read, never
 * inferred; `recorded` says whether an authorized issue payload backs
 * them (a My work row alone is a summary, not the detail).
 */
export interface WorkItemFacts {
  id: string;
  project: string | null;
  title: string | null;
  status: string | null;
  priority: string | null;
  owner: string | null;
  parent: string | null;
  component: string | null;
  type: string | null;
  size: string | null;
  milestone: string | null;
  blocked: boolean;
  blockedReason: string | null;
  checks: { done: number; total: number };
  comments: number;
  recorded: boolean;
}

/** Merge the issue detail with the row that opened it; the detail wins. */
export function workItemFacts(
  detail: IssueDetail | null,
  card: MyWorkIssue | IssueCard | null,
): WorkItemFacts {
  const work = detail?.work ?? ("work" in (card ?? {}) ? (card as IssueCard).work : null);
  return {
    id: detail?.id ?? card?.id ?? "",
    project: detail?.project ?? card?.project ?? null,
    title: detail?.title ?? card?.title ?? null,
    status: detail?.status ?? card?.status ?? null,
    priority: detail?.priority ?? card?.priority ?? null,
    owner: detail?.owner ?? null,
    parent: detail?.parent ?? null,
    component: detail?.component ?? null,
    type: work?.type ?? null,
    size: work?.size ?? null,
    milestone: work?.milestone ?? null,
    blocked: detail?.blocked ?? card?.blocked ?? false,
    blockedReason: detail?.blocked_reason ?? card?.blocked_reason ?? null,
    checks: detail?.checks ?? card?.checks ?? { done: 0, total: 0 },
    comments: detail ? detail.comments.length : card?.counts.comments ?? 0,
    recorded: detail !== null,
  };
}

/** Dev work shows Code. Epics are planning records; a missing read is not dev. */
export function isDevWork(detail: IssueDetail | null): boolean {
  const type = detail?.work?.type;
  return type !== undefined && type !== "epic";
}

/** The shared detail's tabs, in the accepted order. */
export function workItemTabs(detail: IssueDetail | null): WorkItemTabId[] {
  const tabs: WorkItemTabId[] = ["overview"];
  if (isDevWork(detail)) tabs.push("code");
  tabs.push("evidence", "approvals", "activity", "audit");
  return tabs;
}

/** A plain left click opens the in-page detail; modified clicks keep the link. */
export function opensWorkItem(event: {
  button: number;
  metaKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
}): boolean {
  return event.button === 0 && !event.metaKey && !event.ctrlKey && !event.shiftKey && !event.altKey;
}
