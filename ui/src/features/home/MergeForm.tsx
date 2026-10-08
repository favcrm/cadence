import { useState } from "react";
import { api, ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import type { HomeNeed } from "./needs";

/** What a moved head says in plain words (409 `head_moved`). */
export const HEAD_MOVED_COPY = "This changed after it was checked. It needs a fresh review.";

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
  const fail = (e: ApiError) =>
    setError(e.code === "head_moved" ? HEAD_MOVED_COPY : (e.message ?? String(e)));
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
