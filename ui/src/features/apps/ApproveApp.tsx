import { useState } from "react";
import { api, ApiError } from "../../lib/api";
import { cache, resources } from "../../lib/resources";
import type { Viewer } from "../projects/work";
import { approveBlock, approvalPending, type GateRow } from "./apps";

/**
 * The operator's Approve for one app (CAD-557): POSTs the board's relay
 * of the daemon's `app_approve` — the same call `cadence app approve`
 * makes, admitted by the same operator proof as the board's other
 * operator-only writes. Offered only while approval is pending; when the
 * board cannot run it the reason is shown instead.
 */
export default function ApproveApp({ row, viewer }: { row: GateRow; viewer: Viewer }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!approvalPending(row)) return null;
  const blocked = approveBlock(row, viewer);
  if (blocked) {
    return (
      <p className="text-micro text-ink-500 mt-2 break-words" role="note">
        {blocked}
      </p>
    );
  }
  const name = row.name ?? "";
  const approve = () => {
    if (busy) return;
    setBusy(true);
    setError(null);
    api
      .appApprove(row.project, name)
      .then(() => {
        // The app_approved event flips every view of the gate: the rows,
        // this app's detail, and the workflows' approval chips.
        void resources.apps.invalidate();
        cache.invalidate("app");
        cache.invalidate("workflows");
      })
      .catch((e) =>
        setError(e instanceof ApiError ? e.message : String(e)),
      )
      .finally(() => setBusy(false));
  };
  return (
    <span className="inline-flex flex-wrap items-center gap-2">
      <button
        type="button"
        onClick={approve}
        disabled={busy}
        className="chip bg-accent/15 text-accent hover:bg-accent/25 transition-colors disabled:opacity-40"
        title={`approve ${name} for its current structure`}
      >
        {busy ? "approving…" : "approve"}
      </button>
      {error && (
        <span className="text-micro text-fail break-words" role="alert">
          {error}
        </span>
      )}
    </span>
  );
}
