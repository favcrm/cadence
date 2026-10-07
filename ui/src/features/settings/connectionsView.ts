import type { Connection, ConnectionProvider } from "../../lib/types";
import { isLocalOutbox } from "../../lib/connections";
import { fmtTime } from "../../lib/fmt";

/**
 * The Settings Connections page's view of provider connections
 * (CAD-585) — the adapters between the `/api/connection-providers` and
 * `/api/connections` rows and what the page and the app pickers need,
 * kept pure so the rules are unit-tested in plain node
 * (tests/connections.test.ts).
 *
 * Every helper reads metadata only: rows never carry secret bytes, and
 * nothing here stores a token.
 */

/** Is this connection usable — registered, described and in custody? */
export function isAvailable(row: Connection): boolean {
  return (
    row.status.adapter_registered === true &&
    row.status.descriptor_available === true &&
    row.status.custody_available === true
  );
}

/** Can the operator rotate or revoke it — only enrolled credentials. */
export function canManage(row: Pick<Connection, "kind">): boolean {
  return row.kind === "enrolled";
}

/** The deployment pin in plain words — a declaration, never a probe. */
export function pinWord(status: Pick<Connection["status"], "manifest_status">): string {
  switch (status.manifest_status) {
    case "matched":
      return "reviewed contract matches the deployment";
    case "missing":
      return "deployment has no reported contract to compare";
    default:
      return "deployment differs from the reviewed contract";
  }
}

/** The connection's readiness in one plain line. */
export function readinessText(row: Connection): string {
  if (isLocalOutbox(row)) return "Built-in — always present, no credential to manage.";
  if (!row.status.adapter_registered) return "Provider is not registered on this daemon.";
  if (!row.status.descriptor_available)
    return "Provider has no reviewed descriptor — unavailable.";
  if (row.kind === "enrolled" && !row.status.custody_available)
    return "Credential is missing from custody.";
  return pinWord(row.status);
}

/** The provider's capabilities in plain words — "blog.publish, social.post". */
export function capabilityWords(provider: ConnectionProvider): string | null {
  const caps = provider.descriptor?.capabilities ?? [];
  if (caps.length === 0) return null;
  return caps.map((c) => c.id).join(", ");
}

/** One connection's provider capabilities, or null when unknown. */
export function connectionCapabilities(
  providers: ConnectionProvider[],
  providerName: string,
): string | null {
  const found = providers.find((p) => p.provider === providerName);
  return found ? capabilityWords(found) : null;
}

/** Can this provider enroll a token — one shape the wizard offers. */
export function acceptsToken(provider: ConnectionProvider): boolean {
  return (provider.descriptor?.enrollment_shapes ?? []).includes("token");
}

/** Can this provider enroll a typed SMTP sender (CAD-785). */
export function acceptsSmtp(provider: ConnectionProvider): boolean {
  return (provider.descriptor?.enrollment_shapes ?? []).includes("smtp");
}

/** The wizard's enrollment shapes for one provider, in stable order. */
export function enrollmentShapes(provider: ConnectionProvider): string[] {
  const shapes = provider.descriptor?.enrollment_shapes ?? [];
  return shapes.filter((s) => s === "token" || s === "smtp");
}

/**
 * Is this connection an SMTP sender? Decided from the daemon's shape
 * flag; `smtp` presence is only the fallback for an older daemon.
 */
export function isSmtpSender(row: Connection): boolean {
  return row.smtp_sender === true || (row.smtp ?? null) !== null;
}

/** An SMTP sender whose settings the daemon could not project. */
export function smtpUnreadable(row: Connection): boolean {
  return isSmtpSender(row) && (row.smtp ?? null) === null;
}

/** Plain-language reason and fix for an unreadable SMTP sender. */
export function smtpErrorMessage(row: Connection): string | null {
  if (!smtpUnreadable(row)) return null;
  if (row.smtp_error === "withheld_leak") {
    return "Settings are hidden: your password shares characters with the host, username or sender. Rotate it and choose a different password or app password.";
  }
  return "Settings can't be read. Rotate it to re-enter them.";
}

/** One enrolled SMTP sender in plain words — transport and verified sender, never the secret. */
export function smtpSummary(row: Connection): string | null {
  const smtp = row.smtp ?? null;
  if (!smtp) return null;
  const mode = smtp.tls_mode === "implicit" ? "implicit TLS" : "STARTTLS";
  const name = smtp.sender_name ? `"${smtp.sender_name}" ` : "";
  return `${smtp.host}:${smtp.port} · ${mode} · ${name}<${smtp.sender}> · login ${smtp.username}`;
}

