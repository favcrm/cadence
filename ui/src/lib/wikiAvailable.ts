import type { Meta } from "./types";

/**
 * Whether this board serves /api/wiki. A local board always does; a hosted
 * tenant does only when its `/api/meta` advertises `wiki: true`. Before meta
 * loads the wiki is not hidden (the pre-meta default).
 */
export function wikiAvailable(meta: Pick<Meta, "hosted" | "wiki"> | null | undefined): boolean {
  return meta?.hosted !== true || meta.wiki === true;
}
