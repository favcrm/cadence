import { matchRoute, routePath } from "../src/lib/router";
import type { Connection, ConnectionProvider } from "../src/lib/types";
import {
  acceptsSmtp,
  acceptsToken,
  canManage,
  capabilityWords,
  connectionCapabilities,
  enrollmentShapes,
  enrollmentSupport,
  isAvailable,
  pinWord,
  readinessText,
  scopeHint,
  serviceGroups,
  serviceLabel,
  smtpPortTlsError,
  smtpSummary,
  verificationResult,
  verificationSupport,
} from "../src/features/settings/connectionsView";
import { connectionLabel, isLocalOutbox } from "../src/lib/connections";

function assert(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(`assert: ${msg}`);
}

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
// SMTP sender enrollment (CAD-785): shape allowlist and projection words.
equal(acceptsSmtp(provider()), false, "local takes no smtp");
equal(
  acceptsSmtp(provider({ descriptor: { ...provider().descriptor!, enrollment_shapes: ["smtp"] } })),
  true,
  "smtp enrollment",
);
equal(enrollmentShapes(provider()), [], "no smtp or token shape offered");
equal(
  enrollmentShapes(provider({ descriptor: { ...provider().descriptor!, enrollment_shapes: ["smtp"] } })),
  ["smtp"],
  "smtp shape listed",
);
equal(smtpSummary(row()), null, "no smtp projection, no summary");
equal(
  smtpSummary(
    row({
      provider: "smtp",
      scopes: ["email:send"],
      smtp: { host: "mail.example.com", port: 587, tls_mode: "starttls", username: "sender", sender: "news@example.com", sender_name: "CRM News" },
    }),
  ),
  'mail.example.com:587 · STARTTLS · "CRM News" <news@example.com> · login sender',
  "smtp summary names transport and verified sender",
);
equal(
  connectionCapabilities([provider()], "local"),
  "blog.publish, social.post",
  "connection capabilities",
);
equal(connectionCapabilities([provider()], "unknown"), null, "unknown provider");
equal(connectionCapabilities([], "local"), null, "no providers");

// The client port/TLS gate mirrors the daemon's validate_port_tls:
// public hosts accept exactly (465, implicit) and (587, starttls);
// localhost keeps the mandatory mode but any port (ephemeral rig).
equal(smtpPortTlsError("mail.example.com", "465", "implicit"), null, "465 implicit ok");
equal(smtpPortTlsError("mail.example.com", "587", "starttls"), null, "587 starttls ok");
equal(typeof smtpPortTlsError("mail.example.com", "465", "starttls"), "string", "465 starttls refused");
equal(typeof smtpPortTlsError("mail.example.com", "587", "implicit"), "string", "587 implicit refused");
equal(typeof smtpPortTlsError("mail.example.com", "2525", "starttls"), "string", "2525 refused");
equal(typeof smtpPortTlsError("mail.example.com", "25", "implicit"), "string", "port 25 refused");
equal(typeof smtpPortTlsError("mail.example.com", "465", "none"), "string", "plaintext mode refused");
equal(smtpPortTlsError("localhost", "40211", "implicit"), null, "localhost ephemeral implicit ok");
equal(smtpPortTlsError("localhost", "51997", "starttls"), null, "localhost ephemeral starttls ok");
equal(smtpPortTlsError("localhost", "465", "implicit"), null, "localhost 465 ok");
equal(smtpPortTlsError("localhost.", "587", "starttls"), null, "localhost trailing dot ok");
equal(typeof smtpPortTlsError("localhost", "0", "implicit"), "string", "port 0 refused");
equal(typeof smtpPortTlsError("localhost", "70000", "implicit"), "string", "port over 65535 refused");
equal(typeof smtpPortTlsError("LOCALHOST", "40211", "implicit"), "string", "localhost is exact, not case-folded");

