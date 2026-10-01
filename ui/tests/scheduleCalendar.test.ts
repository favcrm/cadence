export {};
/** CAD-980 — mounted Schedule date-row calendar QA with an injected stub
 *  publish client. Synthetic intents only — no fetch, no provider, no send.
 *  Proves the approved date-row/fixed-240px/horizontal-strip presentation and
 *  every honesty rule the coordinator required (no publish-without-intent, no
 *  unknown→refused, multi-intent history retained, conflict withheld, missing
 *  effect flagged-unavailable, cross-context excluded, scope-tagged reads,
 *  malformed plans surfaced, unplanned rows explicit, cap notice honest). */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/app-installations/install-a" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLDialogElement", "HTMLInputElement", "HTMLSelectElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"]) {
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
}
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const ScheduleCalendar = (require("../src/features/workspace-apps/ScheduleCalendar") as typeof import("../src/features/workspace-apps/ScheduleCalendar")).default;
const { projectSchedule } = require("../src/features/workspace-apps/ScheduleCalendar") as typeof import("../src/features/workspace-apps/ScheduleCalendar");

function assert(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }
const host = document.createElement("div"); document.body.append(host);
const root = createRoot(host);
const flush = () => React.act(async () => { await new Promise((r) => setTimeout(r, 0)); });
const text = () => host.textContent ?? "";
const dayEls = () => Array.from(host.querySelectorAll(".wa-cal-day"));

// --- synthetic host rows -----------------------------------------------------
import type { WorkspaceRun, AppEffect } from "../src/features/workspace-apps/workspaceApps";
import type { PublishIntent, PublishState } from "../src/features/workspace-apps/socialPublish";

const INSTALL = "install-a";
const run = (id: string, context: string | null, state = "running", subject?: string): WorkspaceRun => ({
  id, install_id: INSTALL, context_id: context, state,
  snapshot_digest: `sd-${id}`, approved_digest: null,
  snapshot: {
    workflow: { title: `Wf ${id}`, steps: [] },
    inputs: { subject: subject ?? `Title ${id}` },
    owner_pm: "pm-a", assignments: {},
  },
  steps: [], artifacts: [], reviews: [],
});
const effect = (id: string, runId: string, context: string | null): AppEffect => ({
  effect_id: id, request: `req-${id}`, state: "waiting", needs_you: false, digest: `d-${id}`,
  authorization_kind: "app_artifact",
  authority: { install_id: INSTALL, run_id: runId, context: context ? { id: context } : null, artifact_id: "a", artifact_digest: "x", binding: { id: "b", revision: 1, digest: "bd" } },
  record: {},
});
const intent = (over: Record<string, unknown>): PublishIntent => ({
  intent_id: "i-1", install_id: INSTALL, context_id: null, run_id: "run-a",
  effect_id: "eff-a", state: "queued" as PublishState, channel: "instagram",
  destination_id: "17841400008460056", caption_digest: "c".repeat(64),
  image_digest: null, frozen_digest: "f".repeat(64), idempotency_key: "k",
  due_epoch: 1780000000, timezone: "Asia/Hong_Kong", grant_id: "g", approval_id: "a",
  writer: null, reviewer: null, permalink: null, receipt: null, refusal: null, upstream: null,
  ...over,
} as PublishIntent);

// Injectable stub client — records calls, supports deferred replies so the
// scope-switch race is a real pending→resolve→stale ordering, not a remount.
let listResult: any[] = [];
let listError: Error | null = null;
let listCalls: { installId: string; contextId: string | null; aborted: () => boolean }[] = [];
const deferred: { contextId: string | null; resolve: (v: { intents: any[] }) => void; signal: AbortSignal }[] = [];
let deferNext = false;
const stubClient = {
  list: (installId: string, contextId: string | null, signal?: AbortSignal) => {
    listCalls.push({ installId, contextId, aborted: () => !!signal?.aborted });
    if (deferNext) {
      return new Promise<{ intents: any[] }>((resolve) => {
        // Entries stay addressable after abort so a stale resolve can be fired
        // to prove the component discards a late reply for a previous scope.
        const entry = { contextId, signal: signal!, resolve };
        deferred.push(entry);
      });
    }
    if (listError) return Promise.reject(listError);
    return Promise.resolve({ intents: listResult });
  },
  // schedule/cancel are never called by this read-only view.
} as any;

