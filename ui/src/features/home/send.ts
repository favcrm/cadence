import { api, type ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import type { ThreadRef } from "../../lib/types";
import { MASTER } from "./master";
import { addPending, newMessageId, settlePending } from "./thread";

/** Queue `text` to the master: optimistic entry, reconciled by message id.
 *  `refs` are the needs-me subjects an Ask-master draft cites — they
 *  ride `thread_send`'s `refs` onto the entry's payload (CAD-574).
 *  `attachments` are the retained chat-file ids (CAD-1168): the
 *  pending row carries them so a Retry resends the exact same
 *  envelope — a resend without them is the daemon's content
 *  conflict, not the message the operator sent. */
export function sendToMaster(
  text: string,
  message = newMessageId(),
  refs?: ThreadRef[],
  attachments?: { id: string }[],
): Promise<{ ok: true } | { ok: false; error: string }> {
  const body = text.trim();
  if (!body) return Promise.resolve({ ok: false, error: "Nothing to send." });
  const store = resources.masterThread;
  store.write((s) => addPending(s, message, body, Date.now(), refs, attachments));
  return api
    .threadSend(MASTER, body, message, refs, undefined, undefined, attachments)
    .then(() => {
      store.write((s) => settlePending(s, message, { ok: true }));
      // The send queued a turn — refresh the header's state soon.
      void resources.masterState.refresh();
      return { ok: true as const };
    })
    .catch((e: ApiError) => {
      const error = e.message ?? String(e);
      store.write((s) => settlePending(s, message, { ok: false, error }));
      return { ok: false as const, error };
    });
}
