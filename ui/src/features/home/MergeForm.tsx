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

export const BURNED_STEP_UP_CHALLENGE_CODES = new Set([
  "challenge_expired",
  "expired_code",
  "challenge_invalid",
  "challenge_not_found",
  "challenge_consumed",
  "too_many_attempts",
]);

export function makeRequestId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return `req_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 10)}`;
}

export function formatProjectPolicyUnavailable(
  project?: string | null,
  t: (s: string) => string = (s) => s,
): string {
  const trimmed = project?.trim();
  const key = trimmed && trimmed !== "all" ? trimmed : "<key>";
  return `${t("The owner records this project's access policy first")} (\`cadence issue project policy ${key}\`).`;
}

export function formatRateLimitedStepUp(
  retryAfter?: number | null,
  t: (s: string) => string = (s) => s,
): string {
  if (typeof retryAfter === "number" && Number.isFinite(retryAfter) && retryAfter > 0) {
    return t("Too many verification emails requested. Wait {seconds}s before resending.").replace(
      "{seconds}",
      String(Math.ceil(retryAfter)),
    );
  }
  return t("Too many verification emails requested. Wait a moment before resending.");
}

/**
 * A refusal from an approval or merge route in plain words (CAD-1218).
 * Chosen by the route's `code`, `check` and status; raw daemon text, pids,
 * commands and ids never reach the card.
 */
export function plainRefusal(
  e: unknown,
  options?: { project?: string | null; t?: (s: string) => string },
): string {
  const t = options?.t ?? ((s: string) => s);
  if (!(e instanceof ApiError)) return t("That didn't go through. Try again.");
  if (e.code === "head_moved") return t(/no longer open/.test(e.message) ? NOT_OPEN_COPY : HEAD_MOVED_COPY);
  // The board sends the operator-proof refusal as `code`, other guards as `check`.
  const guard = e.check ?? e.code;
  if (guard === "approver_not_allowed") return t("Only the board's owner can approve from here.");
  if (guard === "member_role") return t("Only the board's owner can decide this.");
  if (guard === "project_policy_unavailable") return formatProjectPolicyUnavailable(options?.project, t);
  if (guard === "otp_invalid" || guard === "invalid_code") {
    return t("That 6-digit code did not match. Check the email and try again.");
  }
  if (guard === "challenge_expired" || guard === "expired_code") {
    return t("That verification code has expired. Send a new code and try again.");
  }
  if (guard === "challenge_invalid" || guard === "challenge_not_found" || guard === "challenge_consumed") {
    return t("Verification challenge expired or missing. Send a new 6-digit code.");
  }
  if (guard === "too_many_attempts") {
    return t("Too many verification attempts. Wait a few minutes or send a new code.");
  }
  if (guard === "rate_limited") return formatRateLimitedStepUp(e.retryAfter, t);
  if (guard === "email_delivery_unavailable") {
    return t("Could not send the verification email right now. Try again shortly.");
  }
  if (guard === "email_not_configured" || guard === "no_verified_email") {
    return t("Owner verification email is not configured for this workspace.");
  }
  if (guard === "capability_unavailable" || guard === "unavailable") {
    return t("Could not reach the approval service right now. Try again in a moment.");
  }
  if (guard === "step_up_required") {
    return t("Email verification is required before approving this version.");
  }
  if (guard === "project_archived") {
    return t("This project is archived and no longer accepts merge approvals.");
  }
  if (guard === "project_not_authorized") {
    return t("This project is not authorized on the hosted merge gate.");
  }
  if (guard && ["operator_proof", "caller_identity", "operator_only", "operator_session_required", "board_session_required"].includes(guard)) {
    return t("This board can't confirm it is you right now. Sign in again from your own computer.");
  }
  if (/reading the PR failed/.test(e.message)) return t(RETRY_COPY);
  if (/approval was revoked/.test(e.message)) {
    return t("This version's approval was taken back. A fresh review makes a new version.");
  }
  if (e.status === 503) return t("The board can't reach the team right now. Try again in a moment.");
  if (e.status === 409) return t("This was already decided. A fresh review makes a new version.");
  if (e.status === 403) return t("You can't do that from here.");
  if (e.status === 404 || /no registered project/.test(e.message ?? "")) {
    return t("This change isn't in a project this board knows.");
  }
  if (e.status >= 500) return t("That didn't go through. Try again, or ask for help.");
  return t("That didn't go through.");
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
