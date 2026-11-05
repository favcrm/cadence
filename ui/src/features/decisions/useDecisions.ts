import { useEffect, useSyncExternalStore } from "react";
import { subscribeMasterChanged } from "../../lib/masterChanged";
import { onPrivateSessionReset } from "../../lib/privateSession";
import { failureReason, outcomeOf, parseStagedList, type StagedOutcome, type StagedWrite } from "./stagedWrite";

/**
 * CAD-2186: the owner's staged decisions as one shared store. The To do rail
 * and the sidebar's Home badge read the same list, so the count they show
 * can never disagree. The tenant Worker serves `/api/assist/actions` and the
 * owner-only approve/deny routes; every other board answers 404/501 (or
 * refuses a non-owner), and then the store is `unavailable` and adds nothing.
 * Nothing here invents a decision: the server owns the owner gate and
 * exactly-once.
 */

export type DecisionsPhase = "idle" | "loading" | "ready" | "unavailable";

/** What happened to one decision the owner just tried. */
export type Attempt =
  | { phase: "busy"; verb: Verb }
  /** The reply never arrived or was not understood: it may have gone through. */
  | { phase: "unconfirmed"; verb: Verb }
  /** The server refused the owner (403). */
  | { phase: "access" }
  /** It was decided, or is gone, by someone else. */
  | { phase: "elsewhere" };

export type Verb = "approve" | "deny";

export interface DecisionNotice {
  outcome: StagedOutcome | "elsewhere" | "still-waiting";
  title: string;
  /** English reason for a failed apply (translated where shown). */
  reason?: string;
}

export interface DecisionsState {
  phase: DecisionsPhase;
  rows: readonly StagedWrite[];
  /** Waiting for the owner, oldest first. */
  pending: readonly StagedWrite[];
  /** Decided, newest first. */
  decided: readonly StagedWrite[];
  attempts: ReadonlyMap<string, Attempt>;
  notice: DecisionNotice | null;
  /** A write that was approved and did not apply, not yet seen in Updates. */
  failedUnseen: boolean;
}

const EMPTY: DecisionsState = {
  phase: "idle",
  rows: [],
  pending: [],
  decided: [],
  attempts: new Map(),
  notice: null,
  failedUnseen: false,
};

let state: DecisionsState = EMPTY;
const listeners = new Set<() => void>();
const seenFailed = new Set<string>();

function publish(next: Omit<DecisionsState, "pending" | "decided" | "failedUnseen"> & Partial<Pick<DecisionsState, "failedUnseen">>) {
  const pending = next.rows.filter((r) => r.status === "pending").sort((a, b) => a.createdAt - b.createdAt);
  const decided = next.rows.filter((r) => r.status !== "pending").sort((a, b) => (b.decidedAt ?? b.createdAt) - (a.decidedAt ?? a.createdAt));
  const failedUnseen = decided.some((r) => outcomeOf(r) === "failed" && !seenFailed.has(r.id));
  state = { ...next, pending, decided, failedUnseen };
  for (const fn of listeners) fn();
}

const patch = (change: Partial<Pick<DecisionsState, "phase" | "rows" | "attempts" | "notice">>) =>
  publish({ phase: state.phase, rows: state.rows, attempts: state.attempts, notice: state.notice, ...change });

function setAttempt(id: string, attempt: Attempt | null) {
  const attempts = new Map(state.attempts);
  if (attempt === null) attempts.delete(id);
  else attempts.set(id, attempt);
  patch({ attempts });
}

// ---- reading ----

let generation = 0;
let retryTimer: ReturnType<typeof setTimeout> | null = null;
let holders = 0;
let stopMaster: (() => void) | null = null;
let stopPoll: (() => void) | null = null;
const POLL_MS = 30_000;

const UNAVAILABLE_STATUSES = new Set([401, 403, 404, 501]);

