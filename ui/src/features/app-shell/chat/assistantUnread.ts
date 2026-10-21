import { useSyncExternalStore } from "react";

/**
 * Where a new assistant reply waits unseen (CAD-1700): `home` or an app
 * install id. The surfaces that can tell (an app pane comparing its
 * conversations with this browser's last-seen, Home comparing its thread
 * tail) publish here; the nav reads it to draw a quiet dot. In-memory,
 * per tab: the durable last-seen lives in `unreadSeen` (localStorage), so
 * opening a conversation in one tab clears it in every tab through the
 * pane's `storage` subscription, which republishes here.
 */
const unread = new Set<string>();
const listeners = new Set<() => void>();
let snapshot: ReadonlySet<string> = new Set();

export function setAssistantUnread(where: string, value: boolean): void {
  if (unread.has(where) === value) return;
  if (value) unread.add(where);
  else unread.delete(where);
  snapshot = new Set(unread);
  for (const listener of listeners) listener();
}

export function useAssistantUnread(): ReadonlySet<string> {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => snapshot,
    () => snapshot,
  );
}
