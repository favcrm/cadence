import { useSyncExternalStore } from "react";
import { onPrivateSessionReset } from "../../lib/privateSession";

/**
 * Current unread count across Hosted Channels (`NotificationInbox` publishes
 * the authoritative `unread_total`). Resets to 0 whenever the private session
 * resets so sidebar badges and document titles never retain stale
 * private state.
 */
let unreadCount = 0;
const listeners = new Set<() => void>();

export function setChannelsUnread(count: number): void {
  const next = typeof count === "number" && Number.isSafeInteger(count) && count > 0 ? count : 0;
  if (unreadCount === next) return;
  unreadCount = next;
  for (const listener of listeners) listener();
}

export function useChannelsUnread(): number {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => unreadCount,
    () => unreadCount,
  );
}

onPrivateSessionReset(() => {
  setChannelsUnread(0);
});
