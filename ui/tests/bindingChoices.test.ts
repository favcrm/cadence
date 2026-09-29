import {
  bindingForSlot,
  bindingHealth,
  candidatesFor,
  connectionLabel,
  declaredSlots,
  isLocalOutbox,
  plainRequirement,
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
    kind: "enrolled",
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
        semantics: "LocalMarkdownSink", input_contract: "in", output_contract: "out",
      }],
    },
  });
}

function sourceConnection(): Connection {
  return connection({
    id: "conn-source", account: "ws_source",
    descriptor: {
      action_mappings: [{
        capability: "social.read", version: 1, action: "list_posts",
        resource_kind: "connection_account", effect: "read",
        semantics: "MetadataRead", input_contract: "in", output_contract: "out",
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
        semantics: "LocalMarkdownSink", input_contract: "in", output_contract: "out",
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
for (const bad of [wrongVersion, wrongEffect, stalePin, noCustody, noDescriptor]) {
  const seen = candidatesFor(sourceRead(), [bad, sourceConnection()]);
  equal(seen.offered.map((c) => c.id), ["conn-source"], `withholds ${bad.id}`);
}

// Plain words, never a tool or scope.
equal(isLocalOutbox(localConnection()), true, "local outbox");
equal(connectionLabel(localConnection()), "Local outbox", "outbox label");
equal(
  connectionLabel(sourceConnection()),
  "agenticos_external · ws_source",
  "enrolled label",
);
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
