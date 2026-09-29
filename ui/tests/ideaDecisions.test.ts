export {};
/**
 * CAD-140: the operator's decision surfaces at a 390px phone width —
 * the researched-idea card (approve/reject/park) and the merge card
 * (merge pinned to its head, decline with a reason) through the real
 * NeedsRail. happy-dom has no layout engine, so like the CRM 390px
 * pass this guards the exact properties that keep a narrow viewport
 * intact — wrapping button rows, shrinking text containers, an
 * anchored overflow menu — and the operator re-verifies
 * `documentElement.scrollWidth <= innerWidth` in a real browser.
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
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
Object.defineProperty(globalThis, "EventSource", {
  configurable: true,
  value: class {
    onopen: null = null;
    onerror: null = null;
    constructor() {}
    addEventListener() {}
    removeEventListener() {}
    close() {}
  },
});
// The 390px pass: happy-dom has no layout engine (and its detached
// window cannot resize), so the width is recorded, not measured — the
// assertions below guard the wrapping/shrinking contracts that keep a
// 390px viewport intact, like the CRM pass before them.
const PASS_WIDTH = 390;
try {
  Object.defineProperty(win, "innerWidth", { value: PASS_WIDTH, configurable: true });
} catch { /* the width stays documentary */ }
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const NeedsRail = (require("../src/features/home/NeedsRail") as typeof import("../src/features/home/NeedsRail")).default;
const { resources } = require("../src/lib/resources") as typeof import("../src/lib/resources");

