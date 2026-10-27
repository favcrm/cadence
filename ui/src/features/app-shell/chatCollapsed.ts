/**
 * Whether the assistant rail starts collapsed. Kept free of React and storage so
 * the rule is testable on its own; the stored choice and the pending decision
 * are supplied by the caller.
 */
/**
 * An app page opens with its work area in front, so the assistant starts
 * collapsed until the person expands it — unless a decision is waiting, which
 * is exactly the thing the rail must not hide. An explicit choice always wins.
 */
export function resolveChatCollapsed(
  choice: boolean | null,
  installId: string,
  pendingDecision: boolean,
): boolean {
  if (choice !== null) return choice;
  return installId !== "" && !pendingDecision;
}

