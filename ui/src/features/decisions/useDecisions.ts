import { useEffect, useSyncExternalStore } from "react";
import { subscribeMasterChanged } from "../../lib/masterChanged";
import { onPrivateSessionReset, withPrivateOperation } from "../../lib/privateSession";
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

export type DecisionsPhase = "idle" | "loading" | "ready" | "error" | "unavailable";

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
export type DecideResult = "decided" | "elsewhere" | "access" | "unconfirmed";

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

function hasUnresolved(attempts: ReadonlyMap<string, Attempt>): boolean {
  for (const attempt of attempts.values())
    if (attempt.phase === "busy" || attempt.phase === "unconfirmed") return true;
  return false;
}

/** Each store owns one conversation; omission selects the owner's Home. */
export function createDecisionsStore(conversation: string | null, retain: (unresolved: boolean) => void = () => {}) {
  const selector = conversation === null ? "" : `?conversation=${encodeURIComponent(conversation)}`;
  let lifetime = 0;
  let state: DecisionsState = EMPTY;
  const listeners = new Set<() => void>();
  const seenFailed = new Set<string>();

  function publish(next: Omit<DecisionsState, "pending" | "decided" | "failedUnseen"> & Partial<Pick<DecisionsState, "failedUnseen">>) {
    const pending = next.rows.filter((r) => r.status === "pending").sort((a, b) => a.createdAt - b.createdAt);
    const decided = next.rows.filter((r) => r.status !== "pending").sort((a, b) => (b.decidedAt ?? b.createdAt) - (a.decidedAt ?? a.createdAt));
    const failedUnseen = decided.some((r) => outcomeOf(r) === "failed" && !seenFailed.has(r.id));
    state = { ...next, pending, decided, failedUnseen };
    retain(hasUnresolved(state.attempts));
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
  let stopReset: (() => void) | null = null;
  const POLL_MS = 30_000;
  const RETRY_MS = 5_000;
  const RETRY_MAX_MS = 60_000;
  let failures = 0;

  const deciding = (): boolean => [...state.attempts.values()].some((a) => a.phase === "busy");

  /** A read nobody asked for (timer, focus, stream, retry): never while a decision is in flight. */
  function backgroundReload() {
    if (deciding() || state.phase === "unavailable") return;
    void reloadDecisions();
  }

  const UNAVAILABLE_STATUSES = new Set([401, 403, 404, 501]);

  /** Read the list. Only the newest read may publish; returns the rows it published. */
  async function reloadDecisions(): Promise<readonly StagedWrite[] | null> {
    const mine = ++generation;
    if (retryTimer !== null) {
      clearTimeout(retryTimer);
      retryTimer = null;
    }
    try {
      const { status, ok, body } = await withPrivateOperation(async (operation) => {
        const response = await operation.fetch(`/api/assist/actions${selector}`, {
          credentials: "same-origin",
          headers: { accept: "application/json" },
        });
        const body: unknown = UNAVAILABLE_STATUSES.has(response.status) ? null : await response.json().catch(() => null);
        return { status: response.status, ok: response.ok, body };
      });
      if (mine !== generation) return null;
      if (UNAVAILABLE_STATUSES.has(status)) {
        patch({ phase: "unavailable", rows: [] });
        return null;
      }
      const rows = ok ? parseStagedList(body) : null;
      if (rows === null) throw new Error("unreadable");
      failures = 0;
      patch({ phase: "ready", rows });
      return rows;
    } catch {
      if (mine !== generation) return null;
      // Keep the last good list; a failed refresh is not an empty queue. A
      // first read that failed retries on its own so a 5xx cannot hide the
      // surface until the page is reloaded.
      // Back off, and do not poll a hidden tab: becoming visible reads at once.
      failures += 1;
      if (state.phase !== "ready") patch({ phase: "error" });
      if (holders > 0 && retryTimer === null)
        retryTimer = setTimeout(() => {
          retryTimer = null;
          if (document.visibilityState === "hidden") return;
          backgroundReload();
        }, Math.min(RETRY_MS * 2 ** (failures - 1), RETRY_MAX_MS));
      return null;
    }
  }

  function acquire() {
    holders += 1;
    if (holders > 1) return;
    if (stopReset === null) stopReset = onPrivateSessionReset(resetDecisions);
    if (state.phase === "idle") patch({ phase: "loading" });
    void reloadDecisions();
    // The Home thread reports every master-state move, including a staged
    // write arriving while the queue is empty: the only refresh for an idle
    // queue, with no timer to leak.
    stopMaster = subscribeMasterChanged(() => {
      if (state.phase === "ready") backgroundReload();
    });
    // Hosted boards have no stream that names a new staged write, so the list
    // is also re-read on a timer, on focus and when the tab becomes visible.
    // A failed re-read keeps the known list (see `reloadDecisions`).
    const again = () => {
      if (document.visibilityState === "hidden") return;
      if (state.phase === "unavailable") return;
      backgroundReload();
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
    const attempts = new Map<string, Attempt>();
    for (const [id, attempt] of state.attempts)
      if (attempt.phase === "busy" || attempt.phase === "unconfirmed") attempts.set(id, attempt);
    if (attempts.size === 0) {
      lifetime += 1;
      stopReset?.();
      stopReset = null;
    }
    stopMaster?.();
    stopMaster = null;
    stopPoll?.();
    stopPoll = null;
    clearTimeout(retryTimer ?? undefined);
    retryTimer = null;
    // Discard private rows, but keep an unresolved request across remounts.
    state = attempts.size > 0 ? { ...EMPTY, attempts } : EMPTY;
    retain(attempts.size > 0);
    for (const fn of listeners) fn();
  }

  // ---- deciding ----

  const currentDecisions = (): DecisionsState => state;

  function failedOrDone(row: StagedWrite): DecisionNotice {
    const outcome = outcomeOf(row);
    return outcome === "failed" ? { outcome, title: row.title, reason: failureReason(row) } : { outcome, title: row.title };
  }

  /** Approve or deny one decision. Never retries; never submits twice. */
  async function decide(id: string, verb: Verb): Promise<DecideResult> {
    const epoch = lifetime;
    const current = state.rows.find((r) => r.id === id);
    // One decision at a time: a second click while one is in flight is dropped.
    if (current === undefined || current.status !== "pending") return "elsewhere";
    for (const attempt of state.attempts.values()) if (attempt.phase === "busy") return "unconfirmed";
    if (state.attempts.get(id)?.phase === "unconfirmed") return "unconfirmed";
    setAttempt(id, { phase: "busy", verb });
    let status = 0;
    try {
      status = await withPrivateOperation(async (operation) => {
        const response = await operation.fetch(`/api/assist/actions/${encodeURIComponent(id)}/${verb}${selector}`, {
          method: "POST",
          credentials: "same-origin",
          headers: { accept: "application/json", "content-type": "application/json" },
          body: "{}",
        });
        return response.status;
      });
    } catch {
      if (epoch !== lifetime) return "unconfirmed";
      // The request may have reached the server: say so, do not resubmit.
      setAttempt(id, { phase: "unconfirmed", verb });
      return "unconfirmed";
    }
    if (epoch !== lifetime) return "unconfirmed";
    if (holders === 0) {
      // A reply without a visible list is not a confirmed outcome.
      setAttempt(id, { phase: "unconfirmed", verb });
      return "unconfirmed";
    }
    if (status === 403 || status === 401) {
      setAttempt(id, { phase: "access" });
      return "access";
    }
    if (status === 409 || status === 404) {
      await reloadDecisions();
      if (epoch !== lifetime) return "unconfirmed";
      setAttempt(id, { phase: "elsewhere" });
      return "elsewhere";
    }
    if (status < 200 || status >= 300) {
      setAttempt(id, { phase: "unconfirmed", verb });
      return "unconfirmed";
    }
    return settle(id, verb, epoch);
  }

  /** After a reply that says it worked, trust the list, not the reply. */
  async function settle(id: string, verb: Verb, epoch: number): Promise<DecideResult> {
    // If this read was superseded, the list that superseded it is as good.
    const read = await reloadDecisions();
    if (epoch !== lifetime) return "unconfirmed";
    const rows = read ?? (state.phase === "ready" ? state.rows : null);
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
  async function checkDecision(id: string): Promise<DecideResult> {
    const epoch = lifetime;
    const rows = await reloadDecisions();
    if (epoch !== lifetime) return "unconfirmed";
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

  function clearNotice() {
    if (state.notice !== null) patch({ notice: null });
  }

  /** Updates was viewed: failed writes stop asking for attention. */
  function markFailedSeen() {
    let changed = false;
    for (const r of state.decided)
      if (outcomeOf(r) === "failed" && !seenFailed.has(r.id)) {
        seenFailed.add(r.id);
        changed = true;
      }
    if (changed) patch({});
  }

  function resetDecisions() {
    lifetime += 1;
    generation += 1;
    seenFailed.clear();
    state = holders > 0 ? { ...EMPTY, phase: "loading" } : EMPTY;
    retain(false);
    if (holders === 0) {
      stopReset?.();
      stopReset = null;
    }
    for (const fn of listeners) fn();
    if (holders > 0) void reloadDecisions();
  }

  return {
    conversation,
    acquire, release, currentDecisions, reloadDecisions, backgroundReload, decide, checkDecision,
    clearNotice, markFailedSeen, resetDecisions,
    subscribe(listener: () => void) {
      listeners.add(listener);
      return () => { listeners.delete(listener); };
    },
  };
}

export type DecisionsStore = ReturnType<typeof createDecisionsStore>;
export const homeDecisions = createDecisionsStore(null);

// Retain only conversations with an unresolved decision, never their old rows.
const unresolvedConversations = new Map<string, DecisionsStore>();

export function conversationDecisions(conversation: string): DecisionsStore {
  const existing = unresolvedConversations.get(conversation);
  if (existing) return existing;
  let store: DecisionsStore;
  store = createDecisionsStore(conversation, (unresolved) => {
    if (unresolved) unresolvedConversations.set(conversation, store);
    else if (unresolvedConversations.get(conversation) === store) unresolvedConversations.delete(conversation);
  });
  return store;
}

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
export function useDecisions(enabled: boolean, store: DecisionsStore = homeDecisions): DecisionsState {
  useEffect(() => {
    if (!enabled) return;
    store.acquire();
    return store.release;
  }, [enabled, store]);
  const snapshot = useSyncExternalStore(store.subscribe, store.currentDecisions);
  return enabled ? snapshot : EMPTY;
}
