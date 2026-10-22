/**
 * CAD-1700: the conversation currently on screen, shared with the shell's
 * reply announcer. The picker's selection lives in client state (a default
 * conversation, a subject switch), so the URL alone cannot answer "is this
 * conversation visible?"; the pane publishes it here while mounted.
 */
let visible: { installId: string; conversation: string | null } | null = null;

export function setVisibleConversation(installId: string, conversation: string | null): void {
  visible = { installId, conversation };
}

export function visibleConversation(): { installId: string; conversation: string | null } | null {
  return visible;
}
