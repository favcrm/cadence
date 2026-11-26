declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const name of [
  "window",
  "document",
  "Node",
  "Element",
  "HTMLElement",
  "Event",
  "MouseEvent",
  "location",
  "history",
]) {
  Object.defineProperty(globalThis, name, {
    value: name === "window" ? win : win[name],
    configurable: true,
    writable: true,
  });
}
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, {
    value: win[name].bind(win),
    configurable: true,
  });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

const assert = (cond: unknown, msg: string) => {
  if (!cond) throw new Error(msg);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const { allowPrivateReads, resetPrivateSession } = require("../src/lib/privateSession") as typeof import("../src/lib/privateSession");
const { threadItems, threadRuns } = require("../src/features/home/thread") as typeof import("../src/features/home/thread");
const { ContextReceiptCard } = require("../src/features/home/ContextReceiptCard") as typeof import("../src/features/home/ContextReceiptCard");

async function main() {
  allowPrivateReads();
  const undoCalls: string[] = [];
  globalThis.fetch = async (input, init) => {
    const url = new URL(String(input), "http://localhost");
    if (url.pathname === "/api/member-assistant/memory-undo") {
      const body = JSON.parse(String(init?.body ?? "{}")) as { history_id?: string };
      undoCalls.push(String(body.history_id ?? ""));
      return new Response(
        JSON.stringify({
          undone: body.history_id,
          undo_history_id: "mem_undo_1",
          op: "remember",
          target: "MEMORY.md",
          sha: "sha_before",
          revision_sha: "sha_before",
          applies: "next_turn",
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    throw new Error(`unexpected fetch ${url.pathname}`);
  };

  // Grouping: context_receipt sits inside the assistant's ThreadRun.
  const entries = [
    { seq: 1, role: "operator", kind: "message", text: "Remember my timezone", message: "m1", created: "2026-11-26T01:00:00Z" },
    { seq: 2, role: "agent", kind: "turn_result", text: "Noted.", message: "m1", created: "2026-11-26T01:00:02Z" },
    {
      seq: 3,
      role: "agent",
      kind: "context_receipt",
      text: "+1 fact in MEMORY.md",
      message: "m1",
      created: "2026-11-26T01:00:02Z",
      payload: {
        op: "remember",
        history_id: "mem_abc123",
        target: "MEMORY.md",
        added_count: 1,
        removed_count: 0,
        added_preview: ["- 2026-11-26: Timezone is Asia/Taipei"],
        removed_preview: [],
        undone_history_id: null,
      },
    },
  ];
  const items = threadItems({ exists: true, entries, pending: [] });
  assert(items.some((i) => i.type === "context_receipt"), "threadItems maps context_receipt kind");
  const runs = threadRuns(items);
  assert(runs.length === 2 && runs[1].turn && runs[1].items.some((i) => i.type === "context_receipt"), "receipt stays grouped inside assistant turn");

  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);
  await React.act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(ContextReceiptCard, { entry: entries[2] }),
      ),
    );
  });
  assert(host.textContent?.includes("+1 fact") && host.textContent?.includes("MEMORY.md"), "receipt shows compact summary and target");
  const viewLink = host.querySelector<HTMLAnchorElement>("a[href='/agents/assistant?tab=memory']");
  assert(viewLink !== null, "View link targets personal assistant Memory tab");

  const diffBtn = Array.from(host.querySelectorAll("button")).find((b) => b.textContent === "Diff");
  assert(diffBtn !== undefined, "Diff toggle is rendered when preview lines exist");
  await React.act(async () => {
    diffBtn!.click();
  });
  assert(host.textContent?.includes("Timezone is Asia/Taipei"), "expanding Diff reveals added preview line");

  const undoBtn = Array.from(host.querySelectorAll("button")).find((b) => b.textContent === "Undo");
  assert(undoBtn !== undefined, "1-click Undo button is rendered");
  await React.act(async () => {
    undoBtn!.click();
    await Promise.resolve();
  });
  assert(JSON.stringify(undoCalls) === JSON.stringify(["mem_abc123"]), "clicking Undo posts history_id to memory-undo");
  assert(host.textContent?.includes("Undone"), "receipt marks change Undone after success");

  await React.act(() => root.unmount());
  resetPrivateSession();
  console.log("context receipt card checks passed");
}

main().catch((err) => {
  resetPrivateSession();
  throw err;
});
export {};
