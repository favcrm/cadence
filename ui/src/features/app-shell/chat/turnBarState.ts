import type { MasterState } from "../../../lib/types";
import { stepSummary, type ThreadEntry } from "../../home/thread";

/** The turn bar's state, derived from the master state and this conversation's thread. */
export type TurnBarState =
  | { kind: "idle" }
  | { kind: "sending" }
  | { kind: "queued"; ahead: string; aheadConversation: string | null; cancelMessage: string | null }
  | { kind: "working"; step: string | null; since: number | null; stopMessage: string | null }
  | { kind: "writing"; since: number | null; stopMessage: string | null }
  | { kind: "approval" }
  | { kind: "failed"; reason: string; message: string | null }
  | { kind: "stopped" }
  | { kind: "done"; seconds: number | null };

export interface TurnBarInput {
  master: MasterState | null | undefined;
  /** This conversation's stored entries, oldest first. */
  entries: ThreadEntry[];
  /** Messages this surface sent that have no stored entry yet (not failed). */
  pendingMessages: string[];
  /** A permission or assistant operation waits on the operator now. */
  awaitingApproval: boolean;
  /** The stream is down: the bar shows the last known state, actions hidden. */
  reconnecting: boolean;
  /** Display name of a conversation id, when this surface knows it. */
  conversationName?: (id: string) => string | null;
  /** Epoch ms this surface opened the conversation; older results are history, not a live outcome. */
  liveSince?: number;
}

/** `turn_result` failure reason, shared with the thread view's rule. */
export function turnFailureReason(entry: ThreadEntry): string | null {
  const p = entry.payload;
  const error = typeof p?.error === "string" ? p.error.trim() : "";
  if (p?.status !== "failed" && p?.status !== "error" && !error) return null;
  return error || "Assistant could not finish this turn.";
}

/**
 * The bar's state for one conversation (`conversationId` null = Home).
 * Stop carries the running message only when that turn belongs to this
 * conversation. Cancel is offered only for a message THIS surface sent
 * whose stored entry is the operator's own (a daemon notice or an agent
 * send in the same thread is never cancellable). The daemon refuses a
 * stop whose message is no longer the running turn, so a stale click
 * never reaches a newer turn.
 */
export function turnBarState(input: TurnBarInput, conversationId: string | null): TurnBarState {
  const { master, entries, pendingMessages, awaitingApproval, reconnecting } = input;
  const turn = master?.turn ?? null;
  // Messages whose stored entry this surface's operator wrote (or that it
  // is still sending). Assistant turns, daemon notices and agent sends are
  // somebody else's to stop or cancel.
  const own = new Set<string>(pendingMessages);
  for (const e of entries) if (e.message && e.role === "operator") own.add(e.message);
  const running = turn && turn.state === "working" ? turn : null;
  const runningHere =
    running !== null &&
    ((running.conversation ?? null) === conversationId || own.has(running.message));

  if (runningHere && running) {
    if (awaitingApproval) return { kind: "approval" };
    const stopMessage = reconnecting ? null : running.message;
    // Writing: the turn's newest item here is its own assistant text.
    const last = entries.at(-1);
    if (last && last.kind === "assistant_text" && last.message === running.message) {
      return { kind: "writing", since: running.since ?? null, stopMessage };
    }
    return { kind: "working", step: liveStep(entries, running.message, running.summary), since: running.since ?? null, stopMessage };
  }

  // Queued: this conversation's own message waits in the master queue
  // (delivery order, `submitting` first). What is ahead is the running turn
  // when it is first in line, else the queued message just before ours.
  const queue = master?.queue ?? [];
  const ownIndex = queue.findIndex((q) => own.has(q.message));
  if (ownIndex >= 0) {
    const ahead =
      ownIndex > 0
        ? (queue[ownIndex - 1].conversation ?? null)
        : running
          ? (running.conversation ?? null)
          : undefined;
    return {
      kind: "queued",
      ahead: ahead === undefined ? "" : aheadLabel(ahead, conversationId, input.conversationName),
      aheadConversation: ahead ?? null,
      cancelMessage: reconnecting ? null : queue[ownIndex].message,
    };
  }

  if (pendingMessages.length > 0) return { kind: "sending" };

  // A finished turn shows only when its result arrived while this surface
  // was open: a reload does not replay the last historic outcome.
  const last = entries.at(-1);
  const arrived = last?.created ? Date.parse(last.created) : NaN;
  if (last && last.kind === "turn_result" && !(arrived < (input.liveSince ?? 0))) {
    const reason = turnFailureReason(last);
    if (reason !== null) return { kind: "failed", reason, message: last.message ?? null };
    if (last.payload?.status === "interrupted" || last.payload?.status === "stopped") return { kind: "stopped" };
    return { kind: "done", seconds: turnDuration(entries, last) };
  }
  return { kind: "idle" };
}

/**
 * The running turn's live step: its newest tool entry, else the request
 * summary. `master_state`'s `summary` is the original operator body, so a
 * tool frame alone would never change it without this.
 */
export function liveStep(entries: ThreadEntry[], message: string, fallback: string | null | undefined): string | null {
  for (let i = entries.length - 1; i >= 0; i--) {
    const e = entries[i];
    if (e.message !== message) continue;
    if (e.kind === "tool_call" || e.kind === "tool_result") {
      const text = stepSummary(e.text ?? "");
      if (text) return text;
    }
  }
  return fallback ?? null;
}

/** What is ahead of a queued message: Home, a known conversation name, or a generic name. */
export function aheadLabel(
  conversation: string | null,
  self: string | null,
  name?: (id: string) => string | null,
): string {
  if (conversation === null || conversation === "") return "Home";
  if (conversation === self) return "an earlier message here";
  return name?.(conversation) ?? "another conversation";
}

/** Seconds from the turn's operator message to its `turn_result`, or null. */
function turnDuration(entries: ThreadEntry[], result: ThreadEntry): number | null {
  const start = entries.find((e) => e.message === result.message && e.role === "operator");
  const a = start?.created ? Date.parse(start.created) : NaN;
  const b = result.created ? Date.parse(result.created) : NaN;
  if (Number.isNaN(a) || Number.isNaN(b)) return null;
  return Math.max(0, Math.round((b - a) / 1000));
}
