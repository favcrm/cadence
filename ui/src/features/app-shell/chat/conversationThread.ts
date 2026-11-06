import { useEffect } from "react";
import type { Resource } from "../../../lib/cache";
import { api } from "../../../lib/api";
import { streamInto, type SseErrorState } from "../../../lib/sse";
import { resources } from "../../../lib/resources";
import type { MasterState } from "../../../lib/types";
import { privateReadsAllowed, privateSessionGeneration } from "../../../lib/privateSession";
import { EMPTY_THREAD, PAGE, applyEarlier, asEntry, fetchEarlier, mergePage, pageCloudWithheld, reduceFrame, resumeSeq, threadItems, type PageReader, type ThreadEntry, type ThreadPageLike, type ThreadState } from "../../home/thread";
import { parseTicketProposal, type TicketProposal } from "../../agents/ticketProposal";

/**
 * The one conversation thread engine Home and every app pane share
 * (CAD-1700): the live entry stream with resume, and older-page paging.
 * Each surface keeps only what is genuinely its own (Home's dock and
 * plan cards; the pane's picker, directives and visit fences) and its
 * scroll-follow machine, which already runs on the shared
 * `scrollFollow` predicates.
 */

/**
 * Stream `url` into `store` while `enabled`. Resumes after the store's
 * newest seq (never a replay); on reconnect `streamInto` resumes with
 * `?after=` and the reducer dedupes by seq. The server's first `master`
 * snapshot on every connection makes the bar live before replayed entries;
 * TCP open alone cannot enable actions against a cached previous turn.
 */
/** A thread stream's link. `connecting` is the first attempt of this
 *  subscription: it is not live yet, but nothing has gone wrong either. */
export type StreamLink = "live" | "connecting" | SseErrorState;

/** How long a first attempt may stay silent before it counts as an outage: a
 *  stream that hangs must not read as healthy forever. */
const FIRST_CONNECT_GRACE_MS = 5000;
const OUTCOME_READ_TIMEOUT_MS = 10_000;

/** A joined outcome is a separate observation from the immutable thread row. */
function joinedOutcome(entry: ThreadEntry): { ticket: string; run: NonNullable<TicketProposal["run"]> } | null {
  const proposal = parseTicketProposal(entry.payload);
  return proposal?.ticket && proposal.run ? { ticket: proposal.ticket, run: proposal.run } : null;
}

function outcomeUnavailable(entry: ThreadEntry): ThreadEntry {
  if (entry.proposalOutcomeUnavailable) return entry;
  const payload = { ...entry.payload };
  delete payload.ticket;
  delete payload.run;
  return { ...entry, payload, proposalOutcomeUnavailable: true };
}

function mergeOutcome(previous: ThreadEntry, observed: ThreadEntry): ThreadEntry {
  const next = joinedOutcome(observed);
  const current = joinedOutcome(previous);
  if (!next) return current || previous.proposalOutcomeUnavailable ? outcomeUnavailable(previous) : previous;
  if (!previous.proposalOutcomeUnavailable && current?.ticket === next.ticket &&
      current.run.state === next.run.state && current.run.reason === next.run.reason && current.run.agent === next.run.agent) return previous;
  return {
    ...previous,
    payload: { ...previous.payload, ticket: next.ticket, run: next.run },
    proposalOutcomeUnavailable: false,
  };
}

function sameOutcome(left: ThreadEntry, right: ThreadEntry): boolean {
  if (!!left.proposalOutcomeUnavailable !== !!right.proposalOutcomeUnavailable) return false;
  const a = joinedOutcome(left);
  const b = joinedOutcome(right);
  return a === null ? b === null : b !== null && a.ticket === b.ticket &&
    a.run.state === b.run.state && a.run.reason === b.run.reason && a.run.agent === b.run.agent;
}

type ProposalRefresh = {
  changes: Map<number, { before: ThreadEntry; after: ThreadEntry }>;
  withheld: string | null;
};

