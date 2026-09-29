/**
 * App-neutral connection identity words (CAD-585) — shared by Settings
 * Connections and the installed-App slot picker so the Local outbox
 * reads the same everywhere. Metadata only; rows never carry secrets.
 */
export interface ConnectionRef {
  provider: string;
  account: string;
}

/** Is this the built-in Local outbox — always present, provider local. */
export function isLocalOutbox(row: ConnectionRef): boolean {
  return row.provider === "local" && row.account === "local";
}

/** The connection's plain name: "Local outbox", else `provider · account`. */
export function connectionLabel(row: ConnectionRef): string {
  if (isLocalOutbox(row)) return "Local outbox";
  return `${row.provider} · ${row.account}`;
}
