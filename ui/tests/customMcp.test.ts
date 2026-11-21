/** CAD-2556: Custom MCP connectors view helpers, staged actions, and chat card contracts. */
import { parseConnectRef, parseRecordRef, recordLayout } from "../src/features/home/chatCards";
import type { ThreadEntry } from "../src/features/home/thread";
import {
  stagedAccount,
  stagedFields,
  stagedInTurn,
  stagedStatus,
} from "../src/features/home/turnCards";
import { ApiError } from "../src/lib/apiError";
import {
  failureReason,
  outcomeOf,
  parseStagedRow,
  titleOf,
} from "../src/features/decisions/stagedWrite";
import { serviceLabel } from "../src/features/settings/connectionsView";
import {
  customMcpAuthLabel,
  customMcpAuthorizationUrl,
  customMcpErrorText,
  customMcpReturn,
  customMcpStatusView,
  customMcpToolModeLabel,
  mcpFocusParam,
  parseCustomHeadersInput,
  schemaParamSummary,
  validateCustomMcpUrlInput,
} from "../src/features/settings/customMcpView";
import { withoutConnectParams } from "../src/features/workspace-apps/socialConnect";
import type { CustomMcpConnectionSummary } from "../src/lib/types";

export {};

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`customMcp: ${what}: expected ${e}, got ${a}`);
}

function assert(cond: unknown, what: string): asserts cond {
  if (!cond) throw new Error(`customMcp: ${what}`);
}

// 1. SSRF URL validation UX pre-check.
assert(validateCustomMcpUrlInput("https://mcp.stripe.com/v1/mcp") === null, "public https accepted");
assert(validateCustomMcpUrlInput("http://mcp.stripe.com/v1/mcp") !== null, "http rejected");
assert(validateCustomMcpUrlInput("https://user:pass@mcp.stripe.com/") !== null, "embedded creds rejected");
assert(validateCustomMcpUrlInput("https://localhost:8443/mcp") !== null, "localhost rejected");
assert(validateCustomMcpUrlInput("https://127.0.0.1/mcp") !== null, "loopback v4 rejected");
assert(validateCustomMcpUrlInput("https://10.0.0.5/mcp") !== null, "rfc1918 rejected");
assert(validateCustomMcpUrlInput("https://169.254.169.254/latest") !== null, "link-local rejected");
assert(validateCustomMcpUrlInput("https://[::1]/mcp") !== null, "loopback v6 rejected");
assert(validateCustomMcpUrlInput("https://api.fav-limited.cadencecloud.app/mcp") !== null, "platform origin rejected");
assert(validateCustomMcpUrlInput("https://mcp-internal/tools") !== null, "single-label host rejected");

// 2. OAuth authorization_url check & callback return parsing.
equal(
  customMcpAuthorizationUrl({ authorization_url: "https://auth.example.com/authorize?state=abc" }),
  "https://auth.example.com/authorize?state=abc",
  "https authorization_url accepted",
);
equal(customMcpAuthorizationUrl({ authorization_url: "javascript:alert(1)" }), null, "non-https refused");
equal(customMcpAuthorizationUrl(null), null, "null refused");

equal(
  customMcpReturn("?aos_connect=connected&toolkit=custom_mcp&connection_id=conn_mcp_12345678"),
  { outcome: "connected", connectionId: "conn_mcp_12345678" },
  "connected Custom MCP return parsed",
);
equal(
  customMcpReturn("?aos_connect=cancelled&toolkit=custom_mcp"),
  { outcome: "cancelled", connectionId: null },
  "cancelled Custom MCP return parsed",
);
equal(
  customMcpReturn("?aos_connect=connected&toolkit=custom_mcp&connection_id=bad"),
  { outcome: "connected", connectionId: null },
  "invalid connection_id dropped",
);
equal(customMcpReturn("?aos_connect=connected&toolkit=gmail"), null, "other toolkit ignored");
equal(mcpFocusParam("?mcp_focus=Linear%20MCP"), "Linear MCP", "mcp_focus parsed");
equal(
  withoutConnectParams("?aos_connect=connected&toolkit=custom_mcp&connection_id=conn_mcp_12345678&tab=1"),
  "?tab=1",
  "connect return params stripped while other query params remain",
);
equal(withoutConnectParams("?mcp_focus=Linear"), "", "mcp_focus stripped");
equal(serviceLabel("custom_mcp"), "Custom MCP", "serviceLabel custom_mcp");

