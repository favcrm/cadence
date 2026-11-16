import { ApiError } from "../../../lib/apiError";

/**
 * The one error-to-message layer every CRM list shares (CAD-1009).
 * A 401 confirms the session is unavailable; an opaque redirect or network
 * error does not distinguish an old sign-in redirect from a connection fault.
 * A message is always a string: an object is never stringified.
 */
export const SESSION_EXPIRED_TEXT =
  "Your session ended. Reload to sign in; unsent changes may be lost.";
export const CONNECTION_UNAVAILABLE_TEXT =
  "We couldn't reach the workspace. Your session may still be active. Check your connection and try again.";

/** The daemon's bare "app <x> management refused or unavailable" line
 *  carries no reason: it is dropped, never shown glued to a message. */
const GENERIC_REFUSAL = /^app [a-z ]+? refused or unavailable[\s:.\u2014-]*/i;

export function isGenericRefusal(error: unknown): boolean {
  return error instanceof ApiError && GENERIC_REFUSAL.test(error.message);
}

export function hostErrorText(error: unknown, fallback: string): string {
  if (error instanceof ApiError) {
    if (error.status === 401) return SESSION_EXPIRED_TEXT;
    const message = error.message.replace(GENERIC_REFUSAL, "").trim();
    return message === "" || message.startsWith("[object ") ? fallback : message;
  }
  if (error instanceof TypeError) return CONNECTION_UNAVAILABLE_TEXT;
  if (error instanceof Error && error.message !== "") return error.message;
  if (typeof error === "string" && error !== "") return error;
  if (typeof error === "object" && error !== null) {
    const message = (error as { message?: unknown }).message;
    if (typeof message === "string" && message !== "") return message;
    // A bare object thrown by fetch (an opaque redirect) is a transport failure.
    return CONNECTION_UNAVAILABLE_TEXT;
  }
  return fallback;
}
