import { useRef } from "react";
import type { PendingMessage, ThreadEntry } from "./thread";

/**
 * The send whose stored entry has landed before the state can say what became
 * of it. The pending row is gone, so nothing says "submitting", while the last
 * state read may predate the turn and answer idle. It lasts until a read that
 * finished after the entry landed has answered, or the turn's result arrives,
 * so a turn that never started does not type forever.
 *
 * Only a send seen pending in this visit counts: a stored message with no reply
 * from before it says nothing about a turn running now.
 */
export function useSendHandoff(
  entries: ThreadEntry[],
  pending: PendingMessage[],
  state: { inFlight: boolean; asOf: number | null; error: string | null },
): string | null {
  const sent = useRef(new Set<string>());
  const landed = useRef(new Map<string, number>());
  for (const row of pending) if (row.state !== "failed") sent.current.add(row.message);
  const index = entries.findLastIndex((e) => e.role === "operator" && e.kind === "message");
  const message = entries[index]?.message;
  if (!message || !sent.current.has(message)) return null;
  // By position: hosted agent rows often carry no message id.
  if (entries.slice(index + 1).some((e) => e.kind === "turn_result")) return null;
  let at = landed.current.get(message);
  if (at === undefined) {
    at = Date.now();
    landed.current.set(message, at);
  }
  const answered = !state.inFlight && (state.error !== null || (state.asOf !== null && state.asOf > at));
  return answered ? null : message;
}
