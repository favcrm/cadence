import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import PermissionCard from "../src/features/home/PermissionCard";
import type * as ThreadViewModule from "../src/features/home/ThreadView";
declare function require(name: string): any;
declare const process: { cwd(): string };
import {
  threadItems,
  type ThreadEntry,
  type ThreadItem,
  type ThreadState,
} from "../src/features/home/thread";

type Density = ThreadViewModule.Density;

// Install the existing Node fixture seam before loading the shared identity UI.
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const { entryRefs, StreamBanner, ThreadItemView, ThreadTurn } =
  require("../src/features/home/ThreadView") as typeof ThreadViewModule;
loader.prototype.require = originalRequire;

/**
 * CAD-1029: the shared master-thread renderer. "full" is what Home has
 * always rendered; "compact" swaps only avatar size, body text, bubble
 * width and the step/commentary indent. Every data-kind/data-pending
 * hook survives in both densities.
 */

function ok(cond: boolean, what: string): void {
  if (!cond) throw new Error(`failed: ${what}`);
}

const entry = (over: Partial<ThreadEntry>): ThreadEntry => ({
  seq: 1,
  role: "assistant",
  kind: "assistant_message",
  text: "hello",
  created: "2026-10-02T10:00:00Z",
  ...over,
});

/** CAD-1306: the daemon's real "Permission requested" message, from the format string in
 *  src/daemon/master_rpc.rs. Fails loudly if the literal moves or changes shape. */
const DAEMON_ID = "mp-0123abcd4567";
function daemonPermissionMessage(id: string): string {
  const src: string = require("fs").readFileSync(
    require("path").join(process.cwd(), "..", "src", "daemon", "master_rpc.rs"),
    "utf8",
  );
  const m = src.match(/"(Permission requested \(\{\}, risk \{\}\):[^"]*)"/);
  if (!m) throw new Error("master_rpc.rs: the `Permission requested ({}, risk {}): ...` format string is missing or changed");
  const fmt = m[1].replace(/\\n/g, "\n");
  if ((fmt.match(/\{\}/g) ?? []).length !== 4) throw new Error(`master_rpc.rs: expected 4 placeholders, got: ${fmt}`);
  const args = [id, "high", "ls -la", "look around"];
  return fmt.replace(/\{\}/g, () => args.shift() as string);
}

const noop = () => undefined;
const render = (item: ThreadItem, density?: Density, readOnly = false) =>
  renderToStaticMarkup(
    createElement(ThreadItemView, {
      item,
      readOnly,
      density,
      onOpenIssue: noop,
      onRetry: noop,
      onDiscard: noop,
    }),
  );

// CAD-1306: the thread card is the compact card, like the To do rail's.
{
  const html = renderToStaticMarkup(
    createElement(PermissionCard, {
      card: { id: DAEMON_ID, argv: "ls", cwd: "/", reason: "look around", risk: "high", prefix: null, status: "pending", decisionLabel: "" },
      readOnly: true,
      onDone: noop,
      compact: true,
    }),
  );
  ok(html.includes('data-permission-risk="high"') && html.includes("High risk"), "compact card shows the risk line");
  ok(html.includes("Allow once") && html.includes("Always ▾") && html.includes("Deny ▾"), "compact card uses the rail's labels");
  ok(html.includes("Master says:") && html.includes("disabled"), "compact card shows the reason and is read-only");
}

const items: Record<string, ThreadItem> = {
  operator: { type: "operator", key: "o", entry: entry({ role: "operator", kind: "operator_message", text: "hi" }) },
  answer: { type: "answer", key: "a", entry: entry({ text: "an **answer**" }) },
  commentary: { type: "commentary", key: "c", entry: entry({ kind: "assistant_text", text: "thinking" }) },
  tools: {
    type: "tools",
    key: "t",
    entries: [entry({ seq: 2, kind: "tool_call", text: "ls", payload: { id: "x" } })],
  },
  system: { type: "system", key: "s", entry: entry({ role: "system", kind: "system", text: "note" }) },
  pending: {
    type: "pending",
    key: "p",
    pending: { message: "m1", text: "sending", state: "failed", error: "boom", at: 1 },
  },
};

