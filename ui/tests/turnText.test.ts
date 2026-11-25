import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import type * as ThreadViewModule from "../src/features/home/ThreadView";
import { clockRuns, threadItems, threadRuns, turnStartMs, type ThreadEntry, type ThreadState } from "../src/features/home/thread";
import { dayText } from "../src/features/home/DayDivider";
import { translateUiText } from "../src/lib/locale";
import {
  elapsedSecs,
  errorReason,
  formatElapsed,
  isIdToken,
  resultCount,
  shortId,
  splitIds,
  toolKey,
  toolLabel,
  toolSubject,
  unwrapHarnessText,
} from "../src/features/home/turnText";
declare function require(name: string): any;

// The same Node seam threadView.test.ts uses to load the shared identity UI.
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const { ThreadTurn } = require("../src/features/home/ThreadView") as typeof ThreadViewModule;
loader.prototype.require = originalRequire;

function check(condition: boolean, message: string): void {
  if (!condition) throw new Error(`turnText: ${message}`);
}

// CAD-2363: elapsed time, in English and through the zh catalogue's templates.
const zh: Record<string, string> = { "{s}s": "{s} 秒", "{m}m {s}s": "{m} 分 {s} 秒", "{h}h {m}m": "{h} 小時 {m} 分" };
const tZh = (english: string) => zh[english] ?? english;
check(formatElapsed(8) === "8s" && formatElapsed(65) === "1m 5s" && formatElapsed(3780) === "1h 3m", "English units");
check(formatElapsed(8, tZh) === "8 秒" && formatElapsed(65, tZh) === "1 分 5 秒" && formatElapsed(3780, tZh) === "1 小時 3 分", "written Chinese units");
check(formatElapsed(-3) === "0s" && formatElapsed(59.9) === "59s", "never negative, whole seconds");
check(elapsedSecs(1000, 10_000) === 9 && elapsedSecs(10_000, 1000) === null && elapsedSecs(null, 1) === null && elapsedSecs(0, 300) === null, "elapsedSecs measures forward whole seconds only");

// Tool labels and subjects.
check(toolKey('app_call {"q":"Gia"}') === "app_call" && toolKey("web_search: cafes") === "web_search", "tool key");
check(toolLabel('app_call {"app":"accounting","op":"contacts.search","q":"Gia"}') === "Use an app", "a known tool has a label");
for (const [name, label] of [["agent_message", "Message an agent"], ["find_tools", "Find tools"], ["use_tool", "Use a tool"], ["tool_result_read", "Read a tool result"], ["connected_read", "Read a connected account"], ["connected_action", "Propose an account action"], ["catalog_search", "Search tools"], ["mcp_call_read", "Read from a connector"], ["workspace_skill_draft", "Draft a workspace skill"], ["workspace_skill", "Load a workspace skill"]] as const) {
  check(toolLabel(`${name} {"q":"x"}`) === label, `${name} is labelled ${label}`);
  check(translateUiText(label, "zh-TW") !== label, `${label} has a written-Chinese string`);
}
check(toolSubject('workspace_skill_draft {"name":"weekly-report"}') === "weekly-report", "workspace_skill_draft extracts the skill name");
check(toolLabel("bash: cadence board read") === null && toolLabel("master ask-form") === null, "an unknown tool has none");
check(toolSubject('app_call {"app":"accounting","op":"contacts.search","q":"Gia"}') === "contacts.search · Gia", "an app call leads with its operation, then the naming field");
check(toolSubject('agent_message {"action":"send","alias":"bookkeeper","context_app":"accounting","text":"draft supplier bill"}') === "bookkeeper · accounting", "agent_message send shows alias and context_app without body text");
check(toolSubject('agent_message {"action":"send","alias":"bookkeeper","text":"draft supplier bill"}') === "bookkeeper", "agent_message send shows alias alone when context_app is absent");
check(toolSubject('agent_message {"action":"read","message":"msg-123"}') === "read · msg-123", "agent_message read shows action and message id");
check(toolSubject('agent_message {"action":"inbox","alias":"bookkeeper"}') === "inbox · bookkeeper", "agent_message inbox shows action and alias");
check(toolSubject('web_search {"query":"cafés","api_key":"k_123"}') === "cafés", "only naming fields are shown, never other arguments");
check(toolSubject('artifact_show {"id":"art-5f3a","version":1}') === "art-5f3a", "an id when nothing names it better");
check(toolSubject("web_search: cafés in Central") === "cafés in Central", "a plain argument");
check(toolSubject('artifact_show {"request_id":"req_9f8e","title":"Q3 report"') === "", "an argument that does not parse shows nothing, not a guessed value");
{
  // The row reads the call's full text: a long call whose stored summary is cut at 120 still names its subject.
  const long = `app_call ${JSON.stringify({ app: "accounting", op: "contacts.search", filters: { kind: "supplier", since: "2026-01-01", tags: ["a", "b", "c"] }, q: "Gia" })}`;
  check(long.length > 120 && toolSubject(long) === "contacts.search · Gia", "a long call keeps its real subject");
}
check(resultCount('{"rows":[]}') === 0 && resultCount("[1,2,3]") === 3 && resultCount('{"items":[{}]}') === 1, "result counts");
check(resultCount('{"id":"x"}') === null && resultCount("ok") === null, "no count for a single record or plain text");
check(toolSubject("board_read") === "" && toolSubject('app_call {"version":1}') === "", "no subject when there is none");
check(toolSubject(`web_read: ${"x".repeat(200)}`).length === 60, "a long subject is cut to one line");