// 3. Headers parser, labels, status summary, and typed error mapping.
equal(
  parseCustomHeadersInput("X-API-Key: secret-1\nX-Tenant: acme"),
  {
    headers: [
      { name: "X-API-Key", value: "secret-1" },
      { name: "X-Tenant", value: "acme" },
    ],
    error: null,
  },
  "valid headers parse",
);
assert(parseCustomHeadersInput("Host: evil.com").error !== null, "forbidden Host header blocked");
assert(parseCustomHeadersInput("NoColonLine").error !== null, "malformed header rejected");
assert(parseCustomHeadersInput("X-A: 1\nx-a: 2").error !== null, "case-insensitive duplicates blocked");

const summaryBase: CustomMcpConnectionSummary = {
  id: "conn_mcp_abcdefgh",
  name: "Acme Billing",
  endpoint_url: "https://mcp.acme.example/v1",
  endpoint_host: "mcp.acme.example",
  auth_kind: "oauth",
  auth_hint: "OAuth 2.1",
  oauth_issuer: "https://auth.acme.example",
  oauth_scopes: ["mcp:read"],
  oauth_account: null,
  visibility: "shared",
  status: "needs_review",
  snapshot_revision: 2,
  pending_revision: 3,
  server_name: "acme-mcp",
  server_version: "1.0.0",
  server_instructions_digest: null,
  has_server_instructions: true,
  instructions_drifted: true,
  has_agent_guide: true,
  agent_guide_source: "https://docs.acme.example/llms.txt",
  last_synced_at: 1_795_000_000_000,
  last_used_at: null,
  created_at: 1_795_000_000_000,
  updated_at: 1_795_000_000_000,
  tools_summary: { total: 3, read: 1, approval: 1, off: 1, drifted: 1 },
};
equal(customMcpAuthLabel("oauth"), "OAuth 2.1", "auth label");
equal(customMcpToolModeLabel("approval"), "Ask approval", "mode label");
const statusView = customMcpStatusView(summaryBase);
assert(statusView.tone === "warn" && statusView.detail.includes("1 tool drifted") && statusView.detail.includes("server instructions changed"), "drift status summary");
assert(
  customMcpErrorText(new ApiError("raw backend secret leak", 400, { code: "ssrf_redirect_blocked" })).includes("blocked destination"),
  "typed error maps without leaking raw message",
);
equal(
  schemaParamSummary({ type: "object", properties: { invoice_id: { type: "string" }, memo: { type: "string" } }, required: ["invoice_id"] }),
  ["invoice_id*", "memo"],
  "schema params summarize required marker",
);

// 4. Staged custom_mcp_effect row parsing, turn matching, card display & failure codes.
const ACTION_ID = "a1b2c3d4e5f678901234567890abcdef";
const rawRow = {
  id: ACTION_ID,
  tool: "custom_mcp_effect",
  status: "pending",
  createdAt: 1_795_000_000_000,
  input: {
    connection_id: "conn_mcp_abcdefgh",
    connection_name: "Acme Billing",
    endpoint_host: "mcp.acme.example",
    tool: "create_invoice",
    snapshot_revision: 2,
    args: JSON.stringify({ customer_id: "cus_1", amount: 2500 }),
    preview: {
      title: "create_invoice",
      toolkit_label: "Acme Billing",
      account: "mcp.acme.example",
      kind: "fields",
      fields: [
        { role: "target", label: "customer_id", value: "cus_1" },
        { role: "amount", label: "amount", value: "2500" },
      ],
    },
  },
};
const parsed = parseStagedRow(rawRow);
assert(parsed !== null && parsed.customMcp != null, "custom_mcp_effect parses");
equal(parsed.customMcp.snapshotRevision, 2, "snapshot_revision preserved");
equal(titleOf(parsed, (s) => s), "Run tool · create_invoice", "titleOf custom_mcp_effect");
equal(stagedAccount(parsed), "mcp.acme.example", "stagedAccount returns endpoint_host");
equal(stagedFields(parsed).fields.map((f) => f.label), ["customer_id", "amount"], "stagedFields uses preview");
equal(stagedStatus(parsed, "pending", "Waiting"), "Waiting", "pending keeps rail word");

