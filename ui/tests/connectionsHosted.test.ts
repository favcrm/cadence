import type { Connection, ConnectionProvider } from "../src/lib/types";
import {
  connectedBannerText,
  hostedAccount,
  hostedAccountLine,
  hostedProvider,
  hostedReturn,
  returnedRow,
  serviceGroups,
  showsPolicyPanel,
  toolsSummary,
  visibilityBadge,
  visibilityConfirmed,
} from "../src/features/settings/connectionsView";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

const tools = [
  { slug: "gmail_fetch_emails", label: "Gmail: fetch emails", effect: "read" },
  { slug: "gmail_send_email", label: "Gmail: send email", effect: "send" },
];

function hostedWire(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    label: "Gmail",
    visibility: "personal",
    connected_by_you: true,
    connected_at: 1795000000000,
    last_used_at: null,
    can_change_visibility: true,
    tools,
    ...over,
  };
}

function row(over: Partial<Connection> = {}): Connection {
  return {
    id: "con_1",
    provider: "gmail",
    account: "chris@example.com",
    kind: "enrolled",
    revision: 1,
    registration_digest: null,
    scopes: [],
    status: {
      adapter_registered: true,
      descriptor_available: true,
      custody_available: true,
      manifest_status: "matched",
      reviewed_pin: null,
      reported_pin: null,
      execution_authority: false,
      network_checked: false,
    },
    ...over,
  };
}

// Absent or malformed `hosted` is the same as no hosted data.
for (const bad of [undefined, null, "x", 3, [], {}, hostedWire({ label: "" }), hostedWire({ visibility: "public" }), hostedWire({ tools: "no" })]) {
  equal(hostedAccount(row({ hosted: bad })), null, `malformed hosted ${JSON.stringify(bad)}`);
}

// A well-formed value is narrowed; loose fields degrade, tools with an unknown effect are dropped.
const parsed = hostedAccount(
  row({
    hosted: hostedWire({
      connected_by_you: "yes",
      can_change_visibility: 1,
      connected_at: "soon",
      tools: [...tools, { slug: "x", label: "X", effect: "delete" }, { slug: "gmail_create_draft", effect: "draft" }, 7],
    }),
  }),
);
equal(parsed?.connectedByYou, false, "connected_by_you must be exactly true");
equal(parsed?.canChangeVisibility, false, "can_change_visibility must be exactly true");
equal(parsed?.connectedAt, null, "non-numeric time is null");
equal(parsed?.tools.map((t) => t.slug), ["gmail_fetch_emails", "gmail_send_email", "gmail_create_draft"], "tools kept");
equal(parsed?.tools[2].label, "gmail_create_draft", "tool label falls back to slug");
equal(hostedAccount(row({ hosted: hostedWire({ tools: undefined }) }))?.tools, [], "missing tools is empty");

// Badge text and account line.
equal(visibilityBadge({ visibility: "personal" }), "Personal", "personal badge");
equal(visibilityBadge({ visibility: "shared" }), "Shared", "shared badge");
const h = hostedAccount(row({ hosted: hostedWire() }))!;
equal(hostedAccountLine(row(), h), "Gmail · chris@example.com", "account line");
equal(hostedAccountLine(row({ account: "Gmail" }), h), "Gmail", "account line without identity");

// Tools summary.
equal(toolsSummary([]), null, "no tools");
equal(toolsSummary(h.tools), "1 read · 1 ask you first", "summary");
equal(toolsSummary([h.tools[0]]), "1 read", "read only summary");

// The board-local revoke-first panel is never offered for a row that carries `hosted`.
equal(showsPolicyPanel(row({ hosted: hostedWire() }), false), false, "hosted row never shows the policy panel");
equal(showsPolicyPanel(row({ hosted: hostedWire({ visibility: "shared" }) }), false), false, "shared hosted row either");
equal(showsPolicyPanel(row(), false), true, "absent hosted keeps today's panel");
equal(showsPolicyPanel(row({ hosted: "bad" }), false), true, "malformed hosted is absent");
equal(showsPolicyPanel(row(), true), false, "hosted board keeps hiding it");
equal(showsPolicyPanel(row({ kind: "builtin" }), false), false, "built-in never");

// Provider rows and grouping.
function provider(over: Partial<ConnectionProvider> = {}): ConnectionProvider {
  return {
    provider: "gmail",
    descriptor: null,
    descriptor_available: false,
    manifest_status: "missing",
    reviewed_pin: null,
    reported_pin: null,
    network_checked: false,
    registration_digest: null,
    ...over,
  };
}
equal(hostedProvider(provider({ hosted: { label: "Gmail", tools } }))?.tools.length, 2, "provider hosted");
equal(hostedProvider(provider({ hosted: { tools } })), null, "provider hosted needs a label");
equal(serviceGroups([provider({ hosted: { label: "Gmail", tools } })], [])[0].label, "Gmail", "group label from hosted");
equal(serviceGroups([provider()], [])[0].label, "gmail", "group label without hosted");

// Return banner.
equal(hostedReturn("?aos_connect=connected&toolkit=gmail"), { outcome: "connected", toolkit: "gmail" }, "connected return");
equal(hostedReturn("?aos_connect=failed&toolkit=google_calendar"), { outcome: "failed", toolkit: "google_calendar" }, "failed return");
equal(hostedReturn("?aos_connect=cancelled&toolkit=gmail"), null, "other outcome");
equal(hostedReturn("?aos_connect=connected"), null, "no toolkit");
equal(hostedReturn("?aos_connect=connected&toolkit=%3Cb%3E"), null, "unsafe toolkit");
const older = row({ id: "con_old", hosted: hostedWire({ connected_at: 1 }) });
const newer = row({ id: "con_new", hosted: hostedWire({ connected_at: 2 }) });
equal(returnedRow([older, newer, row({ id: "con_x", provider: "slack", hosted: hostedWire() })], "gmail")?.id, "con_new", "newest hosted row");
equal(returnedRow([row()], "gmail"), null, "no hosted row yet");
equal(connectedBannerText("Gmail", newer), "Gmail connected · chris@example.com · Personal to you", "personal banner");
equal(
  connectedBannerText("Gmail", row({ hosted: hostedWire({ visibility: "shared" }) })),
  "Gmail connected · chris@example.com · Shared with company",
  "shared banner",
);
equal(connectedBannerText("Gmail", null), "Gmail sign-in finished — check your accounts below", "no connected claim without the row");
equal(visibilityConfirmed({ ok: true, visibility: "shared" }, "shared"), true, "confirmed change");
for (const bad of [null, {}, { ok: true }, { ok: true, visibility: "personal" }, { ok: false, visibility: "shared" }, "ok"]) {
  equal(visibilityConfirmed(bad, "shared"), false, `unconfirmed ${JSON.stringify(bad)}`);
}
