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

export interface UnreadCandidatePost {
  id: string;
  silent: boolean;
  sender: { id: string | null };
}

/**
 * Resolve the first unread incoming post id for a room opened with `unread > 0`.
 * Own posts and `/note` silent posts are skipped because neither contributes to
 * the viewer's unread badge count.
 */
export function findFirstUnreadPostId(
  posts: readonly UnreadCandidatePost[],
  unread: number,
  myId: string,
): string | null {
  if (!Number.isSafeInteger(unread) || unread <= 0 || posts.length === 0) return null;
  let remaining = unread;
  let oldestCandidate: string | null = null;
  for (let i = posts.length - 1; i >= 0; i -= 1) {
    const post = posts[i]!;
    if (post.silent || post.sender.id === myId) continue;
    oldestCandidate = post.id;
    remaining -= 1;
    if (remaining <= 0) return post.id;
  }
  return oldestCandidate;
}
