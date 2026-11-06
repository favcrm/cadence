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
  type ThreadState,
} from "../home/thread";

/** One agent's You ↔ agent conversation: the agent's home thread, the same one
 *  the Threads page lists. The store only holds what the daemon returned. */
export function agentThreadStore(alias: string): Resource<ThreadState> {
  return cache.family<string, ThreadState>("agentThread", (key) =>
    loadThread(threadReader(key), cache.peek<ThreadState>(`agentThread:${key}`)?.get().data ?? null),
  )(alias);
}

/** The hosted profile's selected conversation is never the agent's native thread. */
export function profileThreadStore(conversation: string): Resource<ThreadState> {
  return cache.family<string, ThreadState>("profileThread", (key) =>
    loadThread(threadReader("master", key), cache.peek<ThreadState>(`profileThread:${key}`)?.get().data ?? null),
  )(conversation);
}


/** Send through the path Threads uses. `message` is the idempotency key, so a
 *  retry of a lost response is one message rather than two. */
export async function sendToAgent(
  alias: string,
  text: string,
  message = newMessageId(),
  refs?: ThreadRef[],
  attachments?: { id: string }[],
  conversation?: string,
  signal?: AbortSignal,
): Promise<void> {
  const body = text.trim();
  if (!body) return;
  const store = conversation ? profileThreadStore(conversation) : agentThreadStore(alias);
  store.write((s) => addPending(s, message, body, Date.now(), refs, attachments));
  try {
    await withPrivateOperation((op) =>
      api.threadSend(conversation ? "master" : alias, body, message, refs, undefined, conversation, attachments, op),
      signal,
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
