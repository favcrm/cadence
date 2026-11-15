import type { Resource } from "../../lib/cache";
import { cache } from "../../lib/resources";
import { probe, type DocumentsProbe } from "./documentsClient";

/**
 * CAD-2286: whether the board has the document lifecycle, as one shared store.
 *
 * `probe()` resolves every answer, so `Resource`'s own latch (a thrown 501)
 * never engages. The store latches `unavailable` itself: once the board has
 * said it does not serve the lifecycle (a 404, a 501 `not_implemented`, or a
 * capability this client does not speak), later revalidations answer from the
 * store without a request. The cache resets the store when the private session
 * changes, which clears the latch, so a new session asks again. An `error` or
 * an `available` answer is never latched: the next revalidation asks again.
 *
 * An `error` never replaces an earlier `available` answer: a failed re-read
 * says nothing new about the capability, so the store keeps its last good
 * answer and the nav item does not flicker away on one bad request.
 */
export const documentsProbe: Resource<DocumentsProbe> = cache.resource<DocumentsProbe>("documentsProbe", async () => {
  const seen = documentsProbe.get().data;
  if (seen?.kind === "unavailable") return seen;
  const next = await probe();
  return next.kind === "error" && seen?.kind === "available" ? seen : next;
});

/**
 * The Documents nav item is shown only on a hosted board whose probe answered
 * `available`. A native board never shows it, and the item stays hidden while
 * the probe loads, fails or says the lifecycle is not served.
 */
export function documentsNavHidden(hosted: boolean | undefined, answer: DocumentsProbe | null | undefined): boolean {
  return !(hosted === true && answer?.kind === "available");
}
