import { api, ApiError } from "../../lib/api";
import type { Resource } from "../../lib/cache";
import { withPrivateOperation } from "../../lib/privateSession";
import { cache, threadReader } from "../../lib/resources";
import type { ThreadRef } from "../../lib/types";
import {
  addPending,
  loadThread,
  newMessageId,
  settlePending,
  type PendingAttachment,
  type ThreadState,
} from "../home/thread";

/** One agent's You ↔ agent conversation: the agent's home thread, the same one
 *  the Threads page lists. The store only holds what the daemon returned. */
export function agentThreadStore(alias: string): Resource<ThreadState> {
  return cache.family<string, ThreadState>("agentThread", (key) =>
    loadThread(threadReader(key), cache.peek<ThreadState>(`agentThread:${key}`)?.get().data ?? null),
  )(alias);
}

/** Send through the path Threads uses. `message` is the idempotency key, so a
 *  retry of a lost response is one message rather than two. */
export async function sendToAgent(
  alias: string,
  text: string,
  message = newMessageId(),
  refs?: ThreadRef[],
  attachments?: PendingAttachment[],
): Promise<void> {
  const body = text.trim();
  if (!body) return;
  const store = agentThreadStore(alias);
  store.write((s) => addPending(s, message, body, Date.now(), refs, attachments));
  try {
    await withPrivateOperation((op) =>
      api.threadSend(alias, body, message, refs, undefined, undefined, attachments, op),
    );
    store.write((s) => settlePending(s, message, { ok: true }));
    void store.refresh();
  } catch (e) {
    const detail = e instanceof Error ? e.message : String(e);
    // A lost response cannot establish whether the server accepted the send.
    const error =
      e instanceof ApiError && e.status >= 400 && e.status < 500
        ? detail
        : `Delivery uncertain — check before retrying. ${detail}`;
    store.write((s) => settlePending(s, message, { ok: false, error }));
  }
}
