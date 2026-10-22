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

/**
 * The installed apps the shell currently knows about. A queued turn ahead of
 * the running one carries no app origin, so its destination is resolved by
 * looking for its conversation in these installs' observed lists.
 */
let installs: string[] = [];
const installListeners = new Set<() => void>();

export function setKnownInstalls(ids: string[]): void {
  if (ids.length === installs.length && ids.every((id, index) => id === installs[index])) return;
  installs = ids;
  installListeners.forEach((listener) => listener());
}

export function knownInstalls(): string[] {
  return installs;
}

export function onKnownInstallsChanged(listener: () => void): () => void {
  installListeners.add(listener);
  return () => { installListeners.delete(listener); };
}
