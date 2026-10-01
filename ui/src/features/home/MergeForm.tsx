import { useState } from "react";
import { api, ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import type { HomeNeed } from "./needs";
import Button from "../../ui/Button";

/**
 * The merge decision (CAD-431, CAD-140): what passed, then Merge
 * pinned to the shown head — or Decline with a reason. The merge
 * carries the head it was approved against; a moved head refuses
 * with 409 `head_moved` before any `gh` merge runs.
 */
export default function MergeForm({
  need,
  readOnly,
  onDone,
}: {
  need: HomeNeed & { action: { type: "merge" } };
  readOnly: boolean;
  onDone: (text: string) => void;
}) {
  const { issue, pr, sha, reviewer, verdict } = need.action;
  // CAD-140: the approval names its head — REQUIRED. A row that
  // predates the field cannot merge until it is re-read.
  const shown = sha && sha.trim() ? sha : null;
  const [busy, setBusy] = useState<null | "merge" | "decline">(null);
  const [declining, setDeclining] = useState(false);
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const merge = () => {
    setBusy("merge");
    setError(null);
    if (!shown) {
      setError("Re-read the row — it names no head to approve.");
      return;
    }
    api
      .mergeDelivery(issue, shown)
      .then((out) => {
        void resources.overview.invalidate();
        onDone(`merge ${String((out as { state?: unknown }).state ?? "sent")}`);
      })
      .catch((e: ApiError) => setError(e.message ?? String(e)))
      .finally(() => setBusy(null));
  };
  const decline = () => {
    const why = reason.trim();
    if (!why) {
      setError("Say why — the reason goes to the worker loop.");
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
      .catch((e: ApiError) => setError(e.message ?? String(e)))
      .finally(() => setBusy(null));
  };
  return (
    <div className="mt-2 space-y-2">
      <p className="text-micro text-ink-500 break-words">
        {pr ?? issue}
        {sha ? ` · head ${sha.slice(0, 12)}` : ""}
        {reviewer ? ` · PASS by ${reviewer}` : ""}
      </p>
      {verdict && <p className="text-label text-ink-300 break-words">{verdict}</p>}
      {readOnly ? (
        <p className="text-micro text-ink-500">Board is read-only — merge with `cadence delivery merge {issue}`.</p>
      ) : (
        <>
          {declining && (
            <label className="block">
              <span className="slabel">reason (required)</span>
              <textarea
                value={reason}
                onChange={(e) => setReason(e.target.value)}
                rows={2}
                className="field w-full mt-1 text-secondary"
                placeholder="What should change before this can merge?"
                aria-label="decline reason"
              />
            </label>
          )}
          <div className="flex flex-wrap items-center gap-2">
            {!declining && shown && (
              <Button variant="primary" size="sm" disabled={busy !== null} loading={busy === "merge"} onClick={merge}>
                {busy === "merge" ? "Merging…" : "Merge"}
              </Button>
            )}
            {!declining && !shown && (
              <p className="text-micro text-ink-500">Re-read the row — it names no head to approve.</p>
            )}
            {declining ? (
              <>
                <Button
                  variant="danger"
                  size="sm"
                  disabled={busy !== null}
                  loading={busy === "decline"}
                  onClick={decline}
                >
                  {busy === "decline" ? "Declining…" : "Decline"}
                </Button>
                <Button size="sm" onClick={() => { setDeclining(false); setError(null); }}>
                  Cancel
                </Button>
              </>
            ) : (
              <Button variant="danger" size="sm" disabled={busy !== null} onClick={() => setDeclining(true)}>
                Decline…
              </Button>
            )}
          </div>
        </>
      )}
      {error && (
        <p className="text-micro text-fail break-words" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

