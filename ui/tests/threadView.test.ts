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
