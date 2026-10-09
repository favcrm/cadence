import { useState } from "react";
import { api, ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import type { HomeNeed } from "./needs";

/** What a moved head says in plain words (409 `head_moved`). */
export const HEAD_MOVED_COPY = "This changed after it was checked. It needs a fresh review.";
/** A merged or closed PR has no head left to approve (also `head_moved`). */
const NOT_OPEN_COPY = "This pull request was already merged or closed.";
/** A GitHub read failed; the daemon answers 400 but it can be retried. */
const RETRY_COPY = "The board couldn't reach GitHub just now. Try again in a moment.";

/**
 * A refusal from an approval or merge route in plain words (CAD-1218).
 * Chosen by the route's `code`, `check` and status; raw daemon text, pids,
 * commands and ids never reach the card.
 */
export function plainRefusal(e: unknown): string {
  if (!(e instanceof ApiError)) return "That didn't go through. Try again.";
  if (e.code === "head_moved") return /no longer open/.test(e.message) ? NOT_OPEN_COPY : HEAD_MOVED_COPY;
  // The board sends the operator-proof refusal as `code`, other guards as `check`.
  const guard = e.check ?? e.code;
  if (guard === "approver_not_allowed") return "Only the board's owner can approve from here.";
  if (guard === "member_role") return "Only the board's owner can decide this.";
  if (guard && ["operator_proof", "caller_identity", "operator_only", "operator_session_required", "board_session_required"].includes(guard)) {
    return "This board can't confirm it is you right now. Sign in again from your own computer.";
  }
  if (/reading the PR failed/.test(e.message)) return RETRY_COPY;
  if (/approval was revoked/.test(e.message)) return "This version's approval was taken back. A fresh review makes a new version.";
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
