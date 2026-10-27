import type { CredentialRequest } from "../../lib/types";

/**
 * CAD-1085: presentation rules for the trusted credential-entry surface.
 *
 * Everything here reads *server metadata only*. A request row carries no
 * credential, fragment, digest or human-typed transport/username value, and
 * a model-generated string is never input: the chat card renders the request
 * object the authenticated daemon returned, and the form's fields exist only
 * in the entry page's component state.
 */

export type CredentialVerb = CredentialRequest["verb"];

/** The one-line intent of the request. */
export function verbTitle(verb: CredentialVerb): string {
  switch (verb) {
    case "enroll":
      return "Add a credential";
    case "rotate":
      return "Replace a credential";
    case "verify":
      return "Verify a sign-in";
  }
}

/** The submit button's word. Verify is explicit — never an implicit save. */
export function verbAction(verb: CredentialVerb): string {
  switch (verb) {
    case "enroll":
      return "Save credential";
    case "rotate":
      return "Replace credential";
    case "verify":
      return "Verify now";
  }
}

/** Whether the trusted form collects a credential value. */
export function allowsSecretInput(verb: CredentialVerb): boolean {
  return verb !== "verify";
}

/** Fresh SMTP enrollment collects its transport/sender on the isolated form. */
export function needsSmtpTransport(request: Pick<CredentialRequest, "verb" | "shape">): boolean {
  return request.verb === "enroll" && request.shape === "smtp";
}

/** A request may still accept one form submission. */
export function isCollectable(
  request: Pick<CredentialRequest, "state" | "expires_at_ms">,
  now = Date.now(),
): boolean {
  return request.state === "pending" && now < request.expires_at_ms;
}

export type CredentialTone = "pending" | "done" | "problem" | "quiet";

/** How the status reads — never "verified" from a stored flag. */
export function requestTone(
  request: Pick<CredentialRequest, "state" | "status">,
): CredentialTone {
  if (request.state === "pending" || request.state === "submitting") return "pending";
  if (request.state === "done") return "done";
  if (request.state === "cancelled" || request.state === "expired") return "quiet";
  return "problem";
}

/** A native model source is authenticated only by an authorized model turn:
 *  storing the key is not a provider sign-in. */
export function isNativeSource(request: {
  shape?: CredentialRequest["shape"];
}): boolean {
  return (
    request.shape === "native-model-api-key" ||
    request.shape === "native-model-devin-credentials"
  );
}

/** Native provider verification is unsupported: the broker refuses
 *  `unsupported_verification`, and a stored key is not a signed-in state. */
export function nativeVerifyUnsupported(
  request: Pick<CredentialRequest, "verb" | "shape">,
): boolean {
  return request.verb === "verify" && isNativeSource(request);
}

/** The status in plain words. Never calls a stored native key verified. */
export function stateLabel(
  request: Pick<CredentialRequest, "state" | "status"> &
    Partial<Pick<CredentialRequest, "shape">>,
): string {
  switch (request.state) {
    case "pending":
      return "Waiting for you";
    case "submitting":
      return "Sending once…";
    case "done":
      if (request.status === "unknown") return "Outcome unknown — do not resubmit";
      if (request.status === "refused") return "Credential refused";
      if (request.status === "verified") {
        return isNativeSource(request)
          ? "Credential saved — sign-in not verified"
          : "Sign-in verified";
      }
      return "Credential saved";
    case "cancelled":
      return "Cancelled";
    case "expired":
      return "Expired";
    case "unknown":
      return "Outcome unknown — do not resubmit";
  }
}

/**
 * Fixed copy per closed wire code. The server already sends only fixed
 * messages; mapping again keeps a future regression from surfacing raw
 * daemon text (which can name an account or host) in the browser.
 */
export const CREDENTIAL_ERROR: Record<string, string> = {
  entry_session_required:
    "This page needs the signed-in operator's browser session on the trusted entry origin. Sign in there, then reopen the entry link.",
  wrong_entry_origin:
    "This form exists only on the trusted credential origin. Open the link the request names; nothing was collected here.",
  unsupported_deployment:
    "This deployment has no separate trusted credential origin, so credential entry is unsupported. Nothing can be collected on this board.",
  request_not_ready:
    "Protected credential entry is not ready on this daemon. Nothing was collected.",
  already_submitted: "This form was already submitted. It cannot be sent twice.",
  nonce_expired: "This form expired. Reopen the entry page for a fresh one.",
  request_changed:
    "This request no longer matches the connection or custody state. Start a new request.",
  unsupported_verification:
    "This provider has no reviewed verification adapter. Nothing was checked.",
  outcome_unknown:
    "The outcome is unknown. Do not submit again; reconcile the request's status.",
  refused: "The credential was refused. Nothing more was changed.",
  management_forbidden:
    "Credential entry is the signed-in operator's action; this request is not.",
  invalid_request: "This form could not be accepted. Nothing was sent.",
  entry_unavailable:
    "Protected credential entry is unavailable. Nothing was collected.",
};

/** The fixed message for a closed code (or the shared fallback). */
export function errorCopy(code: string | null | undefined): string {
  if (code && CREDENTIAL_ERROR[code]) return CREDENTIAL_ERROR[code];
  return CREDENTIAL_ERROR.entry_unavailable;
}

/** The trusted top-level page for one request — the absolute URL the
 *  server attached (pinned origin plus selector only). `null` means this
 *  deployment has no separate trusted origin; callers must report entry as
 *  unsupported and must never navigate to a board-relative path. */
export function entryHref(request: Pick<CredentialRequest, "entry_url">): string | null {
  const url = request.entry_url ?? null;
  return url !== null && /^https:\/\/[^/?#]+\/connections\/enroll\/[0-9a-f-]{36}$/.test(url)
    ? url
    : null;
}

/** Leave the board for the trusted origin. Returns false when no
 *  server-selected pinned URL exists (nothing is navigated). */
export function openTrustedEntry(request: Pick<CredentialRequest, "entry_url">): boolean {
  const url = entryHref(request);
  if (url === null) return false;
  window.location.assign(url);
  return true;
}

/** A short, non-secret description of the request's target. */
export function targetLabel(request: Pick<CredentialRequest, "verb" | "display_alias" | "provider">): string {
  return request.verb === "enroll"
    ? `${request.provider} · ${request.display_alias}`
    : `${request.provider} · ${request.display_alias}`;
}

/** Requests a conversation should still show as actionable. */
export function actionableRequests(requests: CredentialRequest[]): CredentialRequest[] {
  return requests.filter((request) => request.state === "pending" || request.state === "submitting");
}
