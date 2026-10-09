import { useState } from "react";
import { api, ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import type { HomeNeed } from "./needs";

/** What a moved head says in plain words (409 `head_moved`). */
export const HEAD_MOVED_COPY = "This changed after it was checked. It needs a fresh review.";

/**
 * A refusal from an approval or merge route in plain words (CAD-1218).
 * Chosen by the route's `code`, `check` and status; raw daemon text, pids,
 * commands and ids never reach the card.
 */
export function plainRefusal(e: unknown): string {
  if (!(e instanceof ApiError)) return "That didn't go through. Try again.";
  if (e.code === "head_moved") return HEAD_MOVED_COPY;
  if (e.check === "approver_not_allowed") return "Only the board's owner can approve from here.";
  if (e.check === "member_role") return "Only the board's owner can decide this.";
  if (e.check && ["operator_proof", "caller_identity", "operator_only", "operator_session_required", "board_session_required"].includes(e.check)) {
    return "This board can't confirm it is you right now. Sign in again from your own computer.";
  }
  if (e.status === 503) return "The board can't reach the team right now. Try again in a moment.";
  if (e.status === 409) return "This was already decided. A fresh review makes a new version.";
  if (e.status === 403) return "You can't do that from here.";
  if (e.status === 404 || /no registered project/.test(e.message ?? "")) return "This change isn't in a project this board knows.";
  if (e.status >= 500) return "That didn't go through. Try again, or ask for help.";
  return "That didn't go through.";
}

/**
 * The merge decision (CAD-431, CAD-140): Merge pinned to the reviewed
 * head, or Decline with a reason. The merge carries the head it was
 * approved against; a moved head refuses with 409 `head_moved` before
 * any `gh` merge runs. The review drawer renders it (CAD-1216); this
 * hook is the whole decision, so the buttons live in the drawer footer.
 */
export function useMergeDecision(
  need: HomeNeed & { action: { type: "merge" } },
  onDone: (text: string) => void,
) {
  const { issue, sha } = need.action;
  // CAD-140: the approval names its head — REQUIRED. A row that
  // predates the field cannot merge until it is re-read.
  const head = sha && sha.trim() ? sha : null;
  const [busy, setBusy] = useState<null | "merge" | "decline">(null);
  const [error, setError] = useState<string | null>(null);
  const fail = (e: unknown) => setError(plainRefusal(e));
  const merge = () => {
    if (!head) {
      setError("This has no checked version to approve. It needs a fresh review.");
      return;
    }
    setBusy("merge");
    setError(null);
    api
      .mergeDelivery(issue, head)
      .then((out) => {
        void resources.overview.invalidate();
        onDone(`merge ${String((out as { state?: unknown }).state ?? "sent")}`);
      })
      .catch(fail)
      .finally(() => setBusy(null));
  };
  const decline = (reason: string) => {
    const why = reason.trim();
    if (!why) {
      setError("Say why — the reason goes to whoever made the change.");
      return;
    }
    setBusy("decline");
    setError(null);
    api
      .declineDelivery(issue, why)
      .then((out) => {
        void resources.overview.invalidate();
        onDone(`declined: ${String((out as { state?: unknown }).state ?? "sent")}`);
      })
      .catch(fail)
      .finally(() => setBusy(null));
  };
  return { head, busy, error, merge, decline, clearError: () => setError(null) };
}
