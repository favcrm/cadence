import { useEffect, useState } from "react";
import { plainTitle } from "./presentation";
import { workspaceApps, type WorkspaceRun } from "./workspaceApps";
import { ApiError } from "../../lib/api";

/** Library previews come from the accepted artifact, never the source brief. */
export function StoredDraftCard({ run, onOpen, onDenied }: { run: WorkspaceRun; onOpen: () => void; onDenied: () => void }) {
  const receipt = run.artifacts.find(item => run.reviews.some(review => review.artifact_digest === item.digest && review.decision === "approve"));
  const [text, setText] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    setText(null); setError(null);
    if (!receipt) return;
    const controller = new AbortController();
    void workspaceApps.artifact(receipt.id, controller.signal).then(value => {
      if (controller.signal.aborted) return;
      if (value.id !== receipt.id || value.digest !== receipt.digest) throw new Error("The stored draft does not match its accepted receipt");
      setText(value.text);
    }).catch(cause => {
      if (controller.signal.aborted) return;
      if (cause instanceof ApiError && [401, 403].includes(cause.status)) onDenied();
      setError(cause instanceof Error ? cause.message : "Could not read the stored draft");
    });
    return () => controller.abort();
  }, [receipt?.id, receipt?.digest, onDenied]);
  return <button type="button" className="wa-post-card" onClick={onOpen}>
    <span className="wa-status" data-tone="ok">Accepted text</span>
    <strong>{plainTitle(run.snapshot.inputs, run.snapshot.workflow.title)}</strong>
    <p>{error || (text === null ? "Loading stored draft…" : text.slice(0, 320))}</p>
    <span className="wa-kicker">Open draft and review</span>
  </button>;
}
