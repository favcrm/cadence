import { parsePreview, parseStagedRow, type StagedWrite } from "../src/features/decisions/stagedWrite";
import type { ThreadEntry } from "../src/features/home/thread";
declare function require(name: string): any;
import { SOURCES_SHOWN, STAGED_FIELDS_SHOWN, STAGED_VALUE_CHARS, attachmentNames, bodyNeedsClamp, outsideDomainRecipients, parseToolCall, shortId, stagedAccount, stagedFields, stagedInTurn, stagedStatus, turnSources } from "../src/features/home/turnCards";

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
    call('app_call {"app":"crm","operation":"records.show","args":"{\\"record_id\\":\\"c1\\"}"}'),
    call('app_call {"app":"crm","operation":"records.update","args":"{\\"record_id\\":\\"c2\\"}"}'),
    call('app_call {"app":"crm","operation":"records.list","args":"{}"}'),
    call('app_call {"app":"crm","operation":"records.show","args":"{}"}'),
    call('wiki_search {"query":"refunds"}'),
    call('wiki_read {"path":"policies/refunds"}'),
    prose("I read [the refunds page](wiki:policies/refunds) and CAD-99."),
  ]);
  check(
    JSON.stringify(sources.map((s) => `${s.kind}:${s.label}`)) ===
      JSON.stringify(["wiki:policies/refunds", "issue:CAD-12", "attachment:chf-01234567…", "artifact:art-1", "web:example.com", "app:crm · c1"]),
    `sources in order, one per source: ${JSON.stringify(sources.map((s) => `${s.kind}:${s.label}`))}`,
  );
  check(sources.find((s) => s.kind === "web")?.target === "https://example.com/pricing?x=1", "a web source keeps its full URL");
  check(!sources.some((s) => s.label === "CAD-99"), "an id only in the answer's prose is not a source");
  const app = sources.find((s) => s.kind === "app");
  check(app?.app === "crm" && app.target === "c1", "an app source is the record, by app and record id");
}
check(shortId("art-4cff6ebbaae44734856e9acb4549e320") === "art-4cff6ebb…", `a long artifact id is cut: ${shortId("art-4cff6ebbaae44734856e9acb4549e320")}`);
check(shortId("art-1") === "art-1", "a short id stays whole");
check(turnSources([call('artifact_read {"id":"art-4cff6ebbaae44734856e9acb4549e320"}')])[0]!.target === "art-4cff6ebbaae44734856e9acb4549e320", "the chip still targets the full id");
// CAD-2456: the native runtime's `tool_summary` form, `name: <detail>`.
{
  check(parseToolCall("web_read: https://www.hubspot.com/pricing/crm")?.summary === "https://www.hubspot.com/pricing/crm", "a summary's detail is read");
  check(parseToolCall("board_read_issue: {id}")?.summary === undefined, "a keys-only summary carries no detail");
  const sources = turnSources([
    call("web_read: https://www.hubspot.com/pricing/crm"),
    call("wiki_read: policies/refunds"),
    call('wiki_read {"path":"policies/refunds"}'),
    call('web_read {"url":"https://www.hubspot.com/pricing/crm"}'),
    call("web_read: javascript:alert(1)"),
    call("web_read: not a url"),
    call("wiki_list: projects"),
    call("web_search: crm pricing"),
    call("board_read_issue: {id}"),
    call("chat_attachment_read: {id, offset}"),
    call("app_call: {app, args, operation}"),
    call("wiki_read: {path}"),
  ]);
  check(
    JSON.stringify(sources.map((s) => `${s.kind}:${s.label}`)) === JSON.stringify(["web:www.hubspot.com", "wiki:policies/refunds"]),
    `summary-form sources, deduplicated across both forms: ${JSON.stringify(sources.map((s) => `${s.kind}:${s.label}`))}`,
  );
  check(sources[0]!.target === "https://www.hubspot.com/pricing/crm", "a summary web source keeps its full URL");
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
  check(stagedInTurn([result(`{"staged":true,"action_id":"${dashed}"}`)], rows).length === 1, "a dashed form of the id matches");

  // CAD-2450, shapes seen on Demo Company: the staging result is the only match.
  const staging = result(`{"staged":true,"actionId":"${ACTION}","note":"Recorded as an action for an owner to decide."}`);
  check(stagedInTurn([staging], rows).map((r) => r.id).join() === ACTION, "a camelCase actionId staging result matches");
  const listing = result(`{"total":2,"actions":[{"id":"${ACTION}","tool":"app_call"},{"id":"${OTHER}","tool":"app_call"}]}`);
  check(stagedInTurn([listing], rows).length === 0, "an actions list names ids but stages nothing");
  check(stagedInTurn([result(`{"record":{"id":"${ACTION}"},"staged":false}`)], rows).length === 0, "a record read is not a staging result");
  check(stagedInTurn([result(`{"actionId":"${ACTION}"}`)], rows).length === 0, "an id without staged:true is not a staging result");
  check(stagedInTurn([result(`Staged ${ACTION}`)], rows).length === 0, "plain text naming the id is not a staging result");
  const payloadResult: ThreadEntry = { seq: ++seq, role: "agent", kind: "tool_result", text: "ok", message: "m1", payload: { staged: true, actionId: OTHER } };
  check(stagedInTurn([payloadResult], rows).map((r) => r.id).join() === OTHER, "a staging payload matches");
  // Two turns: the staging turn shows the card, the later turn that lists it does not.
  check(stagedInTurn([call('app_call {"app":"accounting","operation":"records.create"}'), staging], rows).length === 1, "turn 1 staged it");
  check(stagedInTurn([call('app_call {"app":"accounting","operation":"records.list"}'), listing], rows).length === 0, "turn 2 only listed it");
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
    JSON.stringify(view.fields.map((f) => f.label)) === JSON.stringify(["To", "Cc", "Subject"]),
    `preview fields in order, body apart: ${JSON.stringify(view.fields.map((f) => f.label))}`,
  );
  check(view.body === "Hello, this confirms your booking on Friday at 3pm.", "the body is the preview's message, whole");
  check(preview?.account === "ops@example.test" && stagedAccount(row!) === "ops@example.test", "the sending account is read");
  check(outsideDomainRecipients(row!) === 0, "recipients on the account's own domain are inside");
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
    app: "crm",
    title: "Update customer",
    changes: Array.from({ length: 9 }, (_, i) => ({ field: `f${i}`, label: `F${i}`, kind: "changed", before: "a", after: i === 2 ? "" : "x".repeat(400) })),
    changes_complete: true,
    input: { app: "crm", subject: "Update", operation: "records.update", args: "{}" },
  });
  check(long !== null && long.changes.length === 9, "a summary row with nine changes parses");
  const view = stagedFields(long!);
  check(view.fields.length === STAGED_FIELDS_SHOWN && view.more === 3, "6 fields on the card, 3 more on the review card");
  check(view.fields.every((f) => f.value.length <= STAGED_VALUE_CHARS), "values are capped");
  check(view.fields[0]!.value.endsWith("…"), "a cut value says so");
  check(view.fields[2]!.missing && !view.fields[1]!.missing, "only the empty value is flagged");
}

