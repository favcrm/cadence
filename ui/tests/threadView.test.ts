import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { entryRefs, ThreadItemView, type Density } from "../src/features/home/ThreadView";
import type { ThreadEntry, ThreadItem } from "../src/features/home/thread";

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

// Full classes.
const fullAnswer = render(items.answer, "full");
ok(fullAnswer.includes("w-6 h-6"), "full: avatar w-6 h-6");
ok(fullAnswer.includes("max-w-[85%]"), "full: bubble max-w-[85%]");
ok(fullAnswer.includes("text-body"), "full: answer text-body");
ok(render(items.tools, "full").includes("ml-8"), "full: tools indent ml-8");
ok(render(items.commentary, "full").includes("ml-8"), "full: commentary indent ml-8");
ok(render(items.operator, "full").includes("text-body"), "full: operator text-body");
ok(render(items.pending, "full").includes("text-body"), "full: pending text-body");

// Compact swaps exactly the four listed classes.
const compactAnswer = render(items.answer, "compact");
ok(compactAnswer.includes("w-5 h-5") && !compactAnswer.includes("w-6 h-6"), "compact: avatar w-5 h-5");
ok(compactAnswer.includes("max-w-[92%]") && !compactAnswer.includes("max-w-[85%]"), "compact: bubble 92%");
ok(compactAnswer.includes("text-secondary") && !compactAnswer.includes("text-body"), "compact: answer text-secondary");
ok(render(items.tools, "compact").includes("ml-7"), "compact: tools indent ml-7");
ok(!render(items.tools, "compact").includes("ml-8"), "compact: tools drops ml-8");
ok(render(items.commentary, "compact").includes("ml-7"), "compact: commentary indent ml-7");
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
  const d = density;

  // 1. Permission entries render ThreadPermission, never the divider.
  const perm = render(
    sys({ text: "Permission requested (perm-7): ls", payload: { source: "permission" } }),
    density,
  );
  ok(perm.includes('data-permission-thread="perm-7"'), `${d}: permission renders ThreadPermission`);
  ok(!isDivider(perm), `${d}: permission is not the divider`);
  // Even a long multi-line permission text takes the permission route.
  const permLong = render(
    sys({ text: `Permission requested (perm-8)\n${"x".repeat(300)}`, payload: { source: "permission" } }),
    density,
  );
  ok(permLong.includes('data-permission-thread="perm-8"'), `${d}: long permission still ThreadPermission`);
  ok(!isNote(permLong), `${d}: long permission is not a SystemNote`);

  // 2. Collapsed SystemNote vs divider.
  const bySource = render(sys({ payload: { source: "bootstrap" } }), density);
  ok(isBriefing(bySource) && bySource.includes("Session briefing"), `${d}: bootstrap source is a briefing`);
  ok(bySource.includes('aria-expanded="false"') && !bySource.includes("data-open"), `${d}: briefing closed`);
  ok(!isDivider(bySource), `${d}: briefing is not the divider`);
  const byMessage = render(sys({ message: "bootstrap-master" }), density);
  ok(isBriefing(byMessage) && byMessage.includes('aria-expanded="false"'), `${d}: bootstrap-master message is a briefing`);
  const multi = render(sys({ text: "line one\nline two" }), density);
  ok(isNote(multi) && multi.includes("Details"), `${d}: newline is a SystemNote`);
  ok(multi.includes('aria-expanded="false"') && !multi.includes("data-open"), `${d}: note closed`);
  ok(!isDivider(multi), `${d}: newline is not the divider`);
  const long = render(sys({ text: "a".repeat(241) }), density);
  ok(isNote(long), `${d}: 241 chars is a SystemNote`);
  const edge = render(sys({ text: "a".repeat(240) }), density);
  ok(isDivider(edge) && !isNote(edge), `${d}: 240 chars stays a divider`);
  const short = render(sys({ text: "just a line" }), density);
  ok(isDivider(short) && short.includes("just a line"), `${d}: short line is the divider`);
  ok(!isNote(short) && !isBriefing(short), `${d}: short line is not collapsed`);
  const from = render(sys({ text: "joined", payload: { from: "alice" } }), density);
  ok(from.includes("alice: ") && from.includes("joined"), `${d}: payload.from prefix`);
  ok(!short.includes(": just"), `${d}: no prefix without payload.from`);

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
  ok((withRefs.match(/class="refchip/g) ?? []).length === 2, `${d}: one chip per ref`);
  ok(withRefs.includes(">issue:CAD-1<") || withRefs.includes("issue:CAD-1"), `${d}: issue chip label`);
  ok(withRefs.includes("agent:w1"), `${d}: agent chip label`);
  const noRefs = render(items.operator, density);
  ok(!noRefs.includes("refchip") && !noRefs.includes("refsrow"), `${d}: operator without refs has no chips`);
  const pendingRefs = render(
    {
      type: "pending",
      key: "pr",
      pending: { message: "m2", text: "go", state: "sent", at: 1, refs: [{ kind: "issue", id: "CAD-9" }] },
    },
    density,
  );
  ok((pendingRefs.match(/class="refchip/g) ?? []).length === 1 && pendingRefs.includes("issue:CAD-9"), `${d}: pending refs render`);
  ok(!render(items.pending, density).includes("refchip"), `${d}: pending without refs has no chips`);
}
