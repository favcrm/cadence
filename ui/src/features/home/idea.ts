/**
 * The idea decision's view (CAD-140): a researched idea waits at the
 * gate and the operator approves, rejects, or parks it — nothing else
 * is dispatched. Validated before any request, mirroring `plan.ts`:
 * approve takes nothing, reject needs a reason, park needs a
 * `YYYY-MM-DD` date. Tested in tests/ideaCard.test.ts.
 */

export type IdeaAction = "approve" | "reject" | "park";

/** A rejection needs a reason — the decision object records it. */
export function rejectReasonError(reason: string): string | null {
  return reason.trim()
    ? null
    : "Say why — the reason goes on the decision for whoever proposed the idea.";
}

/** `YYYY-MM-DD`, on the calendar — the pipeline re-opens the idea after it. */
export function parkDateError(date: string): string | null {
  const day = date.trim();
  if (!day) return "Pick a date — the idea re-opens after it.";
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(day);
  if (!m) return "Use YYYY-MM-DD — the pipeline reads a UTC date.";
  const [, y, mo, d] = m;
  if (Number(y) < 1970 || Number(mo) < 1 || Number(mo) > 12 || Number(d) < 1 || Number(d) > 31)
    return "Use a real UTC date as YYYY-MM-DD.";
  return null;
}

/**
 * The decision the card is about to send, validated before any
 * request — approve sends nothing, reject sends its reason, park sends
 * its date. Returns the request body, or the error to show instead.
 */
export function ideaDecisionBody(
  action: IdeaAction,
  reason: string,
  parkUntil: string,
): { body: { reason?: string; park_until?: string }; error: null } | { body: null; error: string } {
  if (action === "approve") return { body: {}, error: null };
  if (action === "reject") {
    const why = rejectReasonError(reason);
    if (why) return { body: null, error: why };
    return { body: { reason: reason.trim() }, error: null };
  }
  const when = parkDateError(parkUntil);
  if (when) return { body: null, error: when };
  return { body: { park_until: parkUntil.trim() }, error: null };
}