/**
 * Mirrors the daemon's `validate_port_tls` (src/platform/smtp.rs):
 * a public host submits on exactly (465, implicit) or (587, starttls);
 * the isolated-test host `localhost` may bind any port — the rig uses
 * ephemeral loopback ports — but the TLS mode stays mandatory either
 * way. There is no plaintext mode on any host or port.
 */
export function smtpPortTlsError(host: string, port: string, tlsMode: string): string | null {
  if (tlsMode !== "implicit" && tlsMode !== "starttls") {
    return "SMTP submission requires implicit TLS or mandatory STARTTLS.";
  }
  if (!/^\d+$/.test(port)) {
    return "SMTP port is a number between 1 and 65535.";
  }
  const n = Number(port);
  if (n < 1 || n > 65535) {
    return "SMTP port is a number between 1 and 65535.";
  }
  const normalized = host.endsWith(".") ? host.slice(0, -1) : host;
  if (normalized === "localhost") return null;
  if ((n === 465 && tlsMode === "implicit") || (n === 587 && tlsMode === "starttls")) return null;
  return "Port 465 pairs with implicit TLS; port 587 pairs with STARTTLS.";
}

/** The wizard's scope hint: the union of the reviewed capability scopes. */
export function scopeHint(provider: ConnectionProvider): string[] {
  const seen = new Set<string>();
  for (const cap of provider.descriptor?.capabilities ?? []) {
    for (const scope of cap.scopes ?? []) seen.add(scope);
  }
  return [...seen].sort();
}

// CAD-1166 — mockup C grouping. Group identity is the canonical
// provider name; the label is presentation only.
export interface ServiceGroup {
  provider: string;
  label: string;
  info: ConnectionProvider | null;
  builtin: boolean;
  connections: Connection[];
}

export function serviceLabel(providerName: string): string {
  if (providerName === "smtp") return "Email (SMTP)";
  if (providerName === "agenticos" || providerName === "agenticos_external")
    return "AgenticOS";
  if (providerName === "local") return "Local outbox";
  return providerName;
}

export function serviceGroups(
  providers: ConnectionProvider[],
  connections: Connection[],
): ServiceGroup[] {
  const registered = new Map(providers.map((p) => [p.provider, p]));
  const sorted = [...connections].sort(
    (a, b) => a.account.localeCompare(b.account) || a.id.localeCompare(b.id),
  );
  const groups = new Map<string, ServiceGroup>();
  const groupFor = (name: string): ServiceGroup => {
    const known = groups.get(name);
    if (known) return known;
    const info = registered.get(name) ?? null;
    const g: ServiceGroup = {
      provider: name,
      label: serviceLabel(name),
      info,
      builtin: false,
      connections: [],
    };
    groups.set(name, g);
    return g;
  };
  for (const row of sorted) groupFor(row.provider).connections.push(row);
  for (const p of providers) groupFor(p.provider);
  const out = [...groups.values()];
  for (const g of out) {
    g.builtin =
      (g.info === null || enrollmentShapes(g.info).length === 0) &&
      g.connections.length > 0 &&
      g.connections.every((c) => c.kind === "builtin");
  }
  out.sort(
    (a, b) =>
      Number(a.provider === "local") - Number(b.provider === "local") ||
      a.provider.localeCompare(b.provider),
  );
  return out;
}

// Why a provider cannot enroll, when it cannot.
export function enrollmentSupport(provider: ConnectionProvider | null): {
  shapes: string[];
  reason: string;
} {
  if (provider === null) {
    return {
      shapes: [],
      reason:
        "This provider's metadata could not be loaded — refresh services before assuming it was removed.",
    };
  }
  if (!provider.descriptor_available || provider.descriptor === null) {
    return {
      shapes: [],
      reason: "This provider has no reviewed descriptor — enrollment is unavailable.",
    };
  }
  const shapes = enrollmentShapes(provider);
  if (shapes.length === 0) {
    return {
      shapes,
      reason: "This provider declares no reviewed enrollment — there is nothing to set up.",
    };
  }
  return { shapes, reason: "" };
}