// CAD-1166 service grouping: one group per provider, canonical identity
// kept, built-ins last, unregistered providers never dropped or merged.
{
  const smtpP = provider({ provider: "smtp", descriptor: { ...provider().descriptor!, provider: "smtp", enrollment_shapes: ["smtp"] } });
  const tokenP = provider({ provider: "agenticos_external", descriptor: { ...provider().descriptor!, provider: "agenticos_external", enrollment_shapes: ["token"] } });
  const localP = provider();
  const smtpConn = row({ id: "c-smtp", provider: "smtp", account: "news" });
  const tokenConn = row({ id: "c-tok" });
  const outbox = localRow();
  const groups = serviceGroups([localP, tokenP, smtpP], [smtpConn, tokenConn, outbox]);
  equal(
    groups.map((g) => g.provider),
    ["agenticos_external", "smtp", "local"],
    "groups sort by provider, local last",
  );
  equal(groups[2].label, "Local outbox", "local service reads Local outbox");
  equal(groups[0].label, "AgenticOS", "agenticos_external presents as AgenticOS");
  equal(groups[1].label, "Email (SMTP)", "smtp presents as Email (SMTP)");
  equal(serviceLabel("agenticos"), "AgenticOS", "agenticos presents as AgenticOS");
  equal(serviceLabel("unknown_svc"), "unknown_svc", "unknown provider label is unchanged");
  equal(groups[2].builtin, true, "local group is built-in");
  equal(groups[0].builtin, false, "enrollable group is not built-in");
  equal(groups[0].connections.map((c) => c.id), ["c-tok"], "token account under its provider");
  equal(groups[1].connections.map((c) => c.id), ["c-smtp"], "smtp account under its provider");

  // A registered provider with no accounts still renders (setup state).
  const empty = serviceGroups([tokenP], []);
  equal(empty.length, 1, "empty provider still groups");
  equal(empty[0].connections.length, 0, "empty provider has no accounts");

  // A connection whose provider is not registered keeps its own group —
  // it is never folded into the built-ins and never disappears.
  const stale = serviceGroups([localP], [smtpConn, outbox]);
  equal(
    stale.map((g) => g.provider),
    ["smtp", "local"],
    "unregistered provider keeps its own group before local",
  );
  equal(stale[0].info, null, "unregistered group has no descriptor");
  equal(stale[0].builtin, false, "an unregistered enrolled account is not labelled built-in");

  // Empty groups of providers are suppressed only when nothing owns the name.
  equal(serviceGroups([], []).length, 0, "no providers and no accounts: no groups");

  // Enrollment support is read honestly from the descriptor.
  equal(enrollmentSupport(tokenP).shapes, ["token"], "token provider offers token enrollment");
  equal(enrollmentSupport(smtpP).shapes, ["smtp"], "smtp provider offers smtp enrollment");
  equal(enrollmentSupport(localP).shapes, [], "local offers no enrollment");
  assert(enrollmentSupport(localP).reason.length > 0, "no-enrollment reason is stated");
  assert(enrollmentSupport(null).reason.length > 0, "unregistered names a reason");

  // Remote verification (CAD-1065/CAD-1085) is advertised ONLY by the
  // trusted `verification_support` metadata — never inferred from the
  // provider name or descriptor. Absent/unknown/refused stays
  // unavailable with a fixed safe reason, never the raw reason_code.
  equal(verificationSupport(smtpP).supported, false, "no verification_support -> unavailable");
  equal(verificationSupport(null).supported, false, "unknown provider unsupported");
  const vs = { operation: "smtp-login-no-send-v1" as const, supported: true, reason_code: null };
  equal(
    verificationSupport(provider({ provider: "smtp", verification_support: vs })).supported,
    true,
    "advertised smtp-login-no-send-v1 is supported",
  );
  equal(
    verificationSupport(provider({ provider: "smtp", verification_support: { ...vs, supported: false, reason_code: "unsupported_provider" } })).supported,
    false,
    "supported:false stays unavailable",
  );
  assert(
    verificationSupport(provider({ provider: "smtp", verification_support: { operation: "other-op" as never, supported: true, reason_code: null } })).supported === false,
    "an unlisted operation is not trusted",
  );
  assert(
    !verificationSupport(provider({ provider: "smtp", verification_support: { ...vs, supported: false, reason_code: "some_raw_reason" } })).reason.includes("some_raw_reason"),
    "a raw reason_code is never echoed",
  );

  // verificationResult: only an exact reviewed receipt for the captured
  // tuple can be a shown success. Malformed/partial/extra/
  // non-false-flag responses return null; a stale receipt is never a
  // success and never presents server-current values as proof.
  const want = { id: "c-smtp", revision: 2, registration_digest: "sha256:" + "a".repeat(64) };
  const okReceipt = {
    schema: 1, operation: "smtp-login-no-send-v1", connection_id: "c-smtp",
    revision: 2, registration_digest: want.registration_digest,
    started_at: "2026-10-07T01:00:00Z", completed_at: "2026-10-07T01:00:02Z",
    status: "success", network_attempted: true, authentication_verified: true,
    email_sent: false, delivery_verified: false, sender_entitlement_verified: false,
    execution_authority: false, failure: null,
  };
  const ok = verificationResult({ verification: okReceipt }, want);
  assert(ok !== null && ok.ok === true && ok.stale === false, "exact success receipt parses");
  assert((ok!.text).includes("Login verified") && (ok!.text).includes("no email"), "success says login verified, no email sent");
  assert(ok!.checkedAt === "2026-10-07T01:00:02Z", "checkedAt carried");

  equal(verificationResult({}, want), null, "no verification key -> null");
  equal(verificationResult({ verification: null }, want), null, "null verification -> null");
  equal(verificationResult({ verification: { ...okReceipt, schema: 2 } }, want), null, "wrong schema -> null");
  equal(verificationResult({ verification: { ...okReceipt, operation: "other" } }, want), null, "wrong operation -> null");
  equal(verificationResult({ verification: { ...okReceipt, connection_id: "c-other" } }, want), null, "wrong id -> null");
  equal(verificationResult({ verification: { ...okReceipt, revision: 3 } }, want), null, "mismatched success revision -> null");
  equal(verificationResult({ verification: { ...okReceipt, registration_digest: "sha256:" + "b".repeat(64) } }, want), null, "mismatched success digest -> null");
  equal(verificationResult({ verification: { ...okReceipt, email_sent: true } }, want), null, "email_sent true -> null");
  equal(verificationResult({ verification: { ...okReceipt, delivery_verified: true } }, want), null, "delivery flag true -> null");
  equal(verificationResult({ verification: { ...okReceipt, extra: 1 } }, want), null, "extra field -> null");
  equal(verificationResult({ verification: { ...okReceipt, status: "mystery" } }, want), null, "unknown status -> null");
  equal(verificationResult({ verification: { ...okReceipt, failure: { code: "bogus", step: "auth" } } }, want), null, "unknown failure code -> null");
  equal(verificationResult({ verification: { ...okReceipt, completed_at: "not-a-time" } }, want), null, "bad timestamp -> null");

  // Identity pinning: a receipt for the right id but a different
  // revision/digest is evidence on the wrong tuple — only a stale
  // receipt may differ; every non-stale status must match the request.
  const wrongDigest = "sha256:" + "d".repeat(64);
  equal(
    verificationResult({ verification: { ...okReceipt, status: "failed", authentication_verified: false, revision: 3, failure: { code: "auth_failed", step: "auth" } } }, want),
    null,
    "failed receipt on a different revision -> null",
  );
  equal(
    verificationResult({ verification: { ...okReceipt, status: "failed", authentication_verified: false, registration_digest: wrongDigest, failure: { code: "auth_failed", step: "auth" } } }, want),
    null,
    "failed receipt on a different digest -> null",
  );
  equal(
    verificationResult({ verification: { ...okReceipt, status: "unsupported", authentication_verified: false, network_attempted: false, revision: 3 } }, want),
    null,
    "unsupported receipt on a different revision -> null",
  );
  // Impossible/under-pinned combos are rejected, never shown.
  equal(
    verificationResult({ verification: { ...okReceipt, network_attempted: false } }, want),
    null,
    "success without a network attempt -> null",
  );
  equal(
    verificationResult({ verification: { ...okReceipt, failure: { code: "auth_failed", step: "auth" } } }, want),
    null,
    "success carrying a failure -> null",
  );
  equal(
    verificationResult({ verification: { ...okReceipt, status: "failed", authentication_verified: true, failure: { code: "auth_failed", step: "auth" } } }, want),
    null,
    "failed receipt that also claims verified auth -> null",
  );
  equal(
    verificationResult({ verification: { ...okReceipt, status: "failed", authentication_verified: false } }, want),
    null,
    "failed with no failure detail -> null",
  );
  equal(
    verificationResult({ verification: { ...okReceipt, status: "unsupported", authentication_verified: false, network_attempted: false, failure: { code: "unsupported_provider", step: "admission" } } }, want),
    null,
    "unsupported carrying a failure detail -> null",
  );
  equal(
    verificationResult({ verification: { ...okReceipt, status: "stale", authentication_verified: false, revision: 3, registration_digest: wrongDigest, failure: { code: "stale_connection", step: "admission" } } }, want),
    null,
    "stale carrying a failure detail -> null",
  );

  // A closed failure maps to fixed actionable text; a stale receipt is
  // flagged stale and never a success even when its server tuple differs.
  const failed = verificationResult(
    { verification: { ...okReceipt, status: "failed", authentication_verified: false, failure: { code: "auth_failed", step: "auth" } } },
    want,
  );
  assert(failed !== null && failed.ok === false && failed.stale === false, "closed failure parses");
  assert((failed!.text).includes("login was refused") || (failed!.text).includes("credentials"), "auth_failed gives actionable text");
  const staleRes = verificationResult(
    { verification: { ...okReceipt, status: "stale", authentication_verified: false, revision: 3, registration_digest: "sha256:" + "c".repeat(64) } },
    want,
  );
  assert(staleRes !== null && staleRes.stale === true && staleRes.ok === false, "stale receipt is stale, not success");
  assert((staleRes!.text).includes("refresh"), "stale result asks for a refresh");
  const unsupported = verificationResult(
    { verification: { ...okReceipt, status: "unsupported", authentication_verified: false, revision: 2 } },
    want,
  );
  assert(unsupported !== null && unsupported.ok === false && unsupported.stale === false, "unsupported parses");
}

// The Settings Connections route survives refresh and paste.
equal(matchRoute("/settings/connections"), { screen: "settings", section: "connections" }, "match connections");
equal(routePath({ screen: "settings", section: "connections" }), "/settings/connections", "print connections");
