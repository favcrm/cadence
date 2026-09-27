export type ContentLane = "drafting" | "review" | "waiting" | "released";
export type ContentTone = "ok" | "warn" | "fail" | "muted";

export function runLane(run: {
  state: string;
  steps: { state: string; step_id: string }[];
  snapshot: { workflow: { steps: { id: string; kind: string }[] } };
}): ContentLane {
  if (
    run.state === "awaiting_approval" ||
    run.state === "approved" ||
    run.state === "succeeded" ||
    run.state === "failed" ||
    run.state === "cancelled"
  )
    return "waiting";
  const reviewing = run.snapshot.workflow.steps.some(
    (step) =>
      step.kind === "review_text" &&
      run.steps.some(
        (receipt) =>
          receipt.step_id === step.id && receipt.state === "dispatched",
      ),
  );
  return reviewing ? "review" : "drafting";
}

export function statusText(state: string): string {
  const labels: Record<string, string> = {
    awaiting_approval: "Approve plan",
    approved: "Ready to start",
    running: "Running",
    succeeded: "Review complete",
    failed: "Failed",
    cancelled: "Cancelled",
    waiting: "Approve release",
    decided: "Release approved",
    executing: "Releasing",
    done: "Released to Local",
    reconcile: "Check outcome",
    closed: "Closed",
    pending: "Queued",
    dispatched: "Working",
  };
  return labels[state] ?? state.replaceAll("_", " ");
}

export function statusTone(state: string): ContentTone {
  if (state === "done" || state === "succeeded") return "ok";
  if (state === "failed") return "fail";
  if (["awaiting_approval", "waiting", "reconcile"].includes(state))
    return "warn";
  return "muted";
}

export function plainTitle(
  inputs: Record<string, string>,
  fallback: string,
): string {
  const source = inputs.source || inputs.source_text;
  return (
    inputs.subject?.trim() ||
    inputs.title?.trim() ||
    inputs.topic?.trim() ||
    source?.trim().split("\n")[0]?.slice(0, 100) ||
    fallback
  );
}
