import type { Connection, ConnectionProvider } from "../../lib/types";
import { isLocalOutbox } from "../../lib/connections";

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
    return { shapes: [], reason: "This provider is not registered on this daemon." };
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

// CAD-1166: no provider offers a reviewed no-send verification through
// the approved broker yet (CAD-1065/CAD-1085 own that contract). The
// page reports unsupported honestly and never turns the local metadata
// check into a network result.
export function verificationSupport(_provider: ConnectionProvider | null): {
  supported: boolean;
  reason: string;
} {
  return {
    supported: false,
    reason:
      "Remote login verification is not available on this daemon yet — no reviewed verification operation is offered for this service.",
  };
}
