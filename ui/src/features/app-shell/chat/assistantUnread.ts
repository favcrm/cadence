import { useSyncExternalStore } from "react";

/**
 * Where a new assistant reply waits unseen (CAD-1700): `home` or an app
 * install id. The shell's app-list and Home-thread observers publish here;
 * the nav reads it to draw a quiet dot. In-memory, per tab: durable reply
 * watermarks live in `unreadSeen` (IndexedDB); committed seen changes
 * wake same-tab and cross-tab subscribers to clear the dot.
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