// Home's hooks, unchanged, in both densities.
for (const density of [undefined, "full", "compact"] as const) {
  const d = density ?? "default";
  ok(render(items.answer, density).includes('data-kind="answer"'), `${d}: answer kind`);
  ok(render(items.commentary, density).includes('data-kind="commentary"'), `${d}: commentary kind`);
  ok(render(items.tools, density).includes('data-kind="tools"'), `${d}: tools kind`);
  ok(render(items.system, density).includes('data-kind="system"'), `${d}: system kind`);
  const pending = render(items.pending, density);
  ok(pending.includes('data-pending="failed"'), `${d}: pending state`);
  ok(pending.includes("Retry") && pending.includes("Discard"), `${d}: retry/discard links`);
  ok(render(items.operator, density).includes("hi"), `${d}: operator text`);
}

// Default is "full", byte-identical to an explicit "full".
for (const [name, item] of Object.entries(items)) {
  ok(render(item) === render(item, "full"), `${name}: default equals full`);
}

// Full classes. CAD-1168 (approved mock): no avatars; the label reads
// "Assistant"/"You" above the body; steps and commentary are unindented.
const fullAnswer = render(items.answer, "full");
ok(!fullAnswer.includes("w-6 h-6") && !fullAnswer.includes("w-5 h-5"), "full: no avatar");
ok(fullAnswer.includes("Assistant") && !fullAnswer.includes("Master"), "full: Assistant label, no Master");
ok(fullAnswer.includes("chat-bubble"), "full: the answer renders its bubble surface");
ok(fullAnswer.includes("text-body"), "full: answer text-body");
ok(!render(items.tools, "full").includes("ml-8"), "full: tools unindented");
ok(render(items.operator, "full").includes("text-body"), "full: operator text-body");
ok(render(items.operator, "full").includes(">You<"), "full: operator labelled You");
ok(render(items.pending, "full").includes("text-body"), "full: pending text-body");

// Compact swaps the text size (and drops the bubble cap).
const compactAnswer = render(items.answer, "compact");
ok(!compactAnswer.includes("w-5 h-5") && !compactAnswer.includes("w-6 h-6"), "compact: no avatar");
ok(compactAnswer.includes("text-secondary") && !compactAnswer.includes("text-body"), "compact: answer text-secondary");
ok(!render(items.tools, "compact").includes("ml-7"), "compact: tools unindented");
ok(!render(items.operator, "compact").includes("text-body"), "compact: operator text-secondary");
ok(!render(items.pending, "compact").includes("text-body"), "compact: pending text-secondary");

// entryRefs stays exported and tolerant.
ok(entryRefs({ refs: [{ kind: "issue", id: "CAD-1" }, { kind: 3 }] }).length === 1, "entryRefs keeps typed refs");
ok(entryRefs(null).length === 0, "entryRefs of null is none");

// CAD-1062: system routing and ref chips, asserted in both densities.
const sys = (over: Partial<ThreadEntry>): ThreadItem => ({
  type: "system",
  key: "sx",
  entry: entry({ role: "system", kind: "system", text: "short note", ...over }),
});
const isDivider = (html: string) => html.includes('data-kind="system"');
const isNote = (html: string) => html.includes('data-kind="system-note"');
const isBriefing = (html: string) => html.includes('data-kind="briefing"');