// Identifier tokens.
check(isIdToken("7d2e4c1a9b3f40e8a6c5d2b1f0e9a873") && isIdToken("art-5f3a90c1e2d84b7a9c6e01f2ab47d3c9"), "hex ids, with or without a prefix");
check(!isIdToken("1234567890123456789") && !isIdToken("deadbeef") && !isIdToken("see 7d2e4c1a9b3f40e8a6c5d2b1f0e9a873"), "not a number, not short, not a sentence");
check(shortId("7d2e4c1a9b3f40e8a6c5d2b1f0e9a873") === "7d2e…a873" && shortId("art-5f3a90c1e2d84b7a9c6e01f2ab47d3c9") === "art-5f3a…d3c9", "middle truncation keeps the prefix");
check(
  JSON.stringify(splitIds("deny 828c10186233aefed7dabd65ed0a758a, not order 4111111111111111 or CAD-2362")) ===
    JSON.stringify([
      { id: false, text: "deny " },
      { id: true, text: "828c10186233aefed7dabd65ed0a758a" },
      { id: false, text: ", not order 4111111111111111 or CAD-2362" },
    ]),
  "ids split out of prose; numbers and issue ids stay",
);
check(splitIds("x-7d2e4c1a9b3f40e8a6c5d2b1f0e9a873y").every((p) => !p.id), "no token inside a longer word");

// The day divider uses the board's language and names the weekday.
{
  const now = Date.parse("2026-11-16T12:00:00");
  const earlier = Date.parse("2026-11-08T12:00:00");
  const en = dayText(earlier, now, (k) => k, "en");
  const zhTw = dayText(earlier, now, (k) => k, "zh-TW");
  check(/Sun/.test(en) && /Nov/.test(en), `English date with weekday: ${en}`);
  check(/11月8日/.test(zhTw) && /週日|星期日/.test(zhTw), `written Chinese date with weekday: ${zhTw}`);
  const todayZh = dayText(now, now, (k) => k, "zh-TW");
  check(todayZh.startsWith("Today · ") && /11月16日/.test(todayZh), `today leads with the catalogue word, then the date: ${todayZh}`);
}

// Rendered turns.
const at = (sec: number) => `2026-10-02T10:00:${String(sec).padStart(2, "0")}Z`;
const agent = (seq: number, kind: string, text: string, sec: number, payload: Record<string, unknown> | null = null): ThreadEntry => ({
  seq,
  role: "agent",
  kind,
  text,
  payload,
  message: "m1",
  created: at(sec),
});
const items = (entries: ThreadEntry[]) => threadItems({ exists: true, entries, pending: [] } as ThreadState);
const turn = (entries: ThreadEntry[], props: Record<string, unknown> = {}) =>
  renderToStaticMarkup(createElement(ThreadTurn, { items: items(entries), onOpenIssue: () => undefined, ...props }));

const entries = [
  agent(1, "thinking", "Look the contact up.", 3),
  agent(2, "tool_call", 'app_call {"app":"accounting","op":"contacts.search","q":"Gia"}', 4, { tool_use_id: "t1" }),
  agent(3, "tool_result", '{"rows":[]}', 5, { tool_use_id: "t1" }),
  agent(4, "turn_result", "Staged action `7d2e4c1a9b3f40e8a6c5d2b1f0e9a873`; also deny 828c10186233aefed7dabd65ed0a758a.\n\n```\n7d2e4c1a9b3f40e8a6c5d2b1f0e9a873\n```\n\nOrder 4111111111111111.", 9),
];
const started = Date.parse(at(0));