/** Read the list. Only the newest read may publish; returns the rows it published. */
export async function reloadDecisions(): Promise<readonly StagedWrite[] | null> {
  const mine = ++generation;
  if (retryTimer !== null) {
    clearTimeout(retryTimer);
    retryTimer = null;
  }
  try {
    const response = await fetch("/api/assist/actions", {
      credentials: "same-origin",
      headers: { accept: "application/json" },
    });
    if (mine !== generation) return null;
    if (UNAVAILABLE_STATUSES.has(response.status)) {
      patch({ phase: "unavailable", rows: [] });
      return null;
    }
    const body: unknown = await response.json().catch(() => null);
    if (mine !== generation) return null;
    const rows = response.ok ? parseStagedList(body) : null;
    if (rows === null) throw new Error("unreadable");
    patch({ phase: "ready", rows });
    return rows;
  } catch {
    if (mine !== generation) return null;
    // Keep the last good list; a failed refresh is not an empty queue. A
    // first read that failed retries on its own so a 5xx cannot hide the
    // surface until the page is reloaded.
    if (holders > 0 && retryTimer === null)
      retryTimer = setTimeout(() => {
        retryTimer = null;
        void reloadDecisions();
      }, 5_000);
    return null;
  }
}

function acquire() {
  holders += 1;
  if (holders > 1) return;
  if (state.phase === "idle") patch({ phase: "loading" });
  void reloadDecisions();
  // The Home thread reports every master-state move, including a staged
  // write arriving while the queue is empty: the only refresh for an idle
  // queue, with no timer to leak.
  stopMaster = subscribeMasterChanged(() => {
    if (state.phase === "ready") void reloadDecisions();
  });
  // Hosted boards have no stream that names a new staged write, so the list
  // is also re-read on a timer, on focus and when the tab becomes visible.
  // A failed re-read keeps the known list (see `reloadDecisions`).
  const again = () => {
    if (document.visibilityState === "hidden") return;
    if (state.phase === "unavailable") return;
    void reloadDecisions();
  };
  const timer = setInterval(again, POLL_MS);
  addEventListener("focus", again);
  document.addEventListener("visibilitychange", again);
  stopPoll = () => {
    clearInterval(timer);
    removeEventListener("focus", again);
    document.removeEventListener("visibilitychange", again);
  };
}

function release() {
  holders -= 1;
  if (holders > 0) return;
  holders = 0;
  generation += 1;
  stopMaster?.();
  stopMaster = null;
  stopPoll?.();
  stopPoll = null;
  if (retryTimer !== null) clearTimeout(retryTimer);
  retryTimer = null;
  // Not holding the list means not trusting it: the next holder reads afresh.
  state = EMPTY;
  for (const fn of listeners) fn();
}

// ---- deciding ----

export const currentDecisions = (): DecisionsState => state;

export type DecideResult = "decided" | "elsewhere" | "access" | "unconfirmed";

function failedOrDone(row: StagedWrite): DecisionNotice {
  const outcome = outcomeOf(row);
  return outcome === "failed" ? { outcome, title: row.title, reason: failureReason(row) } : { outcome, title: row.title };
}

/** Approve or deny one decision. Never retries; never submits twice. */
export async function decide(id: string, verb: Verb): Promise<DecideResult> {
  const current = state.rows.find((r) => r.id === id);
  // One decision at a time: a second click while one is in flight is dropped.
  if (current === undefined || current.status !== "pending") return "elsewhere";
  for (const attempt of state.attempts.values()) if (attempt.phase === "busy") return "unconfirmed";
  setAttempt(id, { phase: "busy", verb });
  let status = 0;
  try {
    const response = await fetch(`/api/assist/actions/${encodeURIComponent(id)}/${verb}`, {
      method: "POST",
      credentials: "same-origin",
      headers: { accept: "application/json", "content-type": "application/json" },
      body: "{}",
    });
    status = response.status;
  } catch {
    // The request may have reached the server: say so, do not resubmit.
    setAttempt(id, { phase: "unconfirmed", verb });
    return "unconfirmed";
  }
  if (status === 403 || status === 401) {
    setAttempt(id, { phase: "access" });
    return "access";
  }
  if (status === 409 || status === 404) {
    await reloadDecisions();
    setAttempt(id, { phase: "elsewhere" });
    return "elsewhere";
  }
  if (status < 200 || status >= 300) {
    setAttempt(id, { phase: "unconfirmed", verb });
    return "unconfirmed";
  }
  return settle(id, verb);
}

