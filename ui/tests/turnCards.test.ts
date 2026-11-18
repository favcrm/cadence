import { parsePreview, parseStagedRow, type StagedWrite } from "../src/features/decisions/stagedWrite";
import type { ThreadEntry } from "../src/features/home/thread";
declare function require(name: string): any;
import { SOURCES_SHOWN, STAGED_FIELDS_SHOWN, STAGED_VALUE_CHARS, parseToolCall, stagedFields, stagedInTurn, turnSources } from "../src/features/home/turnCards";

/**
 * CAD-2441 (card system phase 1, CAD-2437): a turn's sources and staged
 * writes come only from its recorded tool calls and results.
 */

function check(ok: boolean, what: string): void {
  if (!ok) throw new Error(`turnCards: ${what}`);
}

let seq = 0;
const call = (text: string): ThreadEntry => ({ seq: ++seq, role: "agent", kind: "tool_call", text, message: "m1", payload: null });
const result = (text: string, payload: Record<string, unknown> | null = null): ThreadEntry => ({ seq: ++seq, role: "agent", kind: "tool_result", text, message: "m1", payload });
const prose = (text: string): ThreadEntry => ({ seq: ++seq, role: "agent", kind: "turn_result", text, message: "m1", payload: null });

// Recorded calls parse as `name {json}`; anything else is not a call.
check(parseToolCall('wiki_read {"path":"policies/refunds"}')?.args.path === "policies/refunds", "a call with arguments");
check(parseToolCall("board_list_issues")?.name === "board_list_issues", "a call without arguments");
check(parseToolCall("Wiki read: refunds") === null, "prose is not a call");
check(parseToolCall('wiki_read {"path":') === null, "broken JSON is not a call");

// Sources: which calls count, in first-read order, deduplicated.
{
  const sources = turnSources([
    call('wiki_read {"path":"policies/refunds"}'),
    result('{"text":"..."}'),
    call('board_read_issue {"id":"CAD-12"}'),
    call('chat_attachment_read {"id":"chf-0123456789abcdef0123456789abcdef","offset":0}'),
    call('chat_attachment_read {"id":"chf-0123456789abcdef0123456789abcdef","offset":51200}'),
    call('artifact_read {"id":"art-1"}'),
    call('web_read {"url":"https://example.com/pricing?x=1"}'),
    call('web_read {"url":"javascript:alert(1)"}'),
    call('app_call {"app":"crm","operation":"records.show","args":"{\\"id\\":\\"c1\\"}"}'),
    call('app_call {"app":"crm","operation":"records.update","args":"{}"}'),
    call('app_call {"app":"crm","operation":"records.list","args":"{}"}'),
    call('wiki_search {"query":"refunds"}'),
    call('wiki_read {"path":"policies/refunds"}'),
    prose("I read [the refunds page](wiki:policies/refunds) and CAD-99."),
  ]);
  check(
    JSON.stringify(sources.map((s) => `${s.kind}:${s.label}`)) ===
      JSON.stringify(["wiki:policies/refunds", "issue:CAD-12", "attachment:chf-0123456789abcdef0123456789abcdef", "artifact:art-1", "web:example.com", "app:crm"]),
    `sources in order, one per source: ${JSON.stringify(sources.map((s) => `${s.kind}:${s.label}`))}`,
  );
  check(sources.find((s) => s.kind === "web")?.target === "https://example.com/pricing?x=1", "a web source keeps its full URL");
  check(!sources.some((s) => s.label === "CAD-99"), "an id only in the answer's prose is not a source");
}
check(turnSources([prose("No tools here.")]).length === 0, "a turn that read nothing has no sources");
check(SOURCES_SHOWN === 8, "the row shows at most 8 chips");

