import {
  bindingForSlot,
  bindingHealth,
  candidatesFor,
  declaredSlots,
  plainRequirement,
  readinessFor,
} from "../src/features/workspace-apps/bindingChoices";
import type {
  AppBinding,
  Connection,
  Installation,
  SlotDeclaration,
} from "../src/features/workspace-apps/workspaceApps";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

function publication(): SlotDeclaration {
  return {
    schema: 1, capability: "text.publish", version: 1,
    action: "publish", resource_kind: "connection_account", effect: "send",
  };
}

function sourceRead(): SlotDeclaration {
  return {
    schema: 1, capability: "social.read", version: 1,
    action: "list_posts", resource_kind: "connection_account", effect: "read",
  };
}

function installation(): Installation {
  return {
    install_id: "install-a", name: "social-content", title: "Social Content",
    version: "0.5.0", summary: "", digest: "sha256:current",
    catalog_generation: "gen-1", storage_kind: "workspace",
    project_link: null, approved: true, executable: false,
    approval: { state: "approved" }, guide: "", files: [],
    capabilities: { publication: publication(), source: sourceRead() },
    connection_slots: ["cms", "publication"],
  };
}

function connection(over: Partial<Connection> = {}): Connection {
  return {
    id: "conn-a", provider: "agenticos_external", account: "ws_acme",
    kind: "enrolled", scopes: [],
    descriptor: { action_mappings: [] },
    status: { manifest_status: "matched", custody_available: true, adapter_registered: true },
    ...over,
  };
}

function localConnection(): Connection {
  return connection({
    id: "builtin-local", provider: "local", account: "local", kind: "builtin",
    descriptor: {
      action_mappings: [{
        capability: "text.publish", version: 1, action: "publish",
        resource_kind: "connection_account", effect: "send",
        semantics: "LocalMarkdownSink", scopes: [],
        input_contract: "in", output_contract: "out",
      }],
    },
  });
}

function sourceConnection(): Connection {
  return connection({
    id: "conn-source", account: "ws_source", scopes: ["sources"],
    descriptor: {
      action_mappings: [{
        capability: "social.read", version: 1, action: "list_posts",
        resource_kind: "connection_account", effect: "read",
        semantics: "MetadataRead", scopes: ["sources"],
        input_contract: "in", output_contract: "out",
      }],
    },
  });
}

function binding(over: Partial<AppBinding> = {}): AppBinding {
  return {
    id: "bind-a", install_id: "install-a", context_id: null,
    slot: "publication", revision: 2, state: "configured", digest: "sha256:bind",
    config: {
      bundle_digest: "sha256:current", connection_id: "builtin-local",
      provider: "local", account: "local", mapping: {
        capability: "text.publish", version: 1, action: "publish",
        resource_kind: "connection_account", effect: "send",
        semantics: "LocalMarkdownSink", scopes: ["publish"],
        input_contract: "in", output_contract: "out",
      },
    },
    ...over,
  };
}

// Declared slots: typed capabilities first, then untyped legacy ones not shadowed.
const slots = declaredSlots(installation());
equal(
  slots.map((s) => s.slot),
  ["publication", "source", "cms"],
  "slot order",
);
equal(slots[0].declaration?.capability, "text.publish", "typed declaration");
equal(slots[2].declaration, null, "legacy slot is untyped");
// An installation from a daemon that predates the slot projection binds nothing.
equal(declaredSlots({ ...installation(), capabilities: null, connection_slots: [] }), [], "no slots");

// Candidates match the reviewed contract exactly — not a shared label.
const rows = [sourceConnection(), localConnection()];
const pub = candidatesFor(publication(), rows);
equal(pub.offered.map((c) => c.id), ["builtin-local"], "publication keeps Local");
equal(pub.withheld, 1, "source connection withheld from publication");
const src = candidatesFor(sourceRead(), rows);
equal(src.offered.map((c) => c.id), ["conn-source"], "source read matches");
const legacy = candidatesFor(null, rows);
equal(legacy.offered.map((c) => c.id), ["builtin-local", "conn-source"], "legacy takes live, Local first");

// Version, effect, pin and custody mismatches never offer.
const wrongVersion = sourceConnection();
wrongVersion.id = "conn-v2";
wrongVersion.descriptor = {
  action_mappings: [{
    ...wrongVersion.descriptor!.action_mappings[0], version: 2,
  }],
};
const wrongEffect = sourceConnection();
wrongEffect.id = "conn-fx";
wrongEffect.descriptor = {
  action_mappings: [{
    ...wrongEffect.descriptor!.action_mappings[0], effect: "send",
  }],
};
const stalePin = sourceConnection();
stalePin.id = "conn-pin";
stalePin.status = { ...stalePin.status, manifest_status: "mismatched" };
const noCustody = sourceConnection();
noCustody.id = "conn-cust";
noCustody.status = { ...noCustody.status, custody_available: false };
const noDescriptor = connection({ id: "conn-nodesc", descriptor: null });
// An enrolled credential missing a reviewed action scope is withheld before Save.
const narrowScopes = sourceConnection();
narrowScopes.id = "conn-narrow";
narrowScopes.scopes = ["sources"];
narrowScopes.descriptor = {
  action_mappings: [{
    ...sourceConnection().descriptor!.action_mappings[0],
    scopes: ["sources", "admin"],
  }],
};
for (const bad of [wrongVersion, wrongEffect, stalePin, noCustody, noDescriptor, narrowScopes]) {
  const seen = candidatesFor(sourceRead(), [bad, sourceConnection()]);
  equal(seen.offered.map((c) => c.id), ["conn-source"], `withholds ${bad.id}`);
}