/** Re-read held proposal rows without changing the transcript cursor or drafts. */
async function refreshProposalOutcomes(state: ThreadState, reader: Pick<PageReader, "before">, conversation?: string): Promise<ProposalRefresh> {
  const pending = new Map<number, ThreadEntry>();
  const changes: ProposalRefresh["changes"] = new Map();
  let withheld: string | null = null;
  for (const entry of state.entries) {
    if (entry.kind !== "ticket_proposal" || typeof entry.payload?.id !== "string") continue;
    if (Number.isSafeInteger(entry.seq) && entry.seq < Number.MAX_SAFE_INTEGER) pending.set(entry.seq, entry);
    else if (joinedOutcome(entry)) changes.set(entry.seq, { before: entry, after: outcomeUnavailable(entry) });
  }
  const descending = [...pending.keys()].sort((a, b) => b - a);
  for (const seq of descending) {
    const target = pending.get(seq);
    if (!target) continue;
    try {
      const page = await reader.before(seq + 1, PAGE);
      const thread = page.thread && typeof page.thread === "object" ? page.thread as Record<string, unknown> : null;
      if (!Array.isArray(page.entries) ||
          ((conversation ?? state.conversation) && typeof thread?.id === "string" && thread.id !== (conversation ?? state.conversation))) throw new Error("Proposal read unavailable");
      withheld = pageCloudWithheld(page) ?? withheld;
      for (const raw of page.entries) {
        const observed = asEntry(raw);
        const previous = observed && pending.get(observed.seq);
        if (!observed || !previous || observed.kind !== "ticket_proposal" ||
            observed.payload?.id !== previous.payload?.id) continue;
        const next = mergeOutcome(previous, observed);
        if (next !== previous) changes.set(previous.seq, { before: previous, after: next });
        pending.delete(previous.seq);
      }
      // A legacy or filtered page may not contain the requested row.
      if (pending.has(seq)) {
        if (joinedOutcome(target) || target.proposalOutcomeUnavailable) changes.set(seq, { before: target, after: outcomeUnavailable(target) });
        pending.delete(seq);
      }
    } catch {
      for (const [pendingSeq, entry] of pending) {
        if (joinedOutcome(entry) || entry.proposalOutcomeUnavailable) changes.set(pendingSeq, { before: entry, after: outcomeUnavailable(entry) });
      }
      break;
    }
  }
  return { changes, withheld };
}

