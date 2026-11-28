export {};
/**
 * AOS-779 (CAD-2894): Grouped ActionBatch approval card in To do, sample preview,
 * Canvas artifact link, live progress polling, failed-item inspection, and
 * Retry Failed Only across pending, running, partial_failed, denied, and completed states.
 */
import type { Overview } from "../src/lib/types";
import type { ResourceState } from "../src/lib/cache";

declare function require(name: string): any;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/" });
  for (const name of [
    "window",
    "document",
    "Node",
    "Element",
    "HTMLElement",
    "HTMLInputElement",
    "HTMLSelectElement",
    "HTMLTextAreaElement",
    "SVGElement",
    "navigator",
    "MutationObserver",
    "ResizeObserver",
    "Event",
    "MouseEvent",
    "KeyboardEvent",
    "location",
    "history",
    "sessionStorage",
  ]) {
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  }
  for (const name of ["addEventListener", "removeEventListener"]) {
    Object.defineProperty(globalThis, name, {
      value: win[name].bind(win),
      configurable: true,
      writable: true,
    });
  }
  Object.defineProperty(globalThis, "crypto", {
    value: require("crypto").webcrypto,
    configurable: true,
  });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

  const loader = require("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, arguments);
  };

  const React = require("react") as typeof import("react");
  const { act } = React;
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const NeedsRail = (require("../src/features/home/NeedsRail") as typeof import("../src/features/home/NeedsRail")).default;
  const { resetTodoLocal } = require("../src/features/home/todoLocal") as typeof import("../src/features/home/todoLocal");
  const { homeDecisions } = require("../src/features/decisions/useDecisions") as typeof import("../src/features/decisions/useDecisions");
  const { parseStagedRow } = require("../src/features/decisions/stagedWrite") as typeof import("../src/features/decisions/stagedWrite");
  const { canvasSnapshot, resetCanvas } = require("../src/features/canvas/canvasStore") as typeof import("../src/features/canvas/canvasStore");

  assert(
    parseStagedRow({
      id: "c".repeat(32),
      tool: "action_batch",
      status: "approved",
      createdAt: Date.now(),
      input: { title: "Missing batch summary" },
    }) === null,
    "Approved action_batch without batch summary fails closed instead of showing Completed",
  );

  const makeItems = (count: number, statusFor = (_n: number) => "pending" as string) =>
    Array.from({ length: count }, (_, idx) => {
      const n = idx + 1;
      const st = statusFor(n);
      return {
        seq: idx,
        item_key: `item-${n}`,
        child_action_id: `child_${n}`.padEnd(32, "0"),
        tool: "connected_effect",
        label: `Send report #${n}`,
        recipient: `client-${n}@example.test`,
        status: st,
        attempts: st === "pending" || st === "denied" ? 0 : st === "failed" ? 1 : 1,
        error_code: st === "failed" ? "timeout" : null,
        error_message: st === "failed" ? `Timed out delivering report #${n}` : null,
        started_at: st === "pending" ? null : 1795239000000 + n * 10,
        finished_at: st === "applied" || st === "failed" ? 1795239000005 + n * 10 : null,
      };
    });

  let currentRow: Record<string, unknown> | null = null;
  let allItems: ReturnType<typeof makeItems> = [];
  let approveStatus = 200;
  let retryStatus = 200;
  let denyStatus = 200;
  let batchDetailStatus = 200;
  const calls: { method: string; path: string; body?: unknown }[] = [];

  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = new URL(String(input), "http://localhost");
    const method = init?.method ?? "GET";
    const body = init?.body ? JSON.parse(String(init.body)) : undefined;
    calls.push({ method, path: url.pathname, body });

    if (url.pathname === "/api/assist/actions" && method === "GET") {
      return new Response(JSON.stringify({ actions: currentRow ? [currentRow] : [] }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    }
    const batchMatch = /^\/api\/assist\/actions\/([^/]+)\/batch$/.exec(url.pathname);
    if (batchMatch && method === "GET") {
      if (batchDetailStatus !== 200) {
        return new Response(JSON.stringify({ error: { code: "owner_required" } }), { status: batchDetailStatus });
      }
      const batchSummary = (currentRow as { batch: Record<string, unknown> }).batch;
      return new Response(
        JSON.stringify({
          batch: {
            batch: batchSummary,
            items: allItems,
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    const approveMatch = /^\/api\/assist\/actions\/([^/]+)\/approve$/.exec(url.pathname);
    if (approveMatch && method === "POST") {
      if (approveStatus === 200 && currentRow) {
        currentRow = {
          ...currentRow,
          status: "approved",
          decidedAt: Date.now(),
          batch: {
            ...(currentRow.batch as Record<string, unknown>),
            status: "running",
            completed_count: 5,
            pending_count: 17,
            running_count: 0,
            failed_count: 0,
            sample_items: makeItems(5, () => "applied"),
          },
        };
        allItems = makeItems(22, (n) => (n <= 5 ? "applied" : "pending"));
      }
      return new Response(JSON.stringify({ ok: approveStatus === 200 }), { status: approveStatus });
    }
    const retryMatch = /^\/api\/assist\/actions\/([^/]+)\/retry-failed$/.exec(url.pathname);
    if (retryMatch && method === "POST") {
      if (retryStatus === 200 && currentRow) {
        allItems = makeItems(22, () => "applied");
        currentRow = {
          ...currentRow,
          status: "approved",
          result: { ok: true },
          batch: {
            ...(currentRow.batch as Record<string, unknown>),
            status: "completed",
            completed_count: 22,
            pending_count: 0,
            running_count: 0,
            failed_count: 0,
            last_error: null,
            failed_items: [],
            sample_items: allItems.slice(0, 5),
          },
        };
      }
      return new Response(JSON.stringify({ ok: retryStatus === 200 }), { status: retryStatus });
    }
    const denyMatch = /^\/api\/assist\/actions\/([^/]+)\/deny$/.exec(url.pathname);
    if (denyMatch && method === "POST") {
      if (denyStatus === 200 && currentRow) {
        allItems = makeItems(22, () => "denied");
        currentRow = {
          ...currentRow,
          status: "denied",
          decidedAt: Date.now(),
          decision_reason: body?.reason ?? null,
          batch: {
            ...(currentRow.batch as Record<string, unknown>),
            status: "denied",
            decision_reason: body?.reason ?? null,
            pending_count: 0,
          },
        };
      }
      return new Response(JSON.stringify({ ok: denyStatus === 200 }), { status: denyStatus });
    }
    return new Response(JSON.stringify({}), { status: 404 });
  }) as typeof fetch;

  const text = (el: Element | null | undefined) => (el?.textContent ?? "").replace(/\s+/g, " ").trim();
  const settle = async (fn: () => void, maxTicks = 25) => {
    let lastError: unknown;
    for (let i = 0; i < maxTicks; i += 1) {
      await act(async () => {
        await new Promise((r) => setTimeout(r, 10));
      });
      try {
        fn();
        return;
      } catch (error) {
        lastError = error;
      }
    }
    throw lastError;
  };
  const click = async (el: Element | null | undefined) => {
    assert(el, "expected element to click");
    await act(async () => {
      el.dispatchEvent(new win.MouseEvent("click", { bubbles: true, cancelable: true, detail: 1 }));
    });
  };
  const drawerEl = () => document.querySelector('.crm-drawer[data-drawer="staged"]');

  const overviewReady = {
    status: "ok",
    inFlight: false,
    error: null,
    asOf: Date.now(),
    data: {
      counts: {},
      needs_me: [],
      projects: [],
    },
  } as unknown as ResourceState<Overview>;

  async function mountRail() {
    resetTodoLocal();
    resetCanvas();
    homeDecisions.resetDecisions();
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    await act(async () => {
      root.render(
        React.createElement(NeedsRail, {
          overview: overviewReady,
          readOnly: false,
          decisions: true,
        }),
      );
    });
    return {
      cards: () => [...host.querySelectorAll(".needrow.todo")],
      host,
      unmount: async () => {
        await act(async () => {
          root.unmount();
        });
        host.remove();
      },
    };
  }

  const batchId = "b".repeat(32);
  const resetBatchPending = () => {
    allItems = makeItems(22, () => "pending");
    currentRow = {
      id: batchId,
      tool: "action_batch",
      status: "pending",
      createdAt: Date.now() - 60_000,
      decidedAt: null,
      origin: { via: "cli", kind: "agent", label: "Outreach" },
      input: {
        batch_key: "weekly-22",
        title: "Send Q4 renewal summary to 22 clients",
        summary: "Grouped client renewal outreach",
        preview_artifact_id: "art_batch_preview_1",
        total_count: 22,
      },
      batch: {
        id: batchId,
        batch_key: "weekly-22",
        title: "Send Q4 renewal summary to 22 clients",
        summary: "Grouped client renewal outreach",
        preview_artifact_id: "art_batch_preview_1",
        status: "pending",
        created_at: Date.now() - 60_000,
        decided_at: null,
        updated_at: Date.now() - 60_000,
        total_count: 22,
        completed_count: 0,
        failed_count: 0,
        pending_count: 22,
        running_count: 0,
        last_error: null,
        decision_reason: null,
        sample_items: allItems.slice(0, 5),
        failed_items: [],
      },
    };
  };

  // 1. Pending grouped card -> sample preview -> Canvas artifact link -> Inspect all items -> Approve Batch -> Running -> Partial Failed -> Retry Failed Only -> Completed
  {
    resetBatchPending();
    const realDateNow = Date.now;
    let fakeNow = realDateNow();
    Date.now = () => fakeNow;
    try {
      const rail = await mountRail();
      await settle(() => {
        assert(rail.cards().length === 1, "22-item batch renders as 1 grouped To-Do card");
      });
      const card = rail.cards()[0]!;
      assert(text(card).includes("Batch · 22"), "To-Do badge shows grouped item count");
      assert(text(card).includes("Send Q4 renewal summary to 22 clients"), "To-Do card displays batch title");
      assert(text(card).includes("+19"), "To-Do card shows remaining item chip count");

      await click(card.querySelector(".todo-hit"));
      await settle(() => assert(drawerEl() !== null, "Batch drawer opens"));
      const d = drawerEl()!;
      assert(d.querySelectorAll("[data-batch-samples] [data-batch-item]").length === 5, "Drawer renders 5-row sample preview");
      assert(d.querySelector('[role="progressbar"]')?.getAttribute("aria-valuenow") === "0", "Progress bar starts at 0");

      // Expand "Inspect all 22 items" -> fetches /api/assist/actions/:id/batch and supports filtering.
      const inspectBtn = d.querySelector("[data-batch-inspect-all]")!;
      assert(inspectBtn.getAttribute("aria-expanded") === "false", "Inspect all starts collapsed");
      await click(inspectBtn);
      await settle(() => {
        assert(d.querySelectorAll("[data-batch-all-panel] [data-batch-item]").length === 22, "Expanding Inspect all loads all 22 items");
      });
      assert(inspectBtn.getAttribute("aria-expanded") === "true", "Inspect all toggles aria-expanded");

      // Advance past ARM_MS and click Approve Batch -> transitions to `running` without auto-closing.
      fakeNow += 1_000;
      await click(d.querySelector("[data-batch-approve]"));
      await settle(() => {
        const runningDrawer = drawerEl()!;
        assert(text(runningDrawer.querySelector("[data-batch-status]")).includes("5/22"), "Approving transitions drawer live to running 5/22");
        assert(runningDrawer.querySelector('[role="progressbar"]')?.getAttribute("aria-valuenow") === "5", "Progress bar updates to 5");
      });

      // Server pauses at item #18 (`partial_failed`) -> refresh updates UI cleanly with Failed item #18 and Retry Failed Only button.
      allItems = makeItems(22, (n) => (n <= 17 ? "applied" : n === 18 ? "failed" : "pending"));
      currentRow = {
        ...currentRow!,
        status: "approved",
        batch: {
          ...(currentRow!.batch as Record<string, unknown>),
          status: "partial_failed",
          completed_count: 17,
          failed_count: 1,
          pending_count: 4,
          running_count: 0,
          updated_at: fakeNow + 50,
          last_error: 'Execution paused at "Send report #18" (timeout). Retry failed items to continue.',
          sample_items: allItems.slice(0, 5),
          failed_items: [allItems[17]!],
        },
      };
      await click(drawerEl()!.querySelector("[data-batch-refresh]"));
      await settle(() => {
        const paused = drawerEl()!;
        assert(paused.querySelector("[data-batch-failures]") !== null, "Failed items section appears on partial_failed");
        assert(text(paused.querySelector("[data-batch-failures]")).includes("Timed out delivering report #18"), "Shows item #18 failure message");
        assert(paused.querySelector("[data-batch-retry]") !== null, "Shows Retry Failed Only button");
      });

      // Retry Failed Only -> completes all 22 items and moves batch into Feed (`decided`).
      fakeNow += 1_000;
      await click(drawerEl()!.querySelector("[data-batch-retry]"));
      await settle(() => {
        assert(rail.cards().length === 0, "Completed batch leaves To-Do queue");
      });
      await click([...rail.host.querySelectorAll(".todo-tabs > button")][1]!);
      await settle(() => {
        const feedCard = rail.host.querySelector("[data-todo-decisions] .decided-card");
        assert(feedCard !== null && text(feedCard).includes("Completed · 22/22"), "Completed batch appears in Feed with 22/22 summary");
      });

      // Canvas preview button opens/focuses preview artifact in Canvas drawer cleanly.
      resetBatchPending();
      await rail.unmount();
      const rail2 = await mountRail();
      await settle(() => assert(rail2.cards().length === 1, "Fresh pending batch renders"));
      await click(rail2.cards()[0]!.querySelector(".todo-hit"));
      await settle(() => assert(drawerEl() !== null, "Reopened drawer"));
      await click(drawerEl()!.querySelector("[data-batch-preview-canvas]"));
      assert(
        canvasSnapshot().open === true &&
          canvasSnapshot().conversation === "batch:preview" &&
          canvasSnapshot().active === "art_batch_preview_1",
        "Preview link binds fallback conversation and opens preview_artifact_id in Canvas",
      );
      await rail2.unmount();
    } finally {
      Date.now = realDateNow;
    }
  }

  // 2. Deny flow & owner-only 403 / conflict 409 handling
  {
    const realDateNow = Date.now;
    let fakeNow = realDateNow();
    Date.now = () => fakeNow;
    try {
      resetBatchPending();
      const rail = await mountRail();
      await settle(() => assert(rail.cards().length === 1, "Pending batch ready for deny"));
      await click(rail.cards()[0]!.querySelector(".todo-hit"));
      await settle(() => assert(drawerEl() !== null, "Drawer open"));

      // Non-owner 403 refusal surfaces access error cleanly without clearing row silently.
      approveStatus = 403;
      fakeNow += 1_000;
      await click(drawerEl()!.querySelector("[data-batch-approve]"));
      await settle(() => {
        assert(text(drawerEl()).includes("Only the workspace owner"), "Shows owner-only error on 403");
      });
      approveStatus = 200;

      // Fresh mount for clean deny with optional note -> transitions to denied in Feed.
      await rail.unmount();
      resetBatchPending();
      const railDeny = await mountRail();
      await settle(() => assert(railDeny.cards().length === 1, "Pending batch mounted"));
      await click(railDeny.cards()[0]!.querySelector(".todo-hit"));
      await settle(() => assert(drawerEl() !== null, "Deny drawer open"));
      await click(drawerEl()!.querySelector(".dx-deny-toggle"));
      const textarea = drawerEl()!.querySelector("textarea") as HTMLTextAreaElement;
      await act(async () => {
        textarea.value = "Wait for updated CSV";
        textarea.dispatchEvent(new win.Event("input", { bubbles: true }));
      });
      fakeNow += 1_000;
      await click(drawerEl()!.querySelector("[data-batch-deny]"));
      await settle(() => assert(railDeny.cards().length === 0, "Denied batch leaves To-Do queue"));
      const denyCall = [...calls].reverse().find((c) => c.path.endsWith("/deny"));
      assert(denyCall !== undefined, "Sent POST /deny for pending batch");

      // 409 conflict on /retry-failed transitions drawer safely to decided-elsewhere state.
      retryStatus = 409;
      await railDeny.unmount();
      resetBatchPending();
      currentRow = {
        ...currentRow!,
        status: "approved",
        batch: {
          ...(currentRow!.batch as Record<string, unknown>),
          status: "partial_failed",
          completed_count: 10,
          failed_count: 1,
          pending_count: 11,
        },
      };
      const railConflict = await mountRail();
      await settle(() => assert(railConflict.cards().length === 1, "partial_failed card ready"));
      await click(railConflict.cards()[0]!.querySelector(".todo-hit"));
      await settle(() => assert(drawerEl() !== null, "partial_failed drawer open"));
      fakeNow += 1_000;
      await click(drawerEl()!.querySelector("[data-batch-retry]"));
      await settle(() => {
        assert(text(drawerEl()).includes("decided elsewhere"), "409 conflict surfaces safe already-decided state");
      });
      await railConflict.unmount();
    } finally {
      Date.now = realDateNow;
    }
  }
}

void main().catch((error) => {
  console.error(error);
  require("process").exitCode = 1;
});