let deniedCount = 0;
let openedRun: string | null = null;
const onOpenRun = (r: any) => { openedRun = r.id; };
const onDenied = () => { deniedCount++; };

// STABLE mount — same element across renders so a scope change exercises the
// component's own scope-tag logic rather than a remount. `render` forces a
// fresh read by giving the element a new key (a remount) for cases that must
// load a new intents list; `rerender` keeps the SAME instance for real
// context-switch racing.
let remountKey = 0;
let token: { n: number } = { n: 0 }; // stable parent-snapshot token
async function render(props: any) {
  await React.act(async () => {
    root.render(React.createElement(ScheduleCalendar, {
      key: `remount-${++remountKey}`,
      installId: INSTALL, contextId: "", runs: [], effects: [],
      canWrite: true, onOpenRun, onDenied, client: stubClient, refreshToken: token, ...props,
    }));
  });
  await flush();
}
// Re-render only the changing props on the SAME component instance.
async function rerender(contextId: string, runsList: any[], effectsList: any[]) {
  await React.act(async () => {
    root.render(React.createElement(ScheduleCalendar, {
      installId: INSTALL, contextId, runs: runsList, effects: effectsList,
      canWrite: true, onOpenRun, onDenied, client: stubClient, refreshToken: token,
    }));
  });
  await flush();
}
// Simulate a parent snapshot refresh: swap the token reference and re-render.
async function refreshNow(contextId = "", runsList: any[] = [], effectsList: any[] = []) {
  token = { n: token.n + 1 };
  await rerender(contextId, runsList, effectsList);
}

