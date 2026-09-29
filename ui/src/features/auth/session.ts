import { ApiError } from "../../lib/api";
import { sessionHeaders, setSessionKey } from "../../lib/sessionKey";
import { parseLoginNonce } from "./gate";

/**
 * The login link's nonce, taken out of the address bar before anything
 * renders: the fragment is stripped at once (`history.replaceState`), so
 * it is not left in the URL, the history entry or a screenshot. Read
 * once by the login screen.
 */
let captured: string | null = null;

export function captureLoginNonce(): void {
  if (location.pathname !== "/login" || !location.hash) return;
  captured = parseLoginNonce(location.hash);
  history.replaceState(null, "", "/login");
}

export function takeLoginNonce(): string | null {
  const nonce = captured;
  captured = null;
  return nonce;
}

async function sessionPost(path: string, body: object): Promise<unknown> {
  const resp = await fetch(path, {
    method: "POST",
    headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
    body: JSON.stringify(body),
  });
  const parsed = await resp.json().catch(() => null);
  if (!resp.ok) {
    throw new ApiError(parsed?.error ?? `${resp.status} ${resp.statusText}`, resp.status, parsed ?? undefined);
  }
  return parsed;
}

/**
 * `POST /api/session` — exchange the nonce for the session: the server
 * sets the HttpOnly cookie and answers `{session_key}`, which this tab
 * keeps in `sessionStorage` and sends on every request.
 */
export async function openSession(nonce: string): Promise<void> {
  const out = (await sessionPost("/api/session", { nonce })) as { session_key?: unknown } | null;
  const key = typeof out?.session_key === "string" ? out.session_key : null;
  if (!key) throw new ApiError("the board answered no session key — upgrade the board", 500);
  setSessionKey(key);
}

/** The display block `POST /api/session/device/code` answers (CAD-777).
 *  `pending_id` is a server-side handle — never logged or rendered raw
 *  beyond its poll use. */
export interface DeviceDisplay {
  pending_id: string;
  user_code: string;
  verification_uri: string;
  verification_uri_complete?: string;
  expires_in: number;
  interval: number;
}

/** A device poll's outcome: the status the server reported, or
 *  `signed_in` once the session key was set. */
export type DevicePollOutcome =
  | { kind: "signed_in" }
  | { kind: "pending" | "slow_down" | "denied" | "expired" };

/** `POST /api/session/device/code` — start a device grant: the board
 *  asks the configured issuer and answers the human's display block. */
export async function startDeviceSignIn(): Promise<DeviceDisplay> {
  const out = (await sessionPost("/api/session/device/code", {})) as DeviceDisplay | null;
  if (!out?.pending_id || !out.user_code || !out.verification_uri) {
    throw new ApiError("the board answered an incomplete device grant — upgrade the board", 502);
  }
  return out;
}

/** `POST /api/session/device/poll` — one exchange of a pending grant.
 *  On approval the board has set the cookie; the answered
 *  `session_key` is this tab's half of the session. Refusals come back
 *  as `ApiError` (e.g. `code` `device_subject_not_allowed`, `check`
 *  `session_from_agent`). */
export async function pollDeviceSignIn(pendingId: string): Promise<DevicePollOutcome> {
  const out = (await sessionPost("/api/session/device/poll", { pending_id: pendingId })) as
    | { session_key?: unknown; status?: unknown }
    | null;
  const key = typeof out?.session_key === "string" ? out.session_key : null;
  if (key) {
    setSessionKey(key);
    return { kind: "signed_in" };
  }
  const status = typeof out?.status === "string" ? out.status : "expired";
  if (status === "pending" || status === "slow_down" || status === "denied" || status === "expired") {
    return { kind: status };
  }
  return { kind: "expired" };
}

/** `POST /api/session/logout` — end this browser's session. */
export async function closeSession(): Promise<void> {
  try {
    await sessionPost("/api/session/logout", {});
  } finally {
    setSessionKey(null);
  }
}
