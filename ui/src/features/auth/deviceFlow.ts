/**
 * Device-grant sign-in helpers (CAD-777), pure — no React, no fetch —
 * so tests cover them in plain node (tests/deviceSignIn.test.ts).
 */

import { ApiError } from "../../lib/api";
import type { DeviceDisplay } from "./session";

/** The link the board shows for approval: `verification_uri_complete`
 *  when the issuer sent one, else `verification_uri`. Only `https:` or
 *  `http:` (loopback issuers in development) ever pass — a malformed
 *  or `javascript:`-style answer renders as no link, never as href. */
export function safeVerificationHref(display: {
  verification_uri: string;
  verification_uri_complete?: string;
}): string | null {
  const candidate = display.verification_uri_complete || display.verification_uri;
  try {
    const url = new URL(candidate);
    if (url.protocol === "https:" || url.protocol === "http:") return url.toString();
  } catch {
    // fall through
  }
  return null;
}

/** The next poll delay in seconds (RFC 8628): `slow_down` adds 5 s,
 *  `pending` keeps the pace; the starting point is the server's
 *  `interval` with a floor of 1 s. */
export function nextDelaySecs(current: number, status: string): number {
  const base = Math.max(1, current);
  return status === "slow_down" ? base + 5 : base;
}

/** Start delay: the server-reported interval, floored at 1 s. */
export function startDelaySecs(display: Pick<DeviceDisplay, "interval">): number {
  return Math.max(1, display.interval);
}

/** What the sign-in surface tells the user when a poll is refused.
 *  `device_subject_not_allowed` passes the server's message through —
 *  it names the subject id and the flag the operator adds. */
export function describeRefusal(err: unknown): string {
  if (err instanceof ApiError) {
    if (err.code === "device_subject_not_allowed" || err.code === "device_sign_in_refused") {
      return err.message;
    }
    if (err.check === "session_from_agent" || err.code === "session_from_agent") {
      return "Sign-in refused: this request came from an agent — open the board in your own browser.";
    }
    if (err.code === "issuer_unavailable" || err.status === 502) {
      return "The issuer could not be reached or refused the request — try again in a moment.";
    }
    if (err.status === 429) {
      return "Too many sign-ins are in flight — wait for one to settle and try again.";
    }
    if (err.status === 503 || err.code === "daemon_unavailable") {
      return "The board's daemon is unreachable — try again once it answers.";
    }
    return err.message || `Sign-in failed (${err.status}).`;
  }
  return "Sign-in failed — try again.";
}
