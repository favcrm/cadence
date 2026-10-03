import type {
  AppBinding,
  BindingChange,
  Connection,
  Installation,
  SlotDeclaration,
} from "./workspaceApps";
import { isLocalOutbox } from "../../lib/connections";

/**
 * The installed-App slot-binding view (CAD-585) — which declared slots
 * an installation has, which connections are reviewed-compatible with
 * each, and whether a saved binding is still healthy. Kept pure so the
 * matching rules are unit-tested in plain node
 * (tests/bindingChoices.test.ts).
 *
 * Compatibility mirrors the daemon's exact check
 * (`app_binding_config`): a candidate must map the declared
 * capability/version/action/resource-kind with the same effect, cover
 * the mapping's scopes when enrolled, sit on a matched deployment pin
 * with its credential in custody. The board never invents a match the
 * daemon would refuse — the typed create/update calls enforce it again
 * server-side.
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
      const mapping = (row.descriptor?.action_mappings ?? []).find((candidate) =>
        mappingMatches(candidate, declaration),
      );
      if (!mapping) return false;
      // Enrolled credentials must cover the reviewed action scopes, exactly
      // as the daemon's `app_binding_config` requires; built-ins carry none.
      if (row.kind === "enrolled") {
        const held = new Set(row.scopes ?? []);
        if (!mapping.scopes.every((scope) => held.has(scope))) return false;
      }
      return true;
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

/** Receipt fields that migrate without the operator (daemon allowlist). */
const BOOKKEEPING = new Set([
  "registration_digest",
  "sink_registration",
  "descriptor_revision",
  "reviewed_pin",
  "reported_pin",
  "mapping.tool",
]);

export interface SlotReadiness {
  slot: string;
  requirement: string;
  binding: AppBinding | undefined;
  connection: Connection | undefined;
  health: BindingHealth;
  custody: boolean;
  registered: boolean;
  /** CAD-1119: the slot contract changes awaiting the operator's confirm. */
  confirm: BindingChange[] | null;
  ready: boolean;
  nextAction: string;
}

/**
 * The installed-App Ready-to-run checklist (CAD-796, CAD-1119): one row
 * per typed capability slot with the plain next action for a real
 * blocker — an unbound or stale slot, an unhealthy or unregistered
 * connection, or a connection whose slot contract widened and awaits the
 * operator's confirm. Installing the app is its approval, and a provider
 * change that keeps the contract migrates silently, so neither is a
 * blocker. Legacy untyped slots never authorize an effect, so they stay
 * out of the list.
 */
export function readinessFor(
  installation: Installation,
  bindings: AppBinding[],
  connections: Connection[],
  contextId: string | null,
): SlotReadiness[] {
  return declaredSlots(installation)
    .filter((slot) => slot.declaration !== null)
    .map((slot) => {
      const binding = bindingForSlot(bindings, contextId, slot.slot, installation.digest);
      const health = bindingHealth(binding, installation.digest, connections);
      const connection = binding
        ? connections.find((row) => row.id === binding.config.connection_id)
        : undefined;
      const custody =
        connection?.status?.manifest_status === "matched" &&
        connection?.status?.custody_available === true;
      // Candidate selection already requires a registered provider adapter;
      // readiness must gate on it too, or a deregistered provider reads Ready.
      const registered = connection?.status?.adapter_registered === true;
      const drift = health === "ok" ? binding?.drift : undefined;
      // The confirm shows only what widened; provider bookkeeping that
      // would have migrated on its own is not the operator's decision.
      const confirm = drift?.state === "needs_confirm"
        ? (drift.changes ?? []).filter((change) => !BOOKKEEPING.has(change.field))
        : null;
      const unavailable = drift?.state === "unavailable";
      const ready = health === "ok" && custody && registered && !confirm && !unavailable;
      const nextAction = !binding || health === "not-configured"
        ? `Choose a ${slot.slot} connection below and save it.`
        : health === "stale-bundle"
          ? `Save the ${slot.slot} connection again for the current app version.`
          : health === "missing-connection"
            ? `Its connection is gone — choose another ${slot.slot} connection below.`
            : !custody
              ? `Its connection is unhealthy — see Settings → Connections, then rebind.`
              : !registered
                ? `Its connection's provider is no longer registered — see Settings → Connections, then rebind.`
                : confirm
                  ? `Its connection changed what this slot may do. Review the change and confirm it.`
                  : unavailable
                    ? `Its connection no longer fits this slot — see Settings → Connections, then rebind.`
                    : `Ready to run.`;
      return {
        slot: slot.slot,
        requirement: plainRequirement(slot),
        binding,
        connection,
        health,
        custody,
        registered,
        confirm,
        ready,
        nextAction,
      };
    });
}

/** The binding for one slot in this context, preferring the current bundle's pin. */
export function bindingForSlot(
  bindings: AppBinding[],
  contextId: string | null,
  slot: string,
  preferredDigest?: string,
): AppBinding | undefined {
  const rows = bindings.filter(
    (value) => value.context_id === contextId && value.slot === slot,
  );
  if (preferredDigest !== undefined) {
    const current = rows.find((value) => value.config.bundle_digest === preferredDigest);
    if (current) return current;
  }
  return rows[0];
}
