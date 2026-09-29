import type {
  AppBinding,
  Connection,
  Installation,
  SlotDeclaration,
} from "./workspaceApps";

/**
 * The installed-App slot-binding view (CAD-585) — which declared slots
 * an installation has, which connections are reviewed-compatible with
 * each, and whether a saved binding is still healthy. Kept pure so the
 * matching rules are unit-tested in plain node
 * (tests/slotBindings.test.ts).
 *
 * Compatibility mirrors the daemon's exact check
 * (`app_binding_config`): a candidate must map the declared
 * capability/version/action/resource-kind with the same effect, on a
 * matched deployment pin, with its credential in custody. The board
 * never invents a match the daemon would refuse — the typed
 * create/update calls enforce it again server-side.
 */

export interface DeclaredSlot {
  slot: string;
  /** Null for untyped legacy `needs.connections` slots. */
  declaration: SlotDeclaration | null;
}

/** Every slot the app declares: typed capability slots, then untyped legacy ones. */
export function declaredSlots(installation: Installation): DeclaredSlot[] {
  const capabilities = installation.capabilities ?? {};
  const typed = Object.entries(capabilities).map(([slot, declaration]) => ({
    slot,
    declaration,
  }));
  const known = new Set(typed.map((s) => s.slot));
  const untyped = (installation.connection_slots ?? [])
    .filter((slot) => !known.has(slot))
    .map((slot) => ({ slot, declaration: null }));
  return [...typed, ...untyped];
}

function mappingMatches(mapping: {
  capability: string;
  version: number;
  action: string;
  resource_kind: string;
  effect: string;
}, declaration: SlotDeclaration): boolean {
  return (
    mapping.capability === declaration.capability &&
    mapping.version === declaration.version &&
    mapping.action === declaration.action &&
    mapping.resource_kind === declaration.resource_kind &&
    mapping.effect === declaration.effect
  );
}

function isLive(row: Connection): boolean {
  return (
    row.status?.adapter_registered === true &&
    row.status?.custody_available === true &&
    row.status?.manifest_status === "matched"
  );
}

/**
 * The connections a slot may bind, most local first: usable rows whose
 * reviewed action mappings cover the declared contract exactly. Untyped
 * legacy slots accept any live connection. Withheld rows are counted,
 * not shown — Settings → Connections says why.
 */
export function candidatesFor(
  declaration: SlotDeclaration | null,
  connections: Connection[],
): { offered: Connection[]; withheld: number } {
  const offered = connections
    .filter((row) => {
      if (!isLive(row)) return false;
      if (declaration === null) return row.descriptor !== null;
      return (row.descriptor?.action_mappings ?? []).some((mapping) =>
        mappingMatches(mapping, declaration),
      );
    })
    .sort((a, b) => {
      const local = Number(isLocalOutbox(b)) - Number(isLocalOutbox(a));
      if (local !== 0) return local;
      const byProvider = a.provider.localeCompare(b.provider);
      if (byProvider !== 0) return byProvider;
      return a.account.localeCompare(b.account);
    });
  return { offered, withheld: connections.length - offered.length };
}

/** Is this the built-in Local outbox — always present, provider local. */
export function isLocalOutbox(row: Pick<Connection, "provider" | "account">): boolean {
  return row.provider === "local" && row.account === "local";
}

/** The connection's plain name: "Local outbox", else `provider · account`. */
export function connectionLabel(row: Pick<Connection, "provider" | "account">): string {
  if (isLocalOutbox(row)) return "Local outbox";
  return `${row.provider} · ${row.account}`;
}

const EFFECT_WORDS: Record<string, string> = {
  read: "Read",
  draft: "Draft",
  send: "Publish",
};

/** One slot's requirement in plain words — never a tool or scope name. */
export function plainRequirement(slot: DeclaredSlot): string {
  if (slot.declaration === null)
    return "Any live connection (this app names no reviewed capability for this slot).";
  const { capability, version, effect } = slot.declaration;
  const verb = EFFECT_WORDS[effect] ?? effect;
  return `${verb} via ${capability} v${version}.`;
}

export type BindingHealth =
  | "ok"
  | "stale-bundle"
  | "missing-connection"
  | "not-configured";

/**
 * Is the saved binding still the installation's truth: pinned to the
 * current bundle, pointing at a connection that still exists, in the
 * configured state. Anything else must say so on screen — a binding
 * that silently stopped resolving is a rebind waiting to happen.
 */
export function bindingHealth(
  binding: AppBinding | undefined,
  installationDigest: string,
  connections: Connection[],
): BindingHealth {
  if (binding === undefined) return "not-configured";
  if (binding.state !== "configured") return "not-configured";
  if (binding.config.bundle_digest !== installationDigest) return "stale-bundle";
  if (!connections.some((row) => row.id === binding.config.connection_id))
    return "missing-connection";
  return "ok";
}

/** The binding for one slot in this context, pinned to the current bundle. */
export function bindingForSlot(
  bindings: AppBinding[],
  contextId: string | null,
  slot: string,
): AppBinding | undefined {
  return bindings.find(
    (value) => value.context_id === contextId && value.slot === slot,
  );
}
