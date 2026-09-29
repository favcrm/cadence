import { useWriteBlock } from "../auth/WriteGate";
import { useState } from "react";
import { api, ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import { useQuery } from "../../lib/useResource";
import Button from "../../ui/Button";
import { ideaDecisionBody, type IdeaAction } from "./idea";

/**
 * An idea card (CAD-140): a researched idea waiting at the gate, with
 * the operator's three decisions — Approve mints exactly the proposed
 * children, Reject (with a reason) drops it, Park (with a date)
 * re-opens it later. Goes through the board's operator-only idea
 * endpoint; the card re-reads the issue and the overview after a
 * decision.
 */
export default function IdeaCard({
  issue,
  readOnly,
  onOpenIssue,
  onDecided,
}: {
  issue: string;
  readOnly: boolean;
  onOpenIssue: (id: string) => void;
  onDecided?: (issue: string, action: string) => void;
}) {
  const state = useQuery(resources.issue(issue));
  const [busy, setBusy] = useState<IdeaAction | null>(null);
  const [mode, setMode] = useState<"reject" | "park" | null>(null);
  const [reason, setReason] = useState("");
  const [parkUntil, setParkUntil] = useState("");
  const [error, setError] = useState<string | null>(null);

  const block = useWriteBlock(readOnly);
  const detail = state.data?.id === issue ? state.data : null;

  const decide = (action: IdeaAction) => {
    const decided = ideaDecisionBody(action, reason, parkUntil);
    if (decided.body === null) {
      setError(decided.error);
      return;
    }
    const body = decided.body;
    if (!detail) {
      setError("Re-read the idea — it is still loading.");
      return;
    }
    setBusy(action);
    setError(null);
    // CAD-140: bind the decision to the plan shown — REQUIRED. The
    // card read this rev; a moved issue refuses with 409 `stale_view`.
    api
      .ideaDecide(issue, action, { ...body, expect_rev: detail.rev })
      .then((out) => {
        setMode(null);
        setReason("");
        setParkUntil("");
        void resources.issue(issue).invalidate();
        void resources.overview.invalidate();
        onDecided?.(issue, String((out as { decision?: { action?: unknown } }).decision?.action ?? action));
      })
      .catch((e: ApiError) => setError(e.message ?? String(e)))
      .finally(() => setBusy(null));
  };

  if (!detail) {
    return (
      <div className="card px-3.5 py-3 text-label text-ink-500" data-idea-card={issue}>
        {state.status === "failed" ? `Idea ${issue} could not be read — ${state.error}` : `Loading idea ${issue}…`}
      </div>
    );
  }

  return (
    <section className="card overflow-hidden" data-idea-card={issue} aria-label={`idea ${issue}`}>
      <header className="px-3.5 pt-3 pb-2 min-w-0">
        <div className="flex items-center gap-2 flex-wrap">
          <span className="slabel">idea</span>
          <button className="lnk num text-label" onClick={() => onOpenIssue(issue)}>
            {issue}
          </button>
          <span className="chip bg-warn/10 text-warn">awaiting you</span>
        </div>
        <div className="text-cardtitle font-medium text-ink-100 mt-1 break-words">{detail.title}</div>
      </header>

      <div className="px-3.5 py-2.5 border-t border-ink-700 space-y-2">
        {block ? (
          <p className="text-label text-ink-500">
            {block} Or decide with `cadence idea decide {issue} approve`.
          </p>
        ) : (
          <>
            {mode === "reject" && (
              <label className="block">
                <span className="slabel">reason (required)</span>
                <textarea
                  value={reason}
                  onChange={(e) => setReason(e.target.value)}
                  rows={2}
                  className="field w-full mt-1 text-secondary"
                  placeholder="What is wrong with this idea?"
                  aria-label="rejection reason"
                />
              </label>
            )}
            {mode === "park" && (
              <label className="block">
                <span className="slabel">re-open after (required)</span>
                <input
                  type="date"
                  value={parkUntil}
                  onChange={(e) => setParkUntil(e.target.value)}
                  className="field w-full mt-1 text-secondary"
                  aria-label="park until date"
                />
              </label>
            )}
            <div className="flex flex-wrap items-center gap-2">
              {mode === null && (
                <Button
                  variant="primary"
                  size="sm"
                  disabled={busy !== null}
                  loading={busy === "approve"}
                  onClick={() => decide("approve")}
                >
                  {busy === "approve" ? "Approving…" : "Approve idea"}
                </Button>
              )}
              {mode === "reject" ? (
                <>
                  <Button
                    variant="danger"
                    size="sm"
                    disabled={busy !== null}
                    loading={busy === "reject"}
                    onClick={() => decide("reject")}
                  >
                    {busy === "reject" ? "Rejecting…" : "Reject idea"}
                  </Button>
                  <Button size="sm" onClick={() => { setMode(null); setError(null); }}>
                    Cancel
                  </Button>
                </>
              ) : (
                mode === null && (
                  <Button variant="danger" size="sm" disabled={busy !== null} onClick={() => setMode("reject")}>
                    Reject…
                  </Button>
                )
              )}
              {mode === "park" ? (
                <>
                  <Button
                    size="sm"
                    disabled={busy !== null}
                    loading={busy === "park"}
                    onClick={() => decide("park")}
                  >
                    {busy === "park" ? "Parking…" : "Park idea"}
                  </Button>
                  <Button size="sm" onClick={() => { setMode(null); setError(null); }}>
                    Cancel
                  </Button>
                </>
              ) : (
                mode === null && (
                  <Button size="sm" disabled={busy !== null} onClick={() => setMode("park")}>
                    Park…
                  </Button>
                )
              )}
            </div>
          </>
        )}
        {error && (
          <p className="text-label text-fail break-words" role="alert">
            {error}
          </p>
        )}
      </div>
    </section>
  );
}