// CAD-2463: web chips that share a host add their path; others stay bare.
{
  const sources = turnSources([
    call("web_read: https://www.hubspot.com/pricing/crm"),
    call("web_read: https://www.hubspot.com/products/crm?utm=x"),
    call("web_read: https://www.zoho.com/crm/zohobigin/pricing.html"),
    call('web_read {"url":"https://www.hubspot.com/"}'),
    call("web_read: https://docs.example.com/a/really/long/path/that/keeps/going/and/going/forever"),
    call("web_read: https://docs.example.com/b"),
  ]);
  const labels = sources.map((s) => s.label);
  check(
    JSON.stringify(labels.slice(0, 4)) === JSON.stringify(["www.hubspot.com/pricing/crm", "www.hubspot.com/products/crm?…", "www.zoho.com", "www.hubspot.com/"]),
    `shared hosts add their path: ${JSON.stringify(labels)}`,
  );
  check(labels[4]!.length === 40 && labels[4]!.endsWith("…"), `a long path is cut: ${labels[4]}`);
  check(sources.every((s) => s.kind !== "web" || s.target.startsWith("https://")), "targets stay the full URL");
}

// CAD-2463: an attachment is named by the file the thread's own message carried.
{
  const id = "chf-121800f2fb54c8d375f53e115e0a4909";
  const message: ThreadEntry = { seq: ++seq, role: "operator", kind: "message", text: "see attached", message: "m0", payload: { attachments: [{ id, name: "Quote Q-0140.pdf", size: 10, mime: "application/pdf" }, { id: "bad" }, "junk"] } };
  const names = attachmentNames([message, prose("x"), { ...message, payload: null }]);
  check(names.get(id) === "Quote Q-0140.pdf" && names.size === 1, "names come from payload.attachments; malformed rows are skipped");
  const read = call(`chat_attachment_read {"id":"${id}","offset":0}`);
  check(turnSources([read], names)[0]!.label === "Quote Q-0140.pdf", "a held attachment shows its file name");
  check(turnSources([read])[0]!.label === "chf-121800f2…", "an unknown attachment shows a short id");
  check(turnSources([read], names)[0]!.target === id, "the chip still targets the id");
}

