/**
 * Who may act on this board (CAD-313): the server decides, this only
 * explains. Board writes need the operator's session — an HttpOnly
 * cookie a `cadence ui login` link opens — so without one the SPA
 * disables every write control and says how to sign in. Pure, so it is
 * unit-tested in plain node (tests/writeGate.test.ts).
 */

import type { Meta } from "../../lib/types";

export const SIGN_IN_COMMAND = "cadence ui login";

export const READ_ONLY_REASON = "The board is read-only — the server refuses every write.";

/** Why writes are disabled for this client, or null when they are not. */
export function writeBlock(meta: Meta | null): string | null {
  if (!meta) return null;
  if (meta.read_only) return READ_ONLY_REASON;
  // An older server that reports no `operator` field predates sessions.
  if (meta.operator === false) {
    return `Sign in with \`${meta.login_hint ?? SIGN_IN_COMMAND}\` to act as the operator.`;
  }
  return null;
}

/**
 * The single-use nonce a login link carries in its fragment
 * (`#n=<64 hex>`), or null for anything else.
 */
export function parseLoginNonce(hash: string): string | null {
  const m = /^#n=([0-9a-f]{64})$/.exec(hash);
  return m ? m[1] : null;
}