export function useThreadStream(
  store: Resource<ThreadState>,
  url: string | null,
  enabled: boolean,
  onLink: (state: StreamLink) => void,
  onSnapshot?: () => void,
  graceMs: number = FIRST_CONNECT_GRACE_MS,
): void {
  useEffect(() => {
    if (!enabled || url === null) {
      onLink("live");
      return;
    }
    onLink("connecting");
    let active = true;
    // Until this subscription has been live or has failed, an open is still
    // its first attempt, not a reconnect.
    let settled = false;
    const grace = setTimeout(() => {
      if (settled) return;
      settled = true;
      onLink("reconnecting");
    }, graceMs);
    const sub = streamInto(store, (data, event) => {
      if (event.type === "master") {
        const snapshot = JSON.parse(event.data) as MasterState;
        if (Object.hasOwn(snapshot, "actor_identity")) {
          const conversation = snapshot.actor_identity?.conversation_id;
          if (typeof conversation !== "string" || conversation === "" || conversation !== data?.conversation) {
            onLink("reconnecting");
            return data ?? EMPTY_THREAD;
          }
          // A connection snapshot can arrive after a newer identity GET.
          // Reconcile through the canonical reader instead of rolling back
          // its opaque revision with an older streamed profile.
          void resources.masterConversation(conversation).refresh();
          if (resources.masterThread.get().data?.conversation === conversation) {
            resources.masterState.write(() => snapshot);
          }
        } else {
          resources.masterState.write(() => snapshot);
        }
        onSnapshot?.();
        settled = true;
        clearTimeout(grace);
        onLink("live");
        return data ?? EMPTY_THREAD;
      }
      const next = reduceFrame(data, event);
      if (data?.cloudWithheld != null && next.cloudWithheld == null) {
        // A fresh tail can restore rows below the stream's resume cursor.
        // Defer until streamInto has published `next` to the resource.
        queueMicrotask(() => {
          if (active && store.get().data?.cloudRefreshNeeded) void store.invalidate();
        });
      }
      return next;
    }, {
      url,
      events: ["master", "entry", "cloud_withheld"],
      // Resume from what the daemon has SCANNED, not from the newest held
      // entry: a projection can hide rows past the last kept seq, and
      // resuming below them would re-request the same hidden page on every
      // (re)connect.
      lastEventId: String(resumeSeq(store.get().data) ?? 0),
      onOpen: () => onLink(settled ? "reconnecting" : "connecting"),
      onError: (state) => {
        settled = true;
        clearTimeout(grace);
        onLink(state);
        if (state === "stopped") void store.refresh();
      },
    });
    return () => { active = false; clearTimeout(grace); sub.close(); };
    // Both callbacks are stable at the call sites; the stream must not
    // reopen when a parent re-renders.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [store, url, enabled]);
  useEffect(() => {
    if (!enabled || url === null) return;
    const source = new URL(url, window.location.href);
    const parts = source.pathname.split("/");
    if (source.origin !== window.location.origin || parts.length !== 5 ||
        parts[1] !== "api" || parts[2] !== "threads" || parts[4] !== "stream") return;
    if (source.searchParams.getAll("conversation").length > 1 ||
        [...source.searchParams.keys()].some((key) => key !== "conversation")) return;
    let alias: string;
    try { alias = decodeURIComponent(parts[3]); } catch { return; }
    if (alias === "") return;
    const conversation = source.searchParams.get("conversation") ?? undefined;
    let request: AbortController | null = null;
    const reader = { before: async (before: number, limit: number) => {
      const controller = new AbortController();
      request = controller;
      const timeout = setTimeout(() => controller.abort(), OUTCOME_READ_TIMEOUT_MS);
      try {
        return await api.thread(alias, { before, limit, conversation }, controller.signal);
      } finally {
        clearTimeout(timeout);
        if (request === controller) request = null;
      }
    } };
    let active = true;
    let inFlight = false;
    let rerun = false;
    const refresh = () => {
      if (!active || document.visibilityState !== "visible" || !privateReadsAllowed()) return;
      if (inFlight) { rerun = true; return; }
      const state = store.get().data;
      if (!state?.entries.some((entry) => entry.kind === "ticket_proposal")) return;
      const generation = privateSessionGeneration();
      inFlight = true;
      void (async () => {
        try {
          const result = await refreshProposalOutcomes(state, reader, conversation);
          if (!active || !privateReadsAllowed() || generation !== privateSessionGeneration()) return;
          const current = store.get().data;
          if (!current || current.conversation !== state.conversation) return;
          if (current.cloudRevision !== state.cloudRevision) { rerun = true; return; }
          const thread = current.conversation ? { id: current.conversation } : null;
          const base = result.withheld === null ? current :
            mergePage(current, { thread, entries: [], cloud_withheld: result.withheld });
          let changed = base !== current;
          const entries = base.entries.map((entry) => {
            const change = result.changes.get(entry.seq);
            if (!change || entry.kind !== "ticket_proposal" || entry.payload?.id !== change.before.payload?.id) return entry;
            if (!sameOutcome(entry, change.before)) { rerun = true; return entry; }
            const next = change.after.proposalOutcomeUnavailable ? outcomeUnavailable(entry) : mergeOutcome(entry, change.after);
            if (next !== entry) changed = true;
            return next;
          });
          if (changed) store.mutate(() => ({ ...base, entries }));
        } finally {
          inFlight = false;
          if (rerun) { rerun = false; refresh(); }
        }
      })();
    };
    const timer = setInterval(refresh, 30_000);
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refresh);
    return () => {
      active = false;
      request?.abort();
      clearInterval(timer);
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", refresh);
    };
  }, [store, url, enabled]);
}

/** What one "Load earlier" press does. */
export type EarlierStep =
  /** Items already held are hidden by the window: widen it, no fetch. */
  | { kind: "widen" }
  /** Nothing older exists: the store is marked complete. */
  | { kind: "complete" }
  /** An older page is on its way; merge it with `land`. */
  | { kind: "fetch"; page: Promise<ThreadPageLike>; land: (older: ThreadPageLike) => void };

/**
 * One "Load earlier" step for `store` showing `limit` items: widen the
 * window over items already held, else fetch the page before the oldest
 * held entry. The caller owns its window size, loading flag and any
 * visit fencing; `land` merges the page into its own store regardless.
 */
export function earlierStep(store: Resource<ThreadState>, reader: PageReader, limit: number): EarlierStep | null {
  const data = store.get().data;
  const session = privateSessionGeneration();
  if (!data) return null;
  if (threadItems(data).length > limit) return { kind: "widen" };
  const page = fetchEarlier(reader, data);
  if (!page) {
    store.write((cur) => ({ ...(cur ?? data), moreBefore: false }));
    return { kind: "complete" };
  }
  return { kind: "fetch", page, land: (older) => {
    if (session !== privateSessionGeneration()) return;
    store.write((cur) => {
      if (!cur || cur.conversation !== data.conversation) return cur ?? EMPTY_THREAD;
      // Stream status newer than this read wins. Ordinary older history
      // still merges; a stale clean page cannot restore cloud access.
      if (cur.cloudRevision !== data.cloudRevision) {
        const scoped = { ...older };
        if (cur.cloudWithheld == null) delete scoped.cloud_withheld;
        else scoped.cloud_withheld = cur.cloudWithheld;
        return applyEarlier(cur, scoped);
      }
      return applyEarlier(cur, older);
    });
  } };
}
