export {};
/**
 * CAD-140, CAD-1216: the operator's decision surfaces at a 390px phone
 * width — the researched-idea drawer (approve/send back/park) and the
 * merge drawer (publish pinned to its head, send back with a reason)
 * opened from the real NeedsRail's To do cards. happy-dom has no layout engine, so like the CRM 390px
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

await (resources.issue("D-2") as { refreshUsing: (f: () => Promise<unknown>) => Promise<void> }).refreshUsing(async () => ({
  id: "D-2",
  project: "demo",
  title: "Light the fuselage",
  body: "## Outcome\n\nVisitors see a calm retrying message instead of an error. More detail follows.",
  status: "review",
  status_source: "direct",
  priority: "P3",
  tags: [],
  blocked_by: [],
  relates: [],
  refs: [],
  container: false,
  ready: false,
  blocked: false,
  created: "2026-09-29T00:00:00Z",
  rev: "fnv1a:00000000000000d2",
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
      permissionsHref: "/settings/permissions",
      onAsk: () => {},
      collapsed: false,
      onToggleCollapse: () => {},
      onAskAgent: () => {},
    }),
  );
});

// To do tab (the default): one line per card, one control; no kind chip,
// issue id or hash on the card.
await settle(() => assert(host.querySelector('[data-need="idea_plan"]'), "idea card renders"));
{
  const card = host.querySelector('[data-need="idea_plan"]')!;
  assert(!/D-9|idea plan ready/.test(card.textContent ?? ""), "the card carries no ticket id or technical row title");
  const title = card.querySelector(".todo-title");
  assert(title, "card has a one-line title");
  const merge = host.querySelector('[data-need="merge_decision"]')!;
  assert(!/cccc|acme\/app|r1/.test(merge.textContent ?? ""), "the merge card carries no hash, PR or reviewer");
  assert(host.querySelectorAll("[data-need-group]").length === 0, "no group headings");
  await click(Array.from(card.querySelectorAll("button")).find((b) => b.textContent === "Review"));
}

// The idea drawer: approve, send back with a reason, park with a date —
// every control reachable at 390px without horizontal push.
await settle(() => assert(host.querySelector('[data-drawer="review"]'), "review drawer renders"));
{
  const drawer = host.querySelector('[data-drawer="review"]')!;
  assert(drawer.getAttribute("data-scrim") === "board", "the scrim covers the whole board");
  assert(host.querySelector(".crm-drawer-scrim")?.getAttribute("data-scrim") === "board", "scrim is board-wide");
  const labels = Array.from(drawer.querySelectorAll("button")).map((b) => b.textContent);
  assert(labels.some((t) => t?.includes("Approve idea")), "approve offered");
  assert(labels.includes("Not now"), "Not now closes without deciding");
  assert(/1 of 2/.test(drawer.textContent ?? ""), "n of m names the position");
  assert(!/Easy to undo|Undo/.test(drawer.textContent ?? ""), "no undo promise: no server reversal exists");
  assert(!drawer.textContent?.includes("cadence "), "no command in the drawer");
  const title = drawer.querySelector(".crm-drawer-title");
  assert(title && title.textContent?.startsWith("Dark mode"), "the drawer is titled by the idea");
  await click(Array.from(drawer.querySelectorAll("button")).find((b) => b.textContent?.includes("Send back")));
  assert(host.querySelector('[aria-label="What is wrong with this idea?"]'), "send back asks why");
  await click(Array.from(drawer.querySelectorAll("button")).find((b) => b.textContent?.includes("Park it for later")));
  assert(host.querySelector("#rv-park"), "park asks until when");
  await click(Array.from(drawer.querySelectorAll("button")).find((b) => b.textContent === "Details"));
  assert(drawer.querySelector(".rv-tech")?.textContent?.includes("D-9"), "Details shows the technical fields on request");
  // Not now closes and leaves the card.
  await click(Array.from(drawer.querySelectorAll("button")).find((b) => b.textContent === "Not now"));
}
await settle(() => assert(!host.querySelector('[data-drawer="review"]'), "Not now closes the drawer"));
assert(host.querySelector('[data-need="idea_plan"]'), "the card stays after Not now");

// The merge drawer: what passed, the pinned head under Details, and the
// send-back that states why.
{
  const row = host.querySelector('[data-need="merge_decision"]')!;
  await click(Array.from(row.querySelectorAll("button")).find((b) => b.textContent === "Review"));
}
await settle(() => assert(host.querySelector('[data-drawer="review"]'), "merge drawer renders"));
{
  const drawer = host.querySelector('[data-drawer="review"]')!;
  assert(drawer.querySelector(".crm-drawer-title")?.textContent === "Light the fuselage", "titled by the change");
  assert(!drawer.textContent?.includes("cccccccccccc"), "the head is hidden until Details");
  assert(drawer.textContent?.includes("passed by a second agent"), "a Checked tick appears for the reviewer the row carries");
  assert(drawer.textContent?.includes("PASS — green at the reviewed head"), "the verdict summary the row carries is shown");
  assert(Array.from(drawer.querySelectorAll("button")).some((b) => b.textContent === "Publish"), "publish offered");
  await click(Array.from(drawer.querySelectorAll("button")).find((b) => b.textContent === "Details"));
  assert(drawer.querySelector(".rv-tech")?.textContent?.includes("head cccccccccccc"), "Details names the reviewed head");
  await click(Array.from(drawer.querySelectorAll("button")).find((b) => b.textContent?.includes("Send back")));
  assert(host.querySelector('[aria-label="What should change before this goes out?"]'), "send back asks why");
}

// 390px containment on the RENDERED tree. happy-dom has no layout
// engine — scrollWidth/clientWidth are always 0, so no harness can
// measure overflow here. What the harness CAN check on rendered nodes:
// the class contracts that keep a narrow viewport intact (wrapping
// rows, shrinking text containers), and the shipped CSS read back
// computed. The operator re-verifies `scrollWidth <= innerWidth` in a
// real browser.
const must = (el: Element | null, cls: string, why: string) => {
  assert(el, `node present: ${why}`);
  assert(el.classList.contains(cls), `${why} carries .${cls} (wrap/shrink contract)`);
};
const row = host.querySelector('[data-need="idea_plan"]')!;
must(row.querySelector(".todo-main"), "min-w-0", "card text shrinks");
const fs = require("fs");
const path = require("path");
const proc = (globalThis as any).process;
const css: string = fs.readFileSync(path.join(proc.cwd(), "src/styles.css"), "utf8");
const shipped = [".needrow", ".needmenu", ".todo-card", ".todo-title"].map((sel) => {
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
