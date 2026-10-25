/**
 * CAD-1193 / CAD-1806: how the app reads the access probe. An unsigned
 * reader (admitted only by --allow-anonymous-reads) is a resolved
 * non-operator: the server omits `operator` for it. Only a claimed
 * session whose proof is missing is genuinely unavailable. Pure, so it
 * is unit-tested in plain node (tests/accessSplit.test.ts).
 */

import type { Meta } from "../../lib/types";

export function metaAccess(meta: Meta): {
  operator: boolean | null;
  access: "unavailable" | "checking";
  metaUnavailable: boolean;
} {
  const anonymousReader = meta.signed_in === false;
  const metaUnavailable = !anonymousReader && (meta.operator === null || meta.operator === undefined);
  return {
    operator: meta.operator ?? (anonymousReader ? false : null),
    access: metaUnavailable ? "unavailable" : "checking",
    metaUnavailable,
  };
}

/** The "Workspace waking or unreachable" banner: an unknown operator
 *  whose completed probe could not answer. */
export function wakingBanner(viewer: { operator: boolean | null; access?: string | null }): boolean {
  return viewer.operator === null && viewer.access === "unavailable";
}