// Review of #1778: a turn starts at its own send, never at a queued row.
{
  const op = (seq: number, message: string, sec: number): ThreadEntry => ({ seq, role: "operator", kind: "message", text: "hi", payload: null, message, created: at(sec) });
  const ag = (seq: number, kind: string, text: string, sec: number, message: string, payload: Record<string, unknown> | null = null): ThreadEntry => ({ seq, role: "agent", kind, text, payload, message, created: at(sec) });
  // A: the send at 0s, a queued send at 2s, then the first send's turn from 3s to 10s.
  const a = items([op(1, "m-a", 0), op(2, "m-b", 2), ag(3, "thinking", "x", 3, "m-a"), ag(4, "turn_result", "done", 10, "m-a")]);
  const runA = threadRuns(a).find((r) => r.turn);
  check(runA?.turn === true && turnStartMs(a, runA.items) === Date.parse(at(0)), "case A: the start is the turn's own send, not the queued row before it");
  // B: a queued send inside the turn splits it in two segments; the start is still the send, and only the first segment has the clock.
  const b = items([
    op(1, "m-a", 0),
    ag(2, "thinking", "x", 1, "m-a"),
    ag(3, "tool_call", 'web_search: a', 2, "m-a", { tool_use_id: "t1" }),
    ag(4, "tool_result", "ok", 2, "m-a", { tool_use_id: "t1" }),
    op(5, "m-b", 3),
    ag(6, "tool_call", 'web_search: b', 5, "m-a", { tool_use_id: "t2" }),
    ag(7, "tool_result", "ok", 5, "m-a", { tool_use_id: "t2" }),
    ag(8, "turn_result", "done", 10, "m-a"),
  ]);
  const runsB = threadRuns(b).filter((r) => r.turn);
  check(runsB.length === 2, `case B splits into two segments, got ${runsB.length}`);
  const starts = runsB.map((r) => (r.turn ? turnStartMs(b, r.items) : null));
  check(starts.every((ms) => ms === Date.parse(at(0))), `case B: both segments start at the send, got ${JSON.stringify(starts)}`);
  const clocks = clockRuns(threadRuns(b));
  check(clocks.size === 1 && clocks.has(runsB[0].key), "case B: only the first segment carries the clock");
  const last = runsB[1];
  const finished = last.turn ? renderToStaticMarkup(createElement(ThreadTurn, { items: last.items, startedAt: starts[1], clock: false, onOpenIssue: () => undefined })) : "";
  check(finished.includes(">Thought for 10s<"), "case B: the finished segment measures from the send, as the live clock did");
  const twoLive = runsB.map((r) => (r.turn ? renderToStaticMarkup(createElement(ThreadTurn, { items: r.items, running: true, startedAt: Date.parse(at(0)), clock: clocks.has(r.key), onOpenIssue: () => undefined })) : ""));
  check(twoLive.join("").split("data-turn-clock").length - 1 === 1, "two live segments of one send paint one clock");
  // No message id: no send to find, so the head counts from its own first entry.
  const hosted = items([op(1, "", 0), { ...ag(2, "thinking", "x", 4, ""), message: null }, { ...ag(3, "turn_result", "done", 9, ""), message: null }]);
  const runH = threadRuns(hosted).find((r) => r.turn);
  check(runH?.turn === true && turnStartMs(hosted, runH.items) === null, "rows with no message id: no start from an unrelated row");
}

// One clock: the finished turn measures from the same start the live clock counts from.
const done = turn(entries, { startedAt: started });
check(done.includes(">Thought for 9s<"), "finished turn measures from its start (the send), not its first entry");
check(turn(entries).includes(">Thought for 6s<"), "without a start it measures from its first entry, as before");
const live = turn(entries.slice(0, 3), { startedAt: started, running: true });
check(live.includes("data-turn-clock") && !live.includes(">now<"), "a running turn shows its clock beside the name, not 'now'");
check((live.match(/data-turn-clock/g) ?? []).length === 1, "exactly one clock");
check(turn(entries.slice(0, 3), { running: true }).includes("data-turn-clock"), "a running turn without a start counts from its first entry");

