import { ApiError } from "../../lib/api";

/**
 * CAD-1839 hosted connect. The worker's success envelope is exactly
 * `{ok:true, hosted:true, url:<https>}`; any other shape — a falsy flag,
 * a missing or relative URL, a non-https scheme — is a refusal, never a
 * link to open. The credential and OAuth state never reach this board.
 */
export const UI_HOSTED_REFUSED =
  "The deployment did not return a secure sign-in link — nothing was changed.";
const UI_HOSTED_UNAVAILABLE =
  "Hosted connections are not available for this service here — nothing was changed.";
const UI_HOSTED_FAILED =
  "Could not start the hosted sign-in. Reload the board and try again — nothing was changed.";

export function hostedConnectUrl(reply: unknown): string | null {
  if (reply === null || typeof reply !== "object") return null;
  const body = reply as { ok?: unknown; hosted?: unknown; url?: unknown };
  if (body.ok !== true || body.hosted !== true || typeof body.url !== "string") return null;
  try {
    const url = new URL(body.url);
    return url.protocol === "https:" ? url.toString() : null;
  } catch {
    return null;
  }
}

export function hostedConnectError(e: unknown): string {
  // 404 is the deployment saying this provider has no hosted connect
  // route; any other refusal keeps the generic retry line. Raw server
  // text is never rendered on this path.
  return e instanceof ApiError && e.status === 404 ? UI_HOSTED_UNAVAILABLE : UI_HOSTED_FAILED;
}