// CAD-1166: a provider offers the approved SMTP no-send login check only
// when its trusted `verification_support` metadata advertises the
// reviewed `smtp-login-no-send-v1` operation as supported (CAD-1065/
// CAD-1085 own that contract). Support is never inferred from the
// provider name, descriptor shape or a local configuration read — a
// missing, unknown or refused declaration stays unavailable with a
// fixed safe reason, never the raw reason_code.
export function verificationSupport(provider: ConnectionProvider | null): {
  supported: boolean;
  reason: string;
} {
  const vs = provider?.verification_support;
  if (
    provider !== null &&
    vs !== null &&
    typeof vs === "object" &&
    vs.operation === "smtp-login-no-send-v1" &&
    vs.supported === true
  ) {
    return { supported: true, reason: "" };
  }
  if (vs !== null && typeof vs === "object" && typeof vs.reason_code === "string") {
    const byReason: Record<string, string> = {
      unsupported_provider:
        "Remote login verification is not offered for this service — only reviewed SMTP senders can be tested.",
      unsupported_deployment:
        "Remote login verification is not offered for this deployment — it is unavailable on a hosted relay or image-composed workspace.",
      unsupported_custody:
        "Remote login verification is not offered for how this credential is stored — the credential's custody mode cannot be verified.",
      unavailable:
        "Remote login verification is not available on this daemon yet — the service's verification metadata could not be trusted.",
    };
    return {
      supported: false,
      reason: byReason[vs.reason_code] ??
        "Remote login verification is not offered for this service.",
    };
  }
  return {
    supported: false,
    reason:
      "Remote login verification is not available on this daemon yet — no reviewed verification operation is offered for this service.",
  };
}

// CAD-1065/CAD-1085: the closed failure code → a fixed, actionable,
// secret-free line. Raw provider/credential/tool/request text and
// diagnostic fragments never reach the result — only the reviewed
// mapping below is rendered.
const VERIFY_FAILURE_TEXT: Record<string, string> = {
  unsupported_provider:
    "This connection's service cannot be verified remotely — only reviewed SMTP senders support the login test.",
  unsupported_deployment:
    "This connection's deployment cannot be verified remotely — the login test is unavailable on a hosted relay or image-composed workspace.",
  unsupported_custody:
    "This credential's storage cannot be verified remotely — rotate it or check how the credential is stored.",
  stale_connection:
    "This connection changed since the check started — refresh it and test again.",
  configuration_unavailable:
    "This connection's saved configuration can't be read — fix the saved configuration or update credentials, then test again.",
  custody_unavailable:
    "This connection's credential is unavailable in custody — fix the credential storage or update credentials, then test again.",
  busy: "The verification service is busy — wait a moment and try the test again.",
  dns_failed:
    "The server address could not be resolved — check the server details and try the test again.",
  destination_refused:
    "The server refused the connection — check the server address and port, then test again.",
  connect_failed:
    "The server could not be reached — check the server details and network, then test again.",
  tls_failed:
    "The secure connection could not be established — check the security settings (port 465 / implicit TLS, or 587 / STARTTLS), then test again.",
  auth_failed:
    "The login was refused — check the username and password and update credentials, then test again.",
  timeout: "The check timed out — try the test again.",
  quit_failed:
    "The login did not complete cleanly — the connection could not be fully verified. Try the test again.",
  verification_failed:
    "The connection could not be verified — an unexpected error stopped the check. Try the test again.",
};

const VERIFY_FAILURE_CODES = new Set([
  "unsupported_provider", "unsupported_deployment", "unsupported_custody",
  "stale_connection", "configuration_unavailable", "custody_unavailable",
  "busy", "dns_failed", "destination_refused", "connect_failed", "tls_failed",
  "auth_failed", "timeout", "quit_failed", "verification_failed",
]);
const VERIFY_FAILURE_STEPS = new Set([
  "admission", "configuration", "dns", "connect", "tls", "auth", "quit",
]);
const VERIFY_STATUSES = new Set(["success", "failed", "unsupported", "stale"]);

// A canonical registration digest: `sha256:` + 64 lowercase hex, or null.
function canonicalDigest(v: unknown): v is string {
  return v === null || (typeof v === "string" && /^sha256:[0-9a-f]{64}$/.test(v));
}
// A valid credential revision: a positive integer, or null.
function positiveRevision(v: unknown): v is number {
  return v === null || (typeof v === "number" && Number.isInteger(v) && v > 0);
}
// A valid UTC RFC3339 timestamp — strict shape plus a parseable instant.
function validTimestamp(v: unknown): v is string {
  return (
    typeof v === "string" &&
    /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z$/.test(v) &&
    Number.isFinite(Date.parse(v))
  );
}

export interface VerificationOutcome {
  ok: boolean;
  stale: boolean;
  text: string;
  checkedAt: string | null;
}