const host = document.createElement("div");
host.setAttribute("style", "width:390px;overflow:hidden;");
document.body.append(host);
const root = createRoot(host);
const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
const text = () => host.textContent ?? "";
async function settle(check: () => void) {
  for (let i = 0; i < 60; i++) {
    await flush();
    try { check(); return; } catch { /* keep polling */ }
  }
  await flush();
  check();
}
async function click(element: Element | undefined | null) {
  assert(element, "click target exists");
  await React.act(async () => { element.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  await flush();
}

// The idea the card decides on — seeded straight into the store, the
// way a loaded board holds it (rev included: decisions bind to it).
await (resources.issue("D-9") as { refreshUsing: (f: () => Promise<unknown>) => Promise<void> }).refreshUsing(async () => ({
  id: "D-9",
  project: "demo",
  title: "Dark mode for the board, with a very long title that must wrap instead of pushing the rail wide",
  status: "review",
  status_source: "direct",
  priority: "P3",
  tags: ["intake", "idea", "plan-ready"],
  blocked_by: [],
  relates: [],
  refs: [],
  container: false,
  ready: false,
  blocked: false,
  created: "2026-09-29T00:00:00Z",
  rev: "fnv1a:0123456789abcdef",
  counts: { comments: 0, artifacts: 0, refs: 0 },
  checks: { done: 0, total: 0 },
}));

const overview = {
  status: "ok",
  inFlight: false,
  error: null,
  asOf: Date.now(),
  data: {
    needs_me: [
      {
        kind: "idea_plan",
        title: "idea plan ready for your decision — D-9 Dark mode for the board",
        age: 300,
        project: "demo",
        command: "cadence idea decide D-9 approve",
        subject: { kind: "issue", id: "D-9" },
        audience: "operator",
      },
      {
        kind: "merge_decision",
        title: "D-2 ready to merge — light the fuselage with a very long verdict summary that must wrap",
        age: 600,
        project: "demo",
        command: "cadence delivery merge D-2",
        subject: { kind: "issue", id: "D-2" },
        audience: "operator",
        merge: { issue: "D-2", pr: "acme/app#9", sha: "c".repeat(40), reviewer: "r1", verdict_summary: "PASS — green at the reviewed head" },
      },
    ],
  },
} as unknown as ResourceState<Overview>;

await React.act(async () => {
  root.render(
    React.createElement(NeedsRail, {
      overview,
      readOnly: false,
      onOpenIssue: () => {},
      overviewHref: "/overview",
      onAsk: () => {},
      collapsed: false,
      onToggleCollapse: () => {},
      onAskAgent: () => {},
    }),
  );
});

// Decisions tab: the idea row decides inline.
await settle(() => assert(text().includes("Decisions"), "rail tabs render"));
{
  const tabs = Array.from(host.querySelectorAll(".activity-tabs button"));
  await click(tabs.find((b) => b.textContent?.includes("Decisions")));
}
await settle(() => assert(text().includes("idea plan ready"), "idea row renders"));
{
  const row = host.querySelector('[data-need="idea_plan"]');
  assert(row, "idea row present");
  const title = row.querySelector(".min-w-0.flex-1");
  assert(title, "row title shrinks inside the row");
  assert(title.classList.contains("break-words"), "long titles wrap instead of widening");
  await click(Array.from(row.querySelectorAll("button")).find((b) => b.textContent?.includes("Decide")));
}

// The idea card: approve, reject-with-reason, park-with-date — every
// control reachable at 390px without horizontal push.
await settle(() => assert(host.querySelector('[data-idea-card="D-9"]'), "idea card renders"));
{
  const card = host.querySelector('[data-idea-card="D-9"]')!;
  assert(card.classList.contains("overflow-hidden"), "card clips to its column");
  const title = card.querySelector(".text-cardtitle");
  assert(title && title.classList.contains("break-words"), "long idea titles wrap");
  const buttons = card.querySelector(".flex.flex-wrap");
  assert(buttons, "decision buttons wrap onto more lines");
  const labels = Array.from(card.querySelectorAll("button")).map((b) => b.textContent);
  assert(labels.some((t) => t?.includes("Approve idea")), "approve offered");
  await click(Array.from(card.querySelectorAll("button")).find((b) => b.textContent?.includes("Reject")));
  assert(host.querySelector('[aria-label="rejection reason"]'), "reject asks why");
  await click(Array.from(card.querySelectorAll("button")).find((b) => b.textContent === "Cancel"));
  await click(Array.from(card.querySelectorAll("button")).find((b) => b.textContent?.includes("Park")));
  assert(host.querySelector('[aria-label="park until date"]'), "park asks until when");
  await click(Array.from(card.querySelectorAll("button")).find((b) => b.textContent === "Cancel"));
}

// The merge card: the head it merges, and the decline that states why.
{
  const tabs = Array.from(host.querySelectorAll(".activity-tabs button"));
  assert(tabs.length > 0, "tabs present");
  const row = host.querySelector('[data-need="merge_decision"]');
  if (!row) {
    // PRs group folds — open it.
    const groups = Array.from(host.querySelectorAll("[data-need-group] button"));
    await click(groups.find((b) => b.textContent?.includes("PRs")));
  }
}
await settle(() => assert(host.querySelector('[data-need="merge_decision"]'), "merge row renders"));
{
  const row = host.querySelector('[data-need="merge_decision"]')!;
  await click(Array.from(row.querySelectorAll("button")).find((b) => b.textContent?.includes("Review merge")));
}
await settle(() => assert(text().includes("acme/app#9"), "merge card renders"));
{
  assert(text().includes("head cccccccccccc"), "the shown head is named");
  await click(Array.from(host.querySelectorAll("button")).find((b) => b.textContent?.includes("Decline")));
  assert(host.querySelector('[aria-label="decline reason"]'), "decline asks why");
}

// 390px containment on the RENDERED tree. happy-dom has no layout
// engine — scrollWidth/clientWidth are always 0, so no harness can
// measure overflow here (verified: a 600px child in a 390px parent
// reports scrollWidth 0). What the harness CAN check on rendered
// nodes: the computed styles that decide containment, plus the class
// contracts the Tailwind bundle (stubbed in tests) fulfills in the
// browser. The operator re-verifies `scrollWidth <= innerWidth` at
// 390px in a real browser.
const card = host.querySelector('[data-idea-card="D-9"]')!;
const must = (el: Element | null, cls: string, why: string) => {
  assert(el, `node present: ${why}`);
  assert(el.classList.contains(cls), `${why} carries .${cls} (wrap/shrink contract)`);
};
// Every text container shrinks and wraps; every button row wraps.
must(card.querySelector("header"), "min-w-0", "idea header shrinks");
must(card.querySelector(".text-cardtitle"), "break-words", "idea title wraps");
must(card.querySelector(".flex.flex-wrap"), "flex-wrap", "decision buttons wrap");
const row = host.querySelector('[data-need="idea_plan"]')!;
must(row.querySelector(".min-w-0.flex-1"), "break-words", "row title wraps");
// Shipped rail CSS, injected and read back computed: the row takes
// the rail's width (no fixed floor) and the overflow menu anchors to
// the row's right edge within a 390px column.
const fs = require("fs");
const path = require("path");
const proc = (globalThis as any).process;
const css: string = fs.readFileSync(path.join(proc.cwd(), "src/styles.css"), "utf8");
const shipped = [".needrow", ".needmenu"].map((sel) => {
  const match = css.match(new RegExp(`${sel.replace(/\./g, "\\.")}\\s*\\{([^}]*)\\}`));
  assert(match, `shipped CSS block exists: ${sel}`);
  return `${sel} {${match![1]}}`;
}).join("\n");
const style = document.createElement("style");
style.textContent = shipped;
document.head.append(style);
const cs = (el: Element) => win.getComputedStyle(el);
assert(cs(row).position === "relative", "need row positions its menu");
const menuProbe = document.createElement("div");
menuProbe.className = "needmenu";
row.append(menuProbe);
const menuCs = cs(menuProbe);
assert(menuCs.position === "absolute", "overflow menu floats");
assert(menuCs.right !== "" && menuCs.right !== "auto", "overflow menu anchors to the row's right edge");
const minW = menuCs.getPropertyValue("min-width");
assert(!/^\d+px$/.test(minW.trim()) || parseFloat(minW) <= PASS_WIDTH, `overflow menu fits 390px (min-width: ${minW})`);
const rowMinW = cs(row).getPropertyValue("min-width");
assert(rowMinW === "" || rowMinW === "0px", `need rows take the rail's width (min-width: ${rowMinW})`);
menuProbe.remove();

await React.act(async () => { root.unmount(); });
console.log("idea decisions checks passed");
}
void main();
