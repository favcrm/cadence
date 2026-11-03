import { api, ApiError } from "../../lib/api";
import type { Resource } from "../../lib/cache";
import { privateSessionGeneration, withPrivateOperation } from "../../lib/privateSession";
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

/** Send through the path Threads uses. `message` is the idempotency key, so a
 *  retry of a lost response is one message rather than two. */
export async function sendToAgent(
  alias: string,
  text: string,
  message = newMessageId(),
  refs?: ThreadRef[],
  attachments?: { id: string }[],
): Promise<void> {
  const body = text.trim();
  if (!body) return;
  const store = agentThreadStore(alias);
  const session = privateSessionGeneration();
  // A refused retry says nothing about the attempt whose outcome was unknown.
  const wasUnconfirmed = store.get().data?.pending.some((p) => p.message === message && p.delivery === "unconfirmed") === true;
  store.write((s) => addPending(s, message, body, Date.now(), refs, attachments));
  try {
    await withPrivateOperation(async (op) => {
      await api.threadSend(alias, body, message, refs, undefined, undefined, attachments, op);
      op.assertCurrent();
    });
    store.write((s) => settlePending(s, message, { ok: true }));
    void store.refresh();
  } catch (e) {
    // Another session's page must not inherit this send's outcome.
    if (privateSessionGeneration() !== session) return;
    const error = e instanceof Error ? e.message : String(e);
    // A lost response cannot establish whether the server accepted the send;
    // a 4xx refused it, and a 501 is a definite "this board doesn't do that".
    const definite = e instanceof ApiError && ((e.status >= 400 && e.status < 500) || e.status === 501);
    store.write((s) =>
      settlePending(s, message, definite && !wasUnconfirmed ? { ok: false, error } : { ok: false, error, delivery: "unconfirmed" }),
    );
  }
}