// CAD-2463: outside-domain recipients, only for an email with an address account.
{
  const email = (account: string | undefined, fields: { role: string; label: string; value: string }[], kind = "email") =>
    parseStagedRow({
      id: ACTION,
      tool: "connected_effect",
      status: "pending",
      createdAt: 1,
      input: { slug: "gmail_send", toolkit: "gmail", effect: "send", connection_id: "c", account: "ops@example.test", args: "{}", preview: { title: "Gmail: send", toolkit_label: "Gmail", ...(account === undefined ? {} : { account }), kind, fields } },
    })!;
  const to = (value: string) => ({ role: "to", label: "To", value });
  check(outsideDomainRecipients(email("ops@example.test", [to("mei@harbourfans.example, a@example.test"), { role: "cc", label: "Cc", value: "Accounts <ACC@Example.Test>" }])) === 1, "one outside, case-insensitive, Name <addr> read");
  check(outsideDomainRecipients(email("ops@example.test", [to("x@mail.example.test")])) === 0, "a subdomain is inside");
  check(outsideDomainRecipients(email("ops@example.test", [to("a@b.example; c@d.example"), { role: "bcc", label: "Bcc", value: "e@f.example" }])) === 3, "to, cc and bcc all count");
  check(outsideDomainRecipients(email("+852 5555 0100", [to("a@b.example")])) === 0, "a non-address account never guesses");
  check(outsideDomainRecipients(email("ops@example.test", [to("+852 5555 0123")], "message")) === 0, "a message is not an email");
  check(outsideDomainRecipients(email("ops@example.test", [{ role: "subject", label: "Subject", value: "x@y.example" }])) === 0, "only recipient roles count");
}

// CAD-2463: a connected effect's outcome in its own words, only once confirmed.
{
  const effect = (e: "send" | "draft", status: string, result?: Record<string, unknown>) =>
    parseStagedRow({ id: ACTION, tool: "connected_effect", status, createdAt: 1, ...(result ? { result } : {}), input: { slug: "gmail_send", toolkit: "gmail", effect: e, connection_id: "c", account: "a@b.test", args: "{}" } })!;
  check(stagedStatus(effect("send", "approved", { ok: true }), "applied", "Applied") === "Sent", "an applied send is Sent");
  check(stagedStatus(effect("draft", "approved", { ok: true }), "applied", "Applied") === "Draft saved", "an applied draft is Draft saved");
  check(stagedStatus(effect("send", "approved", { ok: false }), "failed", "Did not apply") === "Couldn't send", "a failed send says so");
  check(stagedStatus(effect("send", "approved"), "unconfirmed", "Result not confirmed") === "Result not confirmed", "an unconfirmed send is never Sent");
  check(stagedStatus(effect("send", "pending"), "pending", "Waiting") === "Waiting", "pending keeps the rail's word");
  check(stagedStatus(appRow(ACTION)!, "applied", "Applied") === "Applied", "an app write keeps the rail's word");
}

// CAD-2463 QA: "Show full message" only when the body is long enough to clamp.
check(!bodyNeedsClamp("Hi Mr Lee,\nsee you at 3pm."), "a short body does not clamp");
check(bodyNeedsClamp("x\n".repeat(7)), "seven lines clamp");
check(bodyNeedsClamp("x".repeat(401)), "a long single paragraph clamps");

console.log("turnCards: ok");
