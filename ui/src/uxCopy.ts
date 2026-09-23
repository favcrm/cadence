const KIND_LABEL: Record<string, string> = {
  merge: "merge",
  intake: "intake",
  approval: "approval",
  approval_menu: "approval menu",
  fenced: "fenced",
  stalled: "stalled",
  drift: "drift",
  pr_no_verdict: "no verdict",
  review_no_pr: "review",
  blocked_ready: "unblocked",
  ci_red: "ci red",
  ci_unverified: "ci unverified",
  silent_end: "silent end",
  inbox_unread: "inbox",
  inbox_stale: "stale inbox",
  tracker_behind: "behind",
};

/**
 * One default-branch SHA's CI label. Only the SHA's own successful run
 * reads "passed"; a cancelled or missing SHA covered by a later pass
 * says so and never borrows the pass.
 */
export function shaCiLabel(s: {
  state: string;
  covered_by?: string | null;
  conclusion?: string | null;
}): string {
  const cover = s.covered_by
    ? `covered by ${s.covered_by.slice(0, 7)}`
    : "not yet covered";
  switch (s.state) {
    case "passed":
      return "passed";
    case "failed":
      return s.conclusion && s.conclusion !== "failure" ? `failed (${s.conclusion})` : "failed";
    case "pending":
      return "pending";
    case "cancelled":
      return `${s.conclusion && s.conclusion !== "cancelled" ? s.conclusion : "cancelled"} — ${cover}`;
    case "missing":
      return `no ci run — ${cover}`;
    default:
      return "unknown";
  }
}

/** Empty Agents list. `all` means the fleet itself is empty. */
export function agentsEmptyCopy(project: string): string {
  if (project === "all") return "No agents registered.";
  return "No agents bound to this project.";
}

/** Overview need chip. Kinds absent from the map stay unclassified. */
export function needLabel(kind: string): string {
  return KIND_LABEL[kind] ?? "unknown / unclassified";
}

const NEED_GROUP: Record<string, "decision" | "team" | "dependency" | "info"> = {
  approval: "decision",
  merge: "team",
  intake: "team",
  fenced: "team",
  stalled: "team",
  review_no_pr: "team",
  blocked_ready: "team",
  pr_no_verdict: "team",
  ci_red: "team",
  ci_unverified: "team",
  silent_end: "team",
  // A sampled menu is not an operator approval.
  approval_menu: "team",
  drift: "dependency",
  inbox_stale: "team",
  inbox_unread: "info",
  tracker_behind: "info",
};

/** Unknown kinds stay in team handling, matching the previous fallback. */
export function needGroupKey(kind: string): "decision" | "team" | "dependency" | "info" {
  return NEED_GROUP[kind] ?? "team";
}

/**
 * Drawer status line while the board status comes from notes.
 * A bound job outranks notes; other sources do not use this sentence.
 */
export function notesStatusSentence(
  statusSource: string,
  noteKind: string,
  status: string,
): string | null {
  if (statusSource !== "notes") return null;
  return `This status comes from the latest tagged note (a ${noteKind}), so the board shows ${status}. A bound job outranks notes.`;
}
