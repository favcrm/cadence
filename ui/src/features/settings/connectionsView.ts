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

/** Can this provider enroll a token — the only shape the wizard offers. */
export function acceptsToken(provider: ConnectionProvider): boolean {
  return (provider.descriptor?.enrollment_shapes ?? []).includes("token");
}

/** The wizard's scope hint: the union of the reviewed capability scopes. */
export function scopeHint(provider: ConnectionProvider): string[] {
  const seen = new Set<string>();
  for (const cap of provider.descriptor?.capabilities ?? []) {
    for (const scope of cap.scopes ?? []) seen.add(scope);
  }
  return [...seen].sort();
}
