import { matchRoute, routePath } from "../src/lib/router";
import type { Connection, ConnectionProvider } from "../src/lib/types";
import {
  acceptsToken,
  canManage,
  capabilityWords,
  connectionCapabilities,
  isAvailable,
  pinWord,
  readinessText,
  scopeHint,
} from "../src/features/settings/connectionsView";
import { connectionLabel, isLocalOutbox } from "../src/lib/connections";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

function row(over: Partial<Connection> = {}): Connection {
  return {
    id: "c-1",
    provider: "agenticos_external",
    account: "ws_12345678-1234-1234-1234-123456789012",
    kind: "enrolled",
    revision: 2,
    registration_digest: "sha256:abc",
    scopes: ["provider.read"],
    status: {
      adapter_registered: true,
      descriptor_available: true,
      custody_available: true,
      manifest_status: "matched",
      reviewed_pin: "agenticos-manifest@1",
      reported_pin: "agenticos-manifest@1",
      execution_authority: false,
      network_checked: false,
    },
    ...over,
  };
}

function localRow(): Connection {
  return row({
    id: "builtin-local",
    provider: "local",
    account: "local",
    kind: "builtin",
    revision: null,
    scopes: [],
  });
}

function provider(over: Partial<ConnectionProvider> = {}): ConnectionProvider {
  return {
    provider: "local",
    descriptor: {
      schema: 1,
      provider: "local",
      revision: "local@1",
      enrollment_shapes: [],
      builtin_accounts: ["local"],
      capabilities: [
        { id: "blog.publish", version: 1, tools: ["local_publish"], scopes: ["publish"], effect: "send", semantics: "LocalMarkdownSink" },
        { id: "social.post", version: 1, tools: ["local_publish"], scopes: ["publish"], effect: "send", semantics: "LocalMarkdownSink" },
      ],
    },
    descriptor_available: true,
    manifest_status: "matched",
    reviewed_pin: "local@1",
    reported_pin: "local@1",
    network_checked: false,
    registration_digest: "sha256:local",
    ...over,
  };
}

// The Local outbox is the built-in connection: provider local, connect none.
equal(isLocalOutbox(localRow()), true, "local outbox");
equal(isLocalOutbox(row()), false, "enrolled is not the outbox");
equal(isLocalOutbox(row({ provider: "local", account: "other" })), false, "local provider, other account");

equal(connectionLabel(localRow()), "Local outbox", "outbox label");
equal(
  connectionLabel(row()),
  "agenticos_external · ws_12345678-1234-1234-1234-123456789012",
  "enrolled label",
);

// Availability needs the adapter, the reviewed descriptor and custody.
equal(isAvailable(row()), true, "available");
for (const key of ["adapter_registered", "descriptor_available", "custody_available"] as const) {
  const bad = row();
  bad.status = { ...bad.status, [key]: false };
  equal(isAvailable(bad), false, `unavailable without ${key}`);
}

// Only enrolled credentials rotate or revoke; built-ins never do.
equal(canManage(row()), true, "enrolled is manageable");
equal(canManage(localRow()), false, "built-in is not manageable");

// The pin is a declaration match, never a probe.
equal(pinWord({ manifest_status: "matched" }), "reviewed contract matches the deployment", "matched");
equal(pinWord({ manifest_status: "missing" }), "deployment has no reported contract to compare", "missing");
equal(
  pinWord({ manifest_status: "mismatched" }),
  "deployment differs from the reviewed contract",
  "mismatched",
);
equal(
  pinWord({ manifest_status: "whatever" }),
  "deployment differs from the reviewed contract",
  "unknown pin fails closed",
);

// Readiness in one plain line, worst problem first.
equal(
  readinessText(localRow()),
  "Built-in — always present, no credential to manage.",
  "outbox readiness",
);
equal(
  readinessText(row({ status: { ...row().status, adapter_registered: false } })),
  "Provider is not registered on this daemon.",
  "unregistered",
);
equal(
  readinessText(row({ status: { ...row().status, descriptor_available: false } })),
  "Provider has no reviewed descriptor — unavailable.",
  "no descriptor",
);
equal(
  readinessText(row({ status: { ...row().status, custody_available: false } })),
  "Credential is missing from custody.",
  "missing custody",
);
equal(
  readinessText(row()),
  "reviewed contract matches the deployment",
  "healthy enrolled",
);

// Provider words: capabilities, enrollment and scope hints.
equal(capabilityWords(provider()), "blog.publish, social.post", "capability words");
equal(capabilityWords(provider({ descriptor: null })), null, "no descriptor, no words");
equal(acceptsToken(provider()), false, "local takes no token");
equal(
  acceptsToken(provider({ descriptor: { ...provider().descriptor!, enrollment_shapes: ["token"] } })),
  true,
  "token enrollment",
);
equal(scopeHint(provider()), ["publish"], "scope hint dedupes");
equal(scopeHint(provider({ descriptor: null })), [], "no descriptor, no hint");
equal(
  connectionCapabilities([provider()], "local"),
  "blog.publish, social.post",
  "connection capabilities",
);
equal(connectionCapabilities([provider()], "unknown"), null, "unknown provider");
equal(connectionCapabilities([], "local"), null, "no providers");

// The Settings Connections route survives refresh and paste.
equal(matchRoute("/settings/connections"), { screen: "settings", section: "connections" }, "match connections");
equal(routePath({ screen: "settings", section: "connections" }), "/settings/connections", "print connections");