/** After a reply that says it worked, trust the list, not the reply. */
async function settle(id: string, verb: Verb): Promise<DecideResult> {
  const rows = await reloadDecisions();
  const row = rows?.find((r) => r.id === id);
  if (row === undefined) {
    // The list did not come back, or the row left it: we cannot say what happened.
    setAttempt(id, rows === null ? { phase: "unconfirmed", verb } : { phase: "elsewhere" });
    return rows === null ? "unconfirmed" : "elsewhere";
  }
  if (row.status === "pending") {
    setAttempt(id, { phase: "unconfirmed", verb });
    return "unconfirmed";
  }
  const attempts = new Map(state.attempts);
  attempts.delete(id);
  patch({ attempts, notice: failedOrDone(row) });
  return "decided";
}

/**
 * "Check again" after an unconfirmed decision: re-read, never resubmit. A
 * still-pending row means nothing was recorded, so the owner may decide again.
 */
export async function checkDecision(id: string): Promise<DecideResult> {
  const rows = await reloadDecisions();
  if (rows === null) return "unconfirmed";
  const row = rows.find((r) => r.id === id);
  if (row === undefined) {
    setAttempt(id, { phase: "elsewhere" });
    return "elsewhere";
  }
  if (row.status === "pending") {
    const attempts = new Map(state.attempts);
    attempts.delete(id);
    patch({ attempts, notice: { outcome: "still-waiting", title: row.title } });
    return "unconfirmed";
  }
  const attempts = new Map(state.attempts);
  attempts.delete(id);
  patch({ attempts, notice: failedOrDone(row) });
  return "decided";
}

export function clearNotice() {
  if (state.notice !== null) patch({ notice: null });
}

/** Updates was viewed: failed writes stop asking for attention. */
export function markFailedSeen() {
  let changed = false;
  for (const r of state.decided)
    if (outcomeOf(r) === "failed" && !seenFailed.has(r.id)) {
      seenFailed.add(r.id);
      changed = true;
    }
  if (changed) patch({});
}

export function resetDecisions() {
  generation += 1;
  seenFailed.clear();
  state = holders > 0 ? { ...EMPTY, phase: "loading" } : EMPTY;
  for (const fn of listeners) fn();
  if (holders > 0) void reloadDecisions();
}

onPrivateSessionReset(resetDecisions);

/**
 * Whether this viewer is asked for owner decisions at all: signed in, on a
 * hosted board, and not a member. One predicate for the badge and the rail.
 */
export function ownerDecisionsOn(meta: {
  signed_in?: boolean | null;
  hosted?: boolean | null;
  session?: { origin?: string; user?: { role?: string } | null } | null;
} | null | undefined): boolean {
  return meta?.signed_in === true && (meta.hosted === true || meta.session?.origin === "public") && meta.session?.user?.role !== "member";
}

/**
 * The shared decisions. With `enabled` false the caller holds nothing and
 * sees an empty store, so a board that has no such route is never asked.
 */
export function useDecisions(enabled: boolean): DecisionsState {
  useEffect(() => {
    if (!enabled) return;
    acquire();
    return release;
  }, [enabled]);
  const snapshot = useSyncExternalStore(
    (fn) => {
      listeners.add(fn);
      return () => {
        listeners.delete(fn);
      };
    },
    () => state,
  );
  return enabled ? snapshot : EMPTY;
}