// Staged writes: a decision whose id the turn's tool results carry.
const ACTION = "7d2e4c1a9b3f40e8a6c5d2b1f0e9a873";
const OTHER = "e41b0c97a6d34f2c9b8e7a6d5c4b3a21";
const appRow = (id: string, status = "pending") =>
  parseStagedRow({
    id,
    tool: "app_call",
    status,
    createdAt: 1_790_000_000_000,
    input: { app: "accounting", subject: "Add supplier Northwind", operation: "records.create", args: '{"name":"Northwind","currency":"HKD"}' },
  });
{
  const rows = [appRow(ACTION), appRow(OTHER)].filter((r): r is StagedWrite => r !== null);
  check(rows.length === 2, "fixture rows parse");
  const staged = stagedInTurn(
    [call('app_call {"app":"accounting","operation":"records.create"}'), result(`{"staged":true,"action_id":"${ACTION}"}`)],
    rows,
  );
  check(staged.length === 1 && staged[0]!.id === ACTION, "the write named in a tool result is this turn's");
  check(stagedInTurn([result("nothing staged")], rows).length === 0, "no id in the results, no card");
  check(stagedInTurn([prose(`I staged ${ACTION}`)], rows).length === 0, "an id only in prose does not make a card");
  const dashed = `${ACTION.slice(0, 8)}-${ACTION.slice(8, 12)}-${ACTION.slice(12, 16)}-${ACTION.slice(16, 20)}-${ACTION.slice(20)}`;
  check(stagedInTurn([result(`{"action_id":"${dashed}"}`)], rows).length === 1, "a dashed form of the id matches");
}

// A connected effect: the AOS-647 contract fixture, copied from
// agenticos-v2 packages/contracts/fixtures/chat-cards (staging 9f7f3fdd).
{
  const input = JSON.parse(require("fs").readFileSync(require("path").join(require("process").cwd(), "tests", "fixtures", "chat-cards", "connected-effect-email.json"), "utf8")) as Record<string, unknown>;
  const row = parseStagedRow({ id: ACTION, tool: "connected_effect", status: "pending", createdAt: 1_790_000_000_000, input });
  check(row !== null && row.connected !== null, "the fixture row parses as a connected effect");
  const preview = row!.connected!.preview;
  check(preview?.kind === "email" && preview.toolkitLabel === "Synthetic Mail", "the preview is read");
  const view = stagedFields(row!);
  check(
    JSON.stringify(view.fields.map((f) => f.label)) === JSON.stringify(["To", "Cc", "Subject", "Body"]),
    `preview fields in order: ${JSON.stringify(view.fields.map((f) => f.label))}`,
  );
  check(view.fields[0]!.value === "customer@example.test", "values are the preview's plain text");
  // Without a preview, the stored call's own keys are shown.
  const bare = parseStagedRow({ id: OTHER, tool: "connected_effect", status: "pending", createdAt: 1, input: { ...input, preview: undefined } });
  check(bare!.connected!.preview === undefined, "a row staged before AOS-647 has no preview");
  check(stagedFields(bare!).fields.some((f) => f.label === "recipient_email"), "the args fallback names the call's keys");
}

// A malformed preview is ignored, never half-read.
check(parsePreview({ kind: "email", fields: [{ label: 1, value: "x" }] }) === undefined, "a bad field drops the preview");
check(parsePreview({ kind: "html", fields: [] }) === undefined, "an unknown kind drops the preview");
check(parsePreview({ kind: "fields", fields: Array.from({ length: 20 }, (_, i) => ({ label: `f${i}`, value: "v" })) })!.fields.length === 12, "at most 12 preview fields");

// Field caps, and missing values are flagged.
{
  const long = parseStagedRow({
    id: ACTION,
    tool: "app_call",
    status: "pending",
    createdAt: 1,
    input: {
      app: "crm",
      subject: "Update",
      operation: "records.update",
      args: JSON.stringify(Object.fromEntries(Array.from({ length: 9 }, (_, i) => [`f${i}`, i === 2 ? "" : "x".repeat(400)]))),
    },
  });
  const view = stagedFields(long!);
  check(view.fields.length <= STAGED_FIELDS_SHOWN, "at most 6 fields on the card");
  check(view.fields.every((f) => f.value.length <= STAGED_VALUE_CHARS), "values are capped");
}

console.log("turnCards: ok");
