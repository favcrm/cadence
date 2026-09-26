import { useWriteBlock } from "../auth/WriteGate";
import { useState } from "react";
import { api, ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import { useQuery } from "../../lib/useResource";
import Button from "../../ui/Button";
import { planView, rejectReasonError } from "./plan";

const STATE_CHIP: Record<string, string> = {
  proposed: "bg-warn/10 text-warn",
  approved: "bg-ok/15 text-ok",
  rejected: "bg-fail/10 text-fail",
};

const STATUS_CHIP: Record<string, string> = {
  done: "text-ok",
  doing: "text-accent",
  review: "text-info",
  ready: "text-ink-300",
  dropped: "text-ink-500 line-through",
};

/**
 * A plan card (CAD-328): the epic's goal, its tickets with size and
 * acceptance, size-weighted progress, and — while the plan is proposed —
 * Approve, or Reject with a required reason. Both go through the board's
 * operator-only plan endpoints; the card re-reads the epic after a
 * decision and whenever the tracker stream invalidates issues.
 */
export default function PlanCard({
  epic,
  readOnly,
  onOpenIssue,
  onDecided,
}: {
  epic: string;
  readOnly: boolean;
  onOpenIssue: (id: string) => void;
  onDecided?: (epic: string, state: string) => void;
}) {
  const state = useQuery(resources.issue(epic));
  const [busy, setBusy] = useState<null | "approve" | "reject">(null);
  const [rejecting, setRejecting] = useState(false);
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);

  const block = useWriteBlock(readOnly);
  const detail = state.data?.id === epic ? state.data : null;
  const view = detail ? planView(detail, block) : null;

  if (!detail) {
    return (
      <div className="card px-3.5 py-3 text-label text-ink-500" data-plan-card={epic}>
        {state.status === "failed" ? `Plan ${epic} could not be read — ${state.error}` : `Loading plan ${epic}…`}
      </div>
    );
  }
  if (!view) {
    return (
      <div className="card px-3.5 py-3 text-label text-ink-500" data-plan-card={epic}>
        {epic} is not a plan.
      </div>
    );
  }

  const decide = (verb: "approve" | "reject") => {
    if (verb === "reject") {
      const why = rejectReasonError(reason);
      if (why) {
        setError(why);
        return;
      }
    }
    setBusy(verb);
    setError(null);
    api
      .decidePlan(epic, verb, verb === "reject" ? reason : undefined)
      .then((out) => {
        setRejecting(false);
        setReason("");
        void resources.issue(epic).invalidate();
        void resources.overview.invalidate();
        onDecided?.(epic, String((out as { state?: unknown }).state ?? verb));
      })
      .catch((e: ApiError) => setError(e.message ?? String(e)))
      .finally(() => setBusy(null));
  };

  return (
    <section className="card overflow-hidden" data-plan-card={epic} aria-label={`plan ${epic}`}>
      <header className="px-3.5 pt-3 pb-2 flex items-start gap-2 min-w-0">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2 flex-wrap">
            <span className="slabel">plan</span>
            <button className="lnk num text-label" onClick={() => onOpenIssue(epic)}>
              {epic}
            </button>
            <span className={`chip ${STATE_CHIP[view.state] ?? "bg-ink-800 text-ink-400"}`}>
              {view.state === "proposed" ? "awaiting you" : view.state}
            </span>
          </div>
          <div className="text-cardtitle font-medium text-ink-100 mt-1 break-words">{view.title}</div>
          {view.goal && <p className="text-secondary text-ink-400 mt-1 break-words">{view.goal}</p>}
        </div>
      </header>

      <ul className="border-t border-ink-700 divide-y divide-ink-700">
        {view.tickets.map((t) => (
          <li key={t.id} className="px-3.5 py-2 flex items-start gap-2.5 min-w-0">
            <button className="lnk num text-label shrink-0 pt-px" onClick={() => onOpenIssue(t.id)}>
              {t.id}
            </button>
            <div className="min-w-0 flex-1">
              <div className="text-secondary text-ink-200 break-words">{t.title}</div>
              <div className="text-micro text-ink-500 mt-0.5 flex flex-wrap gap-x-2">
                <span>
                  {typeof t.acceptance === "number"
                    ? `${t.acceptance} acceptance ${t.acceptance === 1 ? "check" : "checks"}`
                    : "acceptance unknown"}
                </span>
                {t.owner && <span>· {t.owner}</span>}
                {t.blocked_by && t.blocked_by.length > 0 && <span>· after {t.blocked_by.join(", ")}</span>}
              </div>
            </div>
            <span className="chip bg-ink-800 text-ink-300 num shrink-0" title="size (S=1, M=3, L=8)">
              {t.size ?? "M?"}
            </span>
            <span className={`text-micro num shrink-0 w-12 text-right ${STATUS_CHIP[t.status] ?? "text-ink-400"}`}>
              {t.status}
            </span>
          </li>
        ))}
        {view.tickets.length === 0 && (
          <li className="px-3.5 py-2 text-label text-ink-500">No tickets listed.</li>
        )}
      </ul>

      <div className="px-3.5 py-2.5 border-t border-ink-700">
        <div className="flex items-center justify-between text-micro text-ink-500">
          <span>
            {view.done}/{view.total} done
          </span>
          <span className="num">{view.percent === null ? "progress unknown" : `${view.percent}%`}</span>
        </div>
        <div
          className="h-1.5 rounded bg-ink-800 mt-1.5 overflow-hidden"
          role="progressbar"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={view.percent ?? 0}
        >
          <div className="h-full bg-accent" style={{ width: `${view.percent ?? 0}%` }} />
        </div>
      </div>

      {view.state === "rejected" && view.reason && (
        <p className="px-3.5 pb-2.5 text-label text-ink-400 break-words">
          Rejected{view.decidedBy ? ` by ${view.decidedBy}` : ""}: {view.reason}
        </p>
      )}

      {view.state === "proposed" && (
        <div className="px-3.5 py-2.5 border-t border-ink-700 space-y-2">
          {view.canDecide ? (
            <>
              {rejecting && (
                <label className="block">
                  <span className="slabel">reason (required)</span>
                  <textarea
                    value={reason}
                    onChange={(e) => setReason(e.target.value)}
                    rows={2}
                    className="field w-full mt-1 text-secondary"
                    placeholder="What should change before this plan can go ahead?"
                    aria-label="rejection reason"
                  />
                </label>
              )}
              <div className="flex flex-wrap items-center gap-2">
                {!rejecting && (
                  <Button
                    variant="primary"
                    size="sm"
                    disabled={busy !== null}
                    loading={busy === "approve"}
                    onClick={() => decide("approve")}
                  >
                    {busy === "approve" ? "Approving…" : "Approve plan"}
                  </Button>
                )}
                {rejecting ? (
                  <>
                    <Button
                      variant="danger"
                      size="sm"
                      disabled={busy !== null}
                      loading={busy === "reject"}
                      onClick={() => decide("reject")}
                    >
                      {busy === "reject" ? "Rejecting…" : "Reject plan"}
                    </Button>
                    <Button
                      size="sm"
                      onClick={() => {
                        setRejecting(false);
                        setError(null);
                      }}
                    >
                      Cancel
                    </Button>
                  </>
                ) : (
                  <Button variant="danger" size="sm" disabled={busy !== null} onClick={() => setRejecting(true)}>
                    Reject…
                  </Button>
                )}
              </div>
            </>
          ) : (
            <p className="text-label text-ink-500">{view.blockedReason}</p>
          )}
          {error && (
            <p className="text-label text-fail break-words" role="alert">
              {error}
            </p>
          )}
        </div>
      )}
    </section>
  );
}
