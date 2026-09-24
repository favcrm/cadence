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

/**
 * Overview needs-me sections, keyed by the server-resolved `audience`
 * (CAD-253). The server decides who a row is for; this is copy only.
 */
export const NEED_SECTION_LABEL = {
  operator: "Needs your decision",
  team: "Team handling",
  dependency: "Waiting on dependency",
  info: "Information",
} as const;

/** "Needs your decision" with no operator row — an answer, not a gap. */
export const NEEDS_DECISION_EMPTY = "Nothing needs your decision";

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