// Readable steps, raw behind the toggle.
check(done.includes("Use an app") && done.includes("· contacts.search · Gia"), "a known tool reads as what it did and its subject");
check(done.includes(">0 results<"), "a known tool's JSON result reads as a count");
{
  const failed = turn([
    agent(1, "tool_call", 'web_search {"query":"cafes"}', 1, { tool_use_id: "e" }),
    agent(2, "tool_result", '{"error":"rate limited, retry later"}', 2, { tool_use_id: "e", is_error: true }),
    agent(3, "turn_result", "Could not search.", 3),
  ]);
  check(failed.includes("rate limited, retry later"), "a known tool's error keeps its reason in the row");
  check(failed.includes(">rate limited, retry later<"), "the reason reads as text, not raw JSON; the raw result stays in the title");
}
{
  check(
    unwrapHarnessText("<harness>\n[error] /fields/0/options: is required for choice and multichoice fields\n</harness>") ===
      "/fields/0/options: is required for choice and multichoice fields",
    "unwrapHarnessText extracts [error] diagnostic",
  );
  check(
    errorReason('<harness>\n[error] {"error":"bad json args"}\n</harness>') === "bad json args",
    "errorReason unwraps JSON inside <harness>",
  );
  check(unwrapHarnessText("<harness>\n[error] partial") === "partial", "unwrapHarnessText handles unclosed block");
  const harnessFailed = turn([
    agent(1, "tool_call", 'ask_form {"fields":[{"id":"a"}]}', 1, { tool_use_id: "h1" }),
    agent(2, "tool_result", "<harness>\n[error] /fields/0/options: is required for choice and multichoice fields\n</harness>", 2, { tool_use_id: "h1", is_error: true }),
    agent(3, "turn_result", "Done.", 3),
  ]);
  check(
    harnessFailed.includes("/fields/0/options: is required for choice and multichoice fields") &&
      !harnessFailed.includes(">&lt;harness&gt;<"),
    "failed tool step renders unwrapped diagnostic instead of bare <harness>",
  );
}
{
  // Review notes on #1778: a saved artifact still reads "in the canvas"; one result is singular;
  // an unknown tool's error and the raw view show the raw result, as on main.
  const rows = turn([
    agent(1, "tool_call", 'artifact_put {"title":"Report"}', 1, { tool_use_id: "p" }),
    agent(2, "tool_result", '{"id":"art-1","version":1}', 2, { tool_use_id: "p" }),
    agent(3, "tool_call", 'board_list_issues {"q":"open"}', 3, { tool_use_id: "l" }),
    agent(4, "tool_result", '[{"id":"CAD-1"}]', 4, { tool_use_id: "l" }),
    agent(5, "tool_call", "bash: ls", 5, { tool_use_id: "b" }),
    agent(6, "tool_result", '{"error":"denied"}', 6, { tool_use_id: "b", is_error: true }),
    agent(7, "turn_result", "Done.", 7),
  ]);
  check(rows.includes(">in the canvas<"), "a hosted artifact save reads 'in the canvas', like the native one");
  check(rows.includes(">1 result<"), "one result is singular");
  check(rows.includes(">{&quot;error&quot;:&quot;denied&quot;}<"), "an unknown tool's error shows its raw result, as on main");
}
check(done.includes('title="app_call {&quot;app&quot;'), "the raw call stays in the row's title");
check(done.includes("Show raw calls") && done.includes('aria-pressed="false"'), "a raw toggle sits in the trace");
check(done.includes("turntrace-summary") && /· Use an app/.test(done), "the closed trace names the tool readably");
const unknown = turn([agent(1, "tool_call", "bash: cadence board read", 1, { tool_use_id: "b" }), agent(2, "tool_result", "ok", 2, { tool_use_id: "b" }), agent(3, "turn_result", "Done.", 3)]);
check(unknown.includes("Bash") && unknown.includes("bash: cadence board read"), "an unknown tool keeps its own name and raw row");

// Identifier tokens in the answer; code blocks and numbers untouched.
check((done.match(/data-id-token/g) ?? []).length === 2, "both ids in prose become tokens");
check(done.includes(">7d2e…a873<") && done.includes(">828c…758a<"), "tokens are middle-truncated");
check(done.includes('title="7d2e4c1a9b3f40e8a6c5d2b1f0e9a873"'), "the full id is the title");
check(/<pre[^>]*>[\s\S]*7d2e4c1a9b3f40e8a6c5d2b1f0e9a873[\s\S]*<\/pre>/.test(done), "a fenced block keeps the full id");
check(done.includes("4111111111111111"), "a number is never a token");
{
  const linked = turn([agent(1, "turn_result", "[`7d2e4c1a9b3f40e8a6c5d2b1f0e9a873`](https://example.test/a)", 1)]);
  check(!linked.includes("data-id-token") && /<a [^>]*>[\s\S]*7d2e4c1a9b3f40e8a6c5d2b1f0e9a873/.test(linked), "an id inside a link stays code, never a button in an anchor");
}

// Every string the turn renders has a written-Chinese entry (Browser QA found "in the canvas" untranslated).
for (const key of ["in the canvas", "1 result", "{n} results", "Show raw calls", "Hide raw calls", "Copy ID", "{s}s", "{m}m {s}s", "{h}h {m}m", "Today", "Yesterday", "Use an app", "Save an artifact", "Delivery unconfirmed", "Assistant could not finish this turn"])
  check(translateUiText(key, "zh-TW") !== key, `zh-TW has "${key}"`);

console.log("turnText: ok");