for (const density of ["full", "compact"] as const) {

  // 1. Permission entries render ThreadPermission, never the divider.
  const perm = render(
    sys({ text: "Permission requested (perm-7): ls", payload: { source: "permission" } }),
    density,
  );
  ok(perm.includes('data-permission-thread="perm-7"'), `${density}: permission renders ThreadPermission`);
  ok(!isDivider(perm), `${density}: permission is not the divider`);
  // CAD-1306: the message is built from the daemon's own format string, read from source.
  const real = render(
    sys({ text: daemonPermissionMessage(DAEMON_ID), payload: { source: "permission" } }),
    density,
  );
  ok(real.includes(`data-permission-thread="${DAEMON_ID}"`), `${density}: the daemon's real permission message becomes the card`);
  // Even a long multi-line permission text takes the permission route.
  const permLong = render(
    sys({ text: `Permission requested (perm-8)\n${"x".repeat(300)}`, payload: { source: "permission" } }),
    density,
  );
  ok(permLong.includes('data-permission-thread="perm-8"'), `${density}: long permission still ThreadPermission`);
  ok(!isNote(permLong), `${density}: long permission is not a SystemNote`);

  // 2. Collapsed SystemNote vs divider.
  const bySource = render(sys({ payload: { source: "bootstrap" } }), density);
  ok(isBriefing(bySource) && bySource.includes("Session briefing"), `${density}: bootstrap source is a briefing`);
  ok(bySource.includes('aria-expanded="false"') && !bySource.includes("data-open"), `${density}: briefing closed`);
  ok(!isDivider(bySource), `${density}: briefing is not the divider`);
  const byMessage = render(sys({ message: "bootstrap-master" }), density);
  ok(isBriefing(byMessage) && byMessage.includes('aria-expanded="false"'), `${density}: bootstrap-master message is a briefing`);
  const multi = render(sys({ text: "line one\nline two" }), density);
  ok(isNote(multi) && multi.includes("Details"), `${density}: newline is a SystemNote`);
  ok(multi.includes('aria-expanded="false"') && !multi.includes("data-open"), `${density}: note closed`);
  ok(!isDivider(multi), `${density}: newline is not the divider`);
  const long = render(sys({ text: "a".repeat(241) }), density);
  ok(isNote(long), `${density}: 241 chars is a SystemNote`);
  const edge = render(sys({ text: "a".repeat(240) }), density);
  ok(isDivider(edge) && !isNote(edge), `${density}: 240 chars stays a divider`);
  const short = render(sys({ text: "just a line" }), density);
  ok(isDivider(short) && short.includes("just a line"), `${density}: short line is the divider`);
  ok(!isNote(short) && !isBriefing(short), `${density}: short line is not collapsed`);
  const from = render(sys({ text: "joined", payload: { from: "alice" } }), density);
  ok(from.includes('text-center">alice: joined</span>'), `${density}: payload.from prefix`);
  ok(short.includes('text-center">just a line</span>'), `${density}: no prefix without payload.from`);

  // 3. Ref chips.
  const withRefs = render(
    {
      type: "operator",
      key: "or",
      entry: entry({
        role: "operator",
        kind: "operator_message",
        text: "see",
        payload: { refs: [{ kind: "issue", id: "CAD-1" }, { kind: "agent", id: "w1" }] },
      }),
    },
    density,
  );
  ok((withRefs.match(/class="refchip/g) ?? []).length === 2, `${density}: one chip per ref`);
  const noRefs = render(items.operator, density);
  ok(!noRefs.includes("refchip") && !noRefs.includes("refsrow"), `${density}: operator without refs has no chips`);
  const pendingRefs = render(
    {
      type: "pending",
      key: "pr",
      pending: { message: "m2", text: "go", state: "sent", at: 1, refs: [{ kind: "issue", id: "CAD-9" }] },
    },
    density,
  );
  ok((pendingRefs.match(/class="refchip/g) ?? []).length === 1, `${density}: pending refs render`);
  ok(!render(items.pending, density).includes("refchip"), `${density}: pending without refs has no chips`);
}

// CAD-1419: a failed turn renders its reason, not an empty Assistant bubble.
// The error string is the daemon's real payload.error for a 503 billing
// refusal, as the live thread API returned it (status failed, empty text).
{
  const billingError =
    '503: {"message":"This company has no billing account yet; its owner sets one up.","type":"server_error","code":"billing_unavailable"}';
  const failed = render(
    {
      type: "answer",
      key: "tf",
      entry: entry({ kind: "turn_result", role: "assistant", text: "", payload: { status: "failed", error: billingError } }),
    },
    "full",
  );
  ok(failed.includes('data-kind="turn-failure"') && failed.includes('role="alert"'), "failed turn: alert row");
  ok(failed.includes("billing is not set up") && failed.includes("owner needs to set up billing"), "failed turn: billing reason");
  ok(!failed.includes("data-kind=\"answer\""), "failed turn: no empty answer body");
  ok(!failed.includes("Retry"), "failed turn: no retry on an executed turn");
}

// The worker-transcript turn (Home): one quiet head, one folded trace, one
// Markdown answer, then the artifact cards. Grouping is a RENDER decision —
// the flat items `threadItems` returns are unchanged.
{
  const at = (seq: number) => `2026-10-02T10:00:${String(seq).padStart(2, "0")}Z`;
  const agent = (
    seq: number,
    kind: string,
    text: string,
    payload: Record<string, unknown> | null = null,
  ): ThreadEntry => ({ seq, role: "agent", kind, text, payload, message: "m1", created: at(seq) });
  const state = (entries: ThreadEntry[]): ThreadState => ({ exists: true, entries, pending: [] });
  const count = (html: string, needle: string) => html.split(needle).length - 1;
  const renderTurn = (items: ThreadItem[], running = false, allowLegacy = false) =>
    renderToStaticMarkup(
      createElement(ThreadTurn, { items, running, allowLegacy, onOpenIssue: noop }),
    );

  const completeItems = threadItems(
    state([
      agent(1, "thinking", "Read the board, then the pending queue."),
      agent(2, "assistant_text", "Board read; one pending action."),
      agent(3, "tool_call", "Bash: cadence board read", { tool_use_id: "t1" }),
      agent(4, "tool_result", "2 issues", { tool_use_id: "t1" }),
      agent(5, "turn_result", "**One** action waits.\n\n| id | title |\n|---|---|\n| PILOT-1 | smoke |"),
      agent(6, "artifact", "", { id: "art-a", version: 1, kind: "csv", title: "Board snapshot", bytes: 4200 }),
    ]),
  );
  const complete = renderTurn(completeItems);
  ok(count(complete, 'data-kind="turn"') === 1, "turn: exactly one turn");
  ok(complete.includes('data-turn-state="complete"'), "turn: completed state");
  ok(count(complete, 'class="turn-head"') === 1, "turn: one head");
  ok(complete.includes(">Assistant<"), "turn: head names the assistant");
  ok(complete.includes("turn-time"), "turn: head carries the time");
  ok(!complete.includes("GLM") && !complete.includes("Working"), "turn: no model and no status chip in the head");
  ok(count(complete, 'data-kind="turn-trace"') === 1, "turn: one trace disclosure");
  ok(complete.includes('aria-expanded="false"'), "turn: trace starts collapsed");
  ok(complete.includes('inert=""') && complete.includes('aria-hidden="true"'), "turn: collapsed trace is inert");
  ok(complete.includes("Thought for 4s"), "turn: the trace measures the turn");
  ok(complete.includes(">Reasoning<") && complete.includes(">Worker note<"), "turn: reasoning and the worker note are trace rows");
  ok(!complete.includes('data-kind="commentary"'), "turn: no standalone commentary row");
  ok(count(complete, 'data-kind="tools"') === 0, "turn: the tool run adds no second disclosure");
  ok(count(complete, "step-row") >= 1 && complete.indexOf("step-row") > complete.indexOf("turn-trace"), "turn: tool rows list flat inside the trace");
  // The answer: Markdown through Md, not the raw text.
  ok(count(complete, 'data-kind="answer"') === 1, "turn: one answer surface");
  ok(complete.includes("<strong>One</strong>"), "turn: answer Markdown bold");
  ok(!complete.includes("answer-kicker"), "turn: no redundant Answer kicker bar");
  ok(complete.includes("turn-copy") && complete.includes(">Copy<"), "turn: Copy action remains available in the head");
  ok(count(complete, "answer-copy") === 0, "turn: the answer surface carries no copy chrome over its text");
  ok(complete.includes("<table>") && complete.includes(">PILOT-1<"), "turn: answer Markdown table");
  ok(!complete.includes("| id | title |"), "turn: no literal Markdown pipes");
  // Artifacts come after the answer, still the existing card.
  ok(count(complete, 'data-kind="artifact"') === 1, "turn: the artifact card survives");
  ok(complete.indexOf('data-kind="artifact"') > complete.indexOf('data-kind="answer"'), "turn: artifacts after the answer");
  ok(!renderTurn(completeItems, true).includes('data-kind="typing"'), "turn: a landed answer ends the typing state");

  // Streaming: the collapsed trace and three dots only.
  const runningItems = threadItems(
    state([agent(1, "thinking", "Reading the board."), agent(2, "tool_call", "Bash: cadence board read")]),
  );
  const running = renderTurn(runningItems, true);
  ok(running.includes('data-turn-state="running"'), "streaming: running state");
  ok(running.includes("Thinking…"), "streaming: the trace says it is thinking");
  ok(count(running, 'data-kind="typing"') === 1, "streaming: one typing row");
  ok(count(running, "<i></i>") === 3, "streaming: exactly three dots");
  ok(!running.includes('data-kind="answer"'), "streaming: no answer body");
  ok(!running.toLowerCase().includes("skeleton"), "streaming: no skeleton");
  ok(!running.includes("data-assistant-activity") && !running.includes("Working"), "streaming: no status chip");
  const initialTyping = renderTurn([], true);
  ok(initialTyping.includes('data-turn-state="running"') && count(initialTyping, 'data-kind="typing"') === 1, "streaming: initial 0-step turn shows typing bubble immediately");
  ok(!initialTyping.includes('data-kind="no-answer"') && !initialTyping.includes('data-kind="turn-trace"'), "streaming: initial 0-step turn has no trace or no-answer line");

  // A transcript from before `turn_result`: the last worker note reads as the
  // reply, with the quiet earlier-transcript marker.
  const legacy = renderTurn(
    threadItems(state([agent(1, "assistant_text", "An older reply.")])),
    false,
    true,
  );
  ok(legacy.includes('data-kind="answer"') && legacy.includes("An older reply."), "legacy: the last note is the answer");
  ok(legacy.includes('data-earlier="true"') && legacy.includes("Earlier transcript"), "legacy: marked as earlier transcript");
  ok(!legacy.includes(">Worker note<"), "legacy: the reply is not repeated inside the trace");

  // A current transcript's last note is not an answer merely because it is
  // last: Home enables legacy promotion only when the whole thread has no
  // turn_result.
  const errorNote = renderTurn(
    threadItems(state([agent(1, "assistant_text", "Provider error")])),
  );
  ok(!errorNote.includes('data-kind="answer"'), "current note: no legacy answer");
  ok(errorNote.includes('data-kind="no-answer"'), "current note: no answer state");

  // A tool-turn note is not a reply: it stays in the trace and the completed
  // turn says no answer returned instead of inventing one from position.
  const toolTurn = renderTurn(
    threadItems(
      state([
        agent(1, "assistant_text", "Checking the board."),
        agent(2, "tool_call", "Bash: cadence board read"),
      ]),
    ),
  );
  ok(!toolTurn.includes('data-kind="answer"'), "tool turn: no invented answer");
  ok(toolTurn.includes('data-kind="no-answer"'), "tool turn: no answer state");
  ok(toolTurn.includes("Checking the board."), "tool turn: the note stays in the trace");
  const toolsOnly = renderTurn(threadItems(state([agent(1, "tool_call", "Bash: cadence board read")])));
  ok(toolsOnly.includes('data-kind="no-answer"'), "tools only: no answer state");
  const emptyAnswer = renderTurn(threadItems(state([agent(1, "turn_result", "   ")])));
  ok(emptyAnswer.includes('data-kind="no-answer"'), "empty answer: no-answer state");
  ok(!emptyAnswer.includes('data-kind="answer"'), "empty answer: no empty surface");

  // A failed turn keeps its reason and no empty answer body.
  const failure =
    '503: {"message":"This company has no billing account yet; its owner sets one up.","type":"server_error","code":"billing_unavailable"}';
  const failed = renderTurn(
    threadItems(state([agent(1, "turn_result", "", { status: "failed", error: failure })])),
  );
  ok(failed.includes('data-kind="turn-failure"') && failed.includes('role="alert"'), "failed turn: alert row");
  ok(failed.includes("billing is not set up"), "failed turn: billing reason");
  ok(!failed.includes('data-kind="answer"'), "failed turn: no empty answer body");

  // Reconnecting: one quiet notice with Retry — never a stale card or label.
  const banner = renderToStaticMarkup(createElement(StreamBanner, { onRetry: noop }));
  ok(count(banner, 'data-kind="stale"') === 1, "reconnect: one notice");
  ok(banner.includes('role="status"') && banner.includes("banner-spinner"), "reconnect: a status line with a spinner");
  ok(banner.includes("Reconnecting — showing the last saved turn") && banner.includes("Retry"), "reconnect: copy and Retry");
  ok(!banner.includes("border ") && !banner.includes("bg-warn"), "reconnect: no stale card styling");
}
