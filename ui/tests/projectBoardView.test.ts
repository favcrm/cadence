/** Preserve completion and search gestures when finished cards are collapsed. */
declare function require(name: string): any;
import type { IssueCard } from "../src/lib/types";
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/projects/cadence/board" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "SVGElement", "navigator", "MutationObserver", "Event", "MouseEvent", "location", "history"]) {
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
}
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const moduleLoader = require("module");
const originalRequire = moduleLoader.prototype.require;
moduleLoader.prototype.require = function (this: unknown, id: string) {
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const BoardComponent = (require("../src/features/projects/Board") as typeof import("../src/features/projects/Board")).default;
const Board = (props: React.ComponentProps<typeof BoardComponent>) => React.createElement(LocaleProvider, null, React.createElement(BoardComponent, props));
const { NO_FILTERS } = require("../src/lib/filters") as typeof import("../src/lib/filters");
function assert(value: unknown, what: string): asserts value { if (!value) throw new Error(what); }
const card = (id: string, title: string, status: string): IssueCard => ({
  id, title, status, project: "cadence", status_source: "file", priority: "P2",
  blocked_by: [], relates: [], refs: [], container: false, ready: true, blocked: false,
  created: "2026-09-27", rev: "fixture", counts: { comments: 0, artifacts: 0, refs: 0 },
  checks: { done: 0, total: 0 },
});
const active = card("CAD-1", "Active task", "doing");
const completed = card("CAD-2", "Finished task", "done");
const moves: { id: string; status: string }[] = [];
let revealed = false;
const props: React.ComponentProps<typeof Board> = {
  issues: { data: [active, completed], status: "ok", error: null, asOf: Date.now(), inFlight: false },
  projects: [{ key: "cadence", prefix: "CAD", components: [], repos: [], issues: 2 }],
  agents: null, health: null, project: "cadence", view: "kanban", query: "", readOnly: false,
  actor: "master", filters: NO_FILTERS, onRetry: () => {}, onView: () => {}, onQuery: () => {},
  onFilters: (filters) => { revealed = filters.showDone; }, onOpen: () => {},
  onMove: (issue, status) => { moves.push({ id: issue.id, status }); }, onCreated: () => {},
  onError: (error) => { throw error; }, onAgents: () => {},
};
const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
async function render(extra: Partial<typeof props> = {}) {
  await React.act(() => root.render(React.createElement(Board, { ...props, ...extra })));
}
function doneColumn() {
  const column = Array.from(host.querySelectorAll("section")).find((section) => section.querySelector("h2")?.textContent === "Done");
  assert(column, "Done stays available as a completion drop target while finished cards are hidden");
  return column;
}
async function dropActive() {
  const transfer = new win.DataTransfer();
  transfer.setData("text/plain", active.id);
  const event = new win.Event("drop", { bubbles: true, cancelable: true });
  // happy-dom does not populate DragEvent.dataTransfer from constructor options.
  Object.defineProperty(event, "dataTransfer", { value: transfer });
  await React.act(() => doneColumn().dispatchEvent(event));
}
async function run() {
  await render();
  const done = doneColumn();
  assert(!done.querySelector("article"), "finished cards stay hidden by default");
  const reveal = Array.from(done.querySelectorAll("button")).find((button) => button.textContent?.includes("1 done hidden"));
  assert(reveal, "collapsed finished cards expose their count and reveal action");
  await React.act(() => reveal.click());
  assert(revealed, "reveal requests the Show done filter");
  await dropActive();
  assert(moves.length === 1 && moves[0].id === active.id && moves[0].status === "done", "dropping active work completes it without changing filters");
  await render({ query: "Finished" });
  assert(doneColumn().querySelector("article")?.getAttribute("aria-label") === "CAD-2 Finished task", "search surfaces completed matches with Show done off");
  await render({ filters: { ...NO_FILTERS, showDone: true } });
  assert(doneColumn().querySelector("article"), "Show done reveals finished cards without a search");
  await render({ readOnly: true });
  await dropActive();
  assert(moves.length === 1, "read-only drops cannot request a move");
  await React.act(() => root.unmount());
  console.log("project Board interaction checks passed");
}
run().catch((error) => { console.error(error); throw error; });
export {};