// Plain words, never a tool or scope.
equal(
  plainRequirement({ slot: "publication", declaration: publication() }),
  "Publish via text.publish v1.",
  "typed requirement",
);
equal(
  plainRequirement({ slot: "cms", declaration: null }),
  "Any live connection (this app names no reviewed capability for this slot).",
  "legacy requirement",
);

// Binding health: current truth, stale pins, vanished connections, dead states.
const all = [localConnection(), sourceConnection()];
equal(bindingHealth(undefined, "sha256:current", all), "not-configured", "absent");
equal(bindingHealth(binding(), "sha256:current", all), "ok", "healthy");
equal(
  bindingHealth(binding(), "sha256:other", all),
  "stale-bundle",
  "bundle moved",
);
equal(
  bindingHealth(binding({ config: { ...binding().config, connection_id: "conn-gone" } }), "sha256:current", all),
  "missing-connection",
  "connection vanished",
);
equal(
  bindingHealth(binding({ state: "revoked" }), "sha256:current", all),
  "not-configured",
  "revoked binding",
);
// Slot lookup is context-exact: the brand binding never answers for the context-free slot.
const brand = binding({ id: "bind-brand", context_id: "brand-a" });
equal(bindingForSlot([brand, binding()], null, "publication")?.id, "bind-a", "context-free");
equal(bindingForSlot([brand, binding()], "brand-a", "publication")?.id, "bind-brand", "brand");
equal(bindingForSlot([brand], "brand-b", "publication"), undefined, "wrong context");
equal(bindingForSlot([brand], null, "source"), undefined, "wrong slot");
// After a rebind both pins exist: the current bundle's row wins regardless of order.
const staleRow = binding({ id: "bind-old", config: { ...binding().config, bundle_digest: "sha256:old" } });
const freshRow = binding({ id: "bind-new" });
equal(bindingForSlot([staleRow, freshRow], null, "publication", "sha256:current")?.id, "bind-new", "current pin first");
equal(bindingForSlot([freshRow, staleRow], null, "publication", "sha256:current")?.id, "bind-new", "current pin either order");
equal(bindingForSlot([staleRow], null, "publication", "sha256:current")?.id, "bind-old", "stale still visible");

// Ready-to-run (CAD-796, CAD-1119): each typed slot names its connection,
// health and custody with the next action for a real blocker; legacy
// slots stay out. Installing is the approval, so it is never a blocker.
const readyRows = readinessFor(installation(), [binding()], all, null);
equal(readyRows.map((r) => r.slot), ["publication", "source"], "typed slots only");
equal(readyRows[0].ready, true, "bound healthy slot is ready");
equal(readyRows[0].nextAction, "Ready to run.", "ready needs nothing");
equal(readyRows[0].connection?.id, "builtin-local", "names the bound connection");
equal(readyRows[0].custody, true, "custody reported");
equal(readyRows[1].ready, false, "unbound slot is not ready");
equal(readyRows[1].nextAction, "Choose a source connection below and save it.", "missing slot action");
// A provider change that keeps the contract migrates silently: still Ready.
const migrating = binding({ drift: { state: "migrates", changes: [{ field: "mapping.tool", from: "a", to: "b" }] } });
equal(readinessFor(installation(), [migrating], all, null)[0].ready, true, "same-contract change is not a blocker");
// A widened contract blocks the slot and carries the change to confirm.
const widened = binding({ drift: { state: "needs_confirm", changes: [{ field: "mapping.scopes", from: [], to: ["x"] }] } });
const confirmRows = readinessFor(installation(), [widened], all, null);
equal(confirmRows[0].ready, false, "widened contract blocks");
equal(confirmRows[0].confirm?.[0]?.field, "mapping.scopes", "change carried to the confirm");
equal(confirmRows[0].nextAction, "Its connection changed what this slot may do. Review the change and confirm it.", "confirm action");
const unfit = binding({ drift: { state: "unavailable", reason: "provider action effect differs from app contract" } });
equal(readinessFor(installation(), [unfit], all, null)[0].ready, false, "unfit connection blocks");
const movedRows = readinessFor({ ...installation(), digest: "sha256:other" }, [binding()], all, null);
equal(movedRows[0].health, "stale-bundle", "bundle move detected");
equal(movedRows[0].nextAction, "Save the publication connection again for the current app version.", "stale rebind action");
const goneRows = readinessFor(installation(), [binding({ config: { ...binding().config, connection_id: "conn-gone" } })], all, null);
equal(goneRows[0].ready, false, "vanished connection is not ready");
equal(goneRows[0].nextAction, "Its connection is gone — choose another publication connection below.", "gone connection action");
const dark = localConnection();
dark.id = "builtin-local";
dark.status = { ...dark.status, custody_available: false };
const darkRows = readinessFor(installation(), [binding()], [dark, sourceConnection()], null);
equal(darkRows[0].ready, false, "unhealthy custody is not ready");
equal(darkRows[0].nextAction, "Its connection is unhealthy — see Settings → Connections, then rebind.", "custody action");
const revokedRows = readinessFor(installation(), [binding({ state: "revoked" })], all, null);
equal(revokedRows[0].ready, false, "revoked binding is not ready");
equal(revokedRows[0].nextAction, "Choose a publication connection below and save it.", "revoke rebind action");
// A deregistered provider adapter never reads Ready, even bound and approved.
const dereg = localConnection();
dereg.status = { ...dereg.status, adapter_registered: false };
const deregRows = readinessFor(installation(), [binding()], [dereg, sourceConnection()], null);
equal(deregRows[0].health, "ok", "binding itself still resolves");
equal(deregRows[0].registered, false, "registration reported");
equal(deregRows[0].ready, false, "deregistered provider is not ready");
equal(deregRows[0].nextAction, "Its connection's provider is no longer registered — see Settings → Connections, then rebind.", "registration action");