async function main() {
  // 1. Loading state then a real intent day row (fixed date anchor + 240px strip).
  listResult = [intent({})];
  await rerender("", [run("run-a", null, "succeeded")], [effect("eff-a", "run-a", null)]);
  assert(text().includes("Schedule"), "header renders");
  const day = dayEls().find((d) => d.getAttribute("data-date"));
  assert(day, "a real intent produces a date row");
  assert(day!.querySelector(".wa-cal-head strong"), "date anchor present");
  assert(day!.textContent!.includes("1 post"), "anchored count");
  const card = day!.querySelector(".wa-cal-card");
  assert(card, "240px card present");
  // run is `succeeded` → completed, but the posted state lives on the INTENT.
  assert(text().includes("Queued"), "intent state shown, not run completion");

  // 2. Card opens the existing run detail.
  (card as HTMLElement).dispatchEvent(new win.MouseEvent("click", { bubbles: true }));
  assert(openedRun === "run-a", "card routes to the existing run detail");

  // 3. A run with NO intent is Unplanned — explicit row, never invented date.
  listResult = [];
  await render({ runs: [run("run-solo", null, "succeeded")] });
  const unplanned = dayEls().find((d) => d.getAttribute("data-unplanned") === "true");
  assert(unplanned, "unplanned row rendered");
  assert(unplanned!.textContent!.includes("Unplanned"), "Unplanned label");
  assert(!unplanned!.getAttribute("data-date"), "unplanned row has no fabricated date");
  const uCard = unplanned!.querySelector(".wa-cal-card");
  (uCard as HTMLElement).dispatchEvent(new win.MouseEvent("click", { bubbles: true }));
  assert((openedRun as string | null) === "run-solo", "unplanned card opens its run");

  // 4. Empty when truly nothing in scope.
  await render({ runs: [] });
  assert(text().includes("No posts are planned"), "empty state honest");

  // 5. cancelled + queued intents for one run BOTH render (independent history).
  listResult = [intent({ intent_id: "i-a", state: "cancelled", due_epoch: 1780000000 }), intent({ intent_id: "i-b", state: "queued", due_epoch: 1780100000 })];
  await render({ runs: [run("run-a", null, "running")], effects: [effect("eff-a", "run-a", null)] });
  const allCards = Array.from(host.querySelectorAll(".wa-cal-day[data-date] .wa-cal-card"));
  assert(allCards.length === 2, `two intent cards retained, got ${allCards.length}`);
  assert(text().includes("Cancelled") && text().includes("Queued"), "both states shown");

  // 6. Pure-projection regressions (identity + honesty).
  const p1 = projectSchedule({
    installId: INSTALL, contextId: "", effects: [effect("eff-a", "run-a", null)],
    runs: [run("run-a", null, "running")],
    intents: [intent({ intent_id: "dup", state: "cancelled" }), intent({ intent_id: "dup", state: "queued" })],
  });
  assert(p1.days.every((d) => d.intents.every((i) => i.intent_id !== "dup")), "conflicting intent_id withholds ALL its cards");
  assert(p1.conflictingIds.includes("dup"), "conflict recorded");

  const p2 = projectSchedule({
    installId: INSTALL, contextId: "ctx-x", effects: [],
    runs: [run("run-a", "ctx-x"), run("run-b", "ctx-y")],
    intents: [intent({ intent_id: "i-x", run_id: "run-a", context_id: "ctx-x" }), intent({ intent_id: "i-y", run_id: "run-b", context_id: "ctx-y" }), intent({ intent_id: "i-z", run_id: "run-a", context_id: "ctx-y" })],
  });
  // i-y is ctx-y (out of scope); i-z claims run-a but is stamped ctx-y → dropped.
  const p2ids = p2.days.flatMap((d) => d.intents.map((i) => i.intent_id));
  assert(p2ids.includes("i-x") && !p2ids.includes("i-y") && !p2ids.includes("i-z"), `cross-context joins excluded: ${p2ids}`);

  const p3 = projectSchedule({
    installId: INSTALL, contextId: "", effects: [], // effect absent (retention gap)
    runs: [run("run-a", null)],
    intents: [intent({ intent_id: "i-orphan", effect_id: "eff-missing" })],
  });
  assert(p3.unresolvedEffects.includes("i-orphan"), "missing effect flagged unresolved, intent still kept");
  assert(p3.days.length === 1, "intent still lands on its day despite missing effect");

  const p4 = projectSchedule({
    installId: INSTALL, contextId: "", effects: [], runs: [run("run-a", null)],
    intents: [intent({ intent_id: "i-tz", timezone: "Not/AZone" }), intent({ intent_id: "i-ep", due_epoch: Number.NaN })],
  });
  assert(p4.malformed.length === 2, "malformed plans surfaced, not vanished");
  assert(p4.days.length === 0, "no day invented for malformed plans");

  // 7. Malformed plan row is visible in the mounted view (not "No posts planned").
  listResult = [intent({ intent_id: "i-tz", timezone: "Not/AZone" })];
  await render({ runs: [run("run-a", null)] });
  assert(text().includes("Unresolved"), "malformed plan renders a visible row");
  assert(!text().includes("No posts are planned"), "malformed is not silently empty");

  // 8. Load failure → honest error + Retry; does not throw the whole tree.
  listError = new Error("Operator session expired");
  await render({ runs: [] });
  assert(text().includes("Could not load the schedule"), "load error surfaced");
  assert(text().includes("Retry"), "retry offered");
  listError = null;
  listResult = [];
  (Array.from(host.querySelectorAll("button")).find((b) => b.textContent?.trim() === "Retry") as HTMLElement)
    .dispatchEvent(new win.MouseEvent("click", { bubbles: true }));
  await flush();
  assert(text().includes("No posts are planned"), "retry recovers after failure");

  // 9. Scope-tagged read — REAL deferred race. Mount ctx-A with a pending list,
  // switch to ctx-B (a second pending list), resolve B first, then resolve A
  // late. Only B's content may render; A's late reply must be discarded.
  deferNext = true;
  await rerender("ctx-a", [run("run-a", "ctx-a")], [effect("eff-a", "run-a", "ctx-a")]);
  deferNext = true; // the B read also defers
  await rerender("ctx-b", [run("run-b", "ctx-b")], []);
  deferNext = false;
  assert(deferred.length === 2, `two pending reads A and B, got ${deferred.length}`);
  const readA = deferred[0];
  const readB = deferred[1];
  assert(readA.contextId === "ctx-a" && readB.contextId === "ctx-b", "A and B tagged by scope");
  // Resolve B first (the current scope).
  await React.act(async () => {
    readB.resolve({ intents: [intent({ intent_id: "i-b", run_id: "run-b", context_id: "ctx-b" })] });
  });
  await flush();
  assert(dayEls().some((d) => d.getAttribute("data-date")), "ctx-B shows its day");
  assert(!text().includes("i-a"), "no ctx-A content in B scope");
  // Resolve the STALE A reply late — it must never repaint B's scope.
  await React.act(async () => {
    readA.resolve({ intents: [intent({ intent_id: "i-a", run_id: "run-a", context_id: "ctx-a" })] });
  });
  await flush();
  assert(!text().includes("i-a"), "stale A reply discarded — no flash of prior scope");
  assert(dayEls().some((d) => d.getAttribute("data-date")), "B scope retained after stale resolve");
  assert(readA.signal.aborted, "prior read aborted on scope switch");

  // 10. Same-install duplicate run_id across contexts never shows a foreign
  //     caption/detail: the ctx-b run wins only in ctx-b scope.
  // Foreign run placed FIRST — a naive `runs.find` would pick the foreign
  // same-id run over the valid in-scope one. Projection resolves exact
  // install+context identity, so only the scoped run's caption renders.
  listResult = [intent({ intent_id: "i-dup", run_id: "run-dup", context_id: "ctx-b" })];
  const foreignDup = { ...run("run-dup", "ctx-b", "running", "FOREIGN-INSTALL CAPTION"), install_id: "inst-OTHER" };
  await render({ contextId: "ctx-b", runs: [foreignDup, run("run-dup", "ctx-b", "running", "ctx-b caption")], effects: [] });
  assert(dayEls().some((d) => d.getAttribute("data-date")), "ctx-b dup run renders");
  assert(text().includes("ctx-b caption"), "in-scope run's caption shown");
  assert(!text().includes("FOREIGN-INSTALL CAPTION"), "foreign-install caption never leaked");

  // 11. BOTH 401 and 403 → onDenied fires and private content clears.
  const { ApiError } = require("../src/lib/api");
  for (const status of [401, 403]) {
    deniedCount = 0;
    listError = new ApiError("Operator session expired", status);
    await render({ contextId: "ctx-a", runs: [run("run-a", "ctx-a")], effects: [] });
    assert(deniedCount === 1, `${status} triggers the denied callback`);
    assert(text().includes("Could not load the schedule"), `${status} error surfaces honestly`);
    assert(!text().includes("Title run-a"), `${status}: private run detail absent after refusal`);
    listError = null;
  }

  // 12. Cap notice: RAW count is 100 even though only 1 is in-scope (99
  //     foreign-context intents). Id-order limit ≠ "most recent" wording.
  listError = null;
  listResult = [intent({ intent_id: "i-in" })]
    .concat(Array.from({ length: 99 }, (_, i) => intent({ intent_id: `i-x${i}`, run_id: "run-a", context_id: "other-ctx", install_id: INSTALL })));
  await rerender("", [run("run-a", null)], []);
  assert(text().includes("incomplete") && text().includes("Schedule history"), "cap notice honest about incompleteness");
  assert(!text().includes("most recent") && !text().includes("intent id") && !text().includes("ordered by"), "no chronology/ordering claim leaks");

  // Conflict-only reply: every duplicated intent_id row withheld AND the
  // conflict notice shows — never a false "No posts planned".
  listResult = [intent({ intent_id: "dup", run_id: "run-a" }), intent({ intent_id: "dup", run_id: "run-a", state: "queued" })];
  await render({ contextId: "", runs: [run("run-a", null)], effects: [] });
  assert(text().includes("withheld") || text().includes("appear more than once"), "conflict notice shows");
  assert(!text().includes("No posts are planned"), "conflict-only is NOT claimed empty");
  assert(dayEls().filter((d) => d.getAttribute("data-date")).length === 0, "conflicting cards withheld — no day row");

  // 13. GET-only read surface: installId is route-owned; context passed through.
  assert(listCalls.every((c) => c.installId === INSTALL), "route-owned installId");

  // 14. Parent refresh token invalidates the intents read: a queued intent
  //     becomes posted when the parent snapshot refreshes — no mutation/poll.
  listResult = [intent({ intent_id: "i-refresh", run_id: "run-a", state: "queued" })];
  await render({ contextId: "", runs: [run("run-a", null)], effects: [effect("eff-a", "run-a", null)] });
  assert(text().includes("Queued"), "queued intent shown before refresh");
  const callsBefore = listCalls.length;
  listResult = [intent({ intent_id: "i-refresh", run_id: "run-a", state: "posted", receipt: { permalink: "https://x/p" } })];
  await refreshNow("", [run("run-a", null)], [effect("eff-a", "run-a", null)]);
  assert(listCalls.length === callsBefore + 1, "token change re-reads intents (one extra list call)");
  assert(text().includes("Posted") && !text().includes("Queued"), "refreshed status shown, no stale queued");

  console.log("scheduleCalendar QA: PASS");
}

void main().then(async () => { await React.act(async () => root.unmount()); });