/**
 * Validate a raw `POST /api/connections/<id>/test` body against the
 * closed CAD-1065 receipt schema and the identity it was requested for.
 * Anything that is not exactly the reviewed receipt — a partial/unknown
 * shape, a closed-enum violation, an email/delivery/sender/execution
 * flag that is not exactly `false`, or a receipt for a different
 * connection — returns null so a malformed response can never become a
 * shown success. A `stale` receipt carries the SERVER-current
 * revision/digest (which legitimately differs from the request), so the
 * tuple check is relaxed only for its own id and the text is a fixed
 * refresh-and-test-again line — the stale receipt never becomes
 * replacement-current evidence.
 */
export function verificationResult(
  input: unknown,
  expected: Pick<Connection, "id" | "revision" | "registration_digest">,
): VerificationOutcome | null {
  const v =
    input !== null && typeof input === "object"
      ? (input as { verification?: unknown }).verification
      : null;
  if (v === null || typeof v !== "object") return null;
  const r = v as Record<string, unknown>;
  // Exact closed shape — every required field present, no extras.
  const RECEIPT_KEYS = new Set([
    "schema", "operation", "connection_id", "revision", "registration_digest",
    "started_at", "completed_at", "status", "network_attempted",
    "authentication_verified", "email_sent", "delivery_verified",
    "sender_entitlement_verified", "execution_authority", "failure",
  ]);
  for (const k of Object.keys(r)) if (!RECEIPT_KEYS.has(k)) return null;
  if (
    r.schema !== 1 ||
    r.operation !== "smtp-login-no-send-v1" ||
    typeof r.connection_id !== "string" ||
    r.connection_id !== expected.id ||
    !VERIFY_STATUSES.has(r.status as string) ||
    r.network_attempted !== true && r.network_attempted !== false ||
    r.authentication_verified !== true && r.authentication_verified !== false ||
    r.email_sent !== false ||
    r.delivery_verified !== false ||
    r.sender_entitlement_verified !== false ||
    r.execution_authority !== false ||
    !validTimestamp(r.started_at) ||
    !validTimestamp(r.completed_at) ||
    !positiveRevision(r.revision) ||
    !canonicalDigest(r.registration_digest)
  ) {
    return null;
  }
  const failure = r.failure;
  if (failure !== null) {
    if (typeof failure !== "object" || Array.isArray(failure)) return null;
    const f = failure as Record<string, unknown>;
    const FAIL_KEYS = new Set(["code", "step"]);
    for (const k of Object.keys(f)) if (!FAIL_KEYS.has(k)) return null;
    if (!VERIFY_FAILURE_CODES.has(f.code as string) || !VERIFY_FAILURE_STEPS.has(f.step as string)) {
      return null;
    }
  }
  const status = r.status as "success" | "failed" | "unsupported" | "stale";
  const digest = r.registration_digest as string | null;
  const revision = r.revision as number | null;

  if (status === "success") {
    // Success is pinned to the exact captured tuple — a different
    // current identity makes the receipt stale, not a success.
    if (
      r.authentication_verified !== true ||
      failure !== null ||
      revision !== expected.revision ||
      digest !== expected.registration_digest
    ) {
      return null;
    }
    return {
      ok: true,
      stale: false,
      checkedAt: r.completed_at as string,
      text:
        `Login verified at ${fmtTime(r.completed_at as string)} — no email was sent. ` +
        "This does not confirm delivery, sender entitlement or execution authority.",
    };
  }
  if (status === "stale") {
    // The server reports it is stale relative to the current identity —
    // refresh and test again; never a success and never the server-
    // current tuple presented as proof of the request's connection.
    return {
      ok: false,
      stale: true,
      checkedAt: r.completed_at as string,
      text:
        "The connection changed while the check ran — refresh it and test again. This result is not proof of the current connection.",
    };
  }
  // failed / unsupported: describe the exact captured tuple, closed
  // code+step mapping only — no provider/request/diagnostic text.
  if (failure === null) {
    // unsupported carries no failure detail; failed always does.
    if (status === "unsupported") {
      return {
        ok: false,
        stale: false,
        checkedAt: r.completed_at as string,
        text: "Remote login verification is not supported for this connection.",
      };
    }
    return null;
  }
  const code = (failure as { code: string }).code;
  return {
    ok: false,
    stale: false,
    checkedAt: r.completed_at as string,
    text:
      VERIFY_FAILURE_TEXT[code] ??
      "The connection could not be verified — an unexpected error stopped the check. Try the test again.",
  };
}
