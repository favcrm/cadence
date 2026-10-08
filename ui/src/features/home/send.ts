import { api, type ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import type { ThreadRef } from "../../lib/types";
import { MASTER } from "./master";
import { addPending, newMessageId, settlePending } from "./thread";

/** Queue `text` to the master: optimistic entry, reconciled by message id.
 *  `refs` are the needs-me subjects an Ask-master draft cites — they
 *  ride `thread_send`'s `refs` onto the entry's payload (CAD-574). */
export function sendToMaster(
  text: string,
  message = newMessageId(),
  refs?: ThreadRef[],
): Promise<{ ok: true } | { ok: false; error: string }> {
  const body = text.trim();
  if (!body) return Promise.resolve({ ok: false, error: "Nothing to send." });
  const store = resources.masterThread;
  store.write((s) => addPending(s, message, body, Date.now(), refs));
  return api
    .threadSend(MASTER, body, message, refs)
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