const appliedRow = parseStagedRow({ ...rawRow, status: "approved", decidedAt: 1_795_000_010_000, result: { ok: true } })!;
equal(outcomeOf(appliedRow), "applied", "approved + ok:true is applied");
equal(stagedStatus(appliedRow, "applied", "Applied"), "Ran", "applied custom_mcp_effect reads Ran");

const driftFailed = parseStagedRow({ ...rawRow, status: "approved", decidedAt: 1_795_000_010_000, result: { ok: false, code: "stale_revision" } })!;
equal(outcomeOf(driftFailed), "failed", "ok:false is failed");
equal(stagedStatus(driftFailed, "failed", "Did not apply"), "Couldn't run", "failed reads Couldn't run");
assert(failureReason(driftFailed).includes("changed since it was staged"), "stale_revision failure explained");

// Reject malformed custom_mcp_effect (e.g., missing snapshot_revision or invalid args JSON).
equal(parseStagedRow({ ...rawRow, input: { ...rawRow.input, snapshot_revision: 0 } }), null, "zero snapshot_revision refused");
equal(parseStagedRow({ ...rawRow, input: { ...rawRow.input, args: "{bad json" } }), null, "invalid args JSON refused");

// Match `{ action, sent: false, note }` emitted by use_tool(custom_mcp) in turnCards.
const matched = stagedInTurn(
  [
    {
      seq: 1,
      role: "agent",
      kind: "tool_result",
      text: JSON.stringify({ action: ACTION_ID, sent: false, note: "Staged, not sent." }),
      message: "m1",
      payload: null,
    },
  ],
  [parsed],
);
equal(matched.map((m) => m.id), [ACTION_ID], "stagedInTurn matches custom_mcp sent:false action receipt");

// 5. Chat Cards (record_ref + connect_ref with toolkit: custom_mcp).
const recordEntry: ThreadEntry = {
  seq: 2,
  role: "agent",
  kind: "record_ref",
  text: "Acme Billing: 1 result (mcp.acme.example)",
  message: "m1",
  payload: {
    v: 1,
    toolkit: "custom_mcp",
    tool: "list_invoices",
    items: [{ id: "inv_101", status: "open" }],
    fields: [
      { key: "id", label: "id", type: "string", untrusted: true },
      { key: "status", label: "status", type: "string", untrusted: true },
    ],
    view: "table",
    truncated: false,
    tool_label: "list_invoices",
    toolkit_label: "Acme Billing",
    account: "mcp.acme.example",
    total: 1,
  },
};
const parsedRecord = parseRecordRef(recordEntry);
assert(parsedRecord !== null && parsedRecord.toolkit === "custom_mcp", "custom_mcp record_ref parses");
equal(
  recordLayout(parsedRecord),
  { view: "table", pairs: [{ label: "id", value: "inv_101" }, { label: "status", value: "open" }] },
  "custom_mcp table layout renders cleanly",
);

const connectEntry: ThreadEntry = {
  seq: 3,
  role: "agent",
  kind: "connect_ref",
  text: "Acme Billing needs reconnection.",
  message: "m1",
  payload: { v: 1, toolkit: "custom_mcp", toolkit_label: "Acme Billing" },
};
equal(parseConnectRef(connectEntry), { toolkit: "custom_mcp", label: "Acme Billing" }, "custom_mcp connect_ref parses");

console.log("customMcp: ok");
