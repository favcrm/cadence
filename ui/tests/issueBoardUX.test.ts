/** The default Issues view must lead to a full issue and expose creation. */
declare function require(name: string): any;
declare const process: { env: Record<string, string | undefined> };
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/projects/cadence/issues" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLDialogElement", "HTMLInputElement", "SVGElement", "navigator", "MutationObserver", "Event", "KeyboardEvent", "MouseEvent", "location", "history"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
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
const Board = (require("../src/features/projects/Board") as typeof import("../src/features/projects/Board")).default;
const Drawer = (require("../src/features/projects/Drawer") as typeof import("../src/features/projects/Drawer")).default;
const { api } = require("../src/lib/api") as typeof import("../src/lib/api");
const { NO_FILTERS } = require("../src/lib/filters") as typeof import("../src/lib/filters");
type Card = import("../src/lib/types").IssueCard;
function assert(value: unknown, what: string): asserts value { if (!value) throw new Error(what); }
const card: Card = {
  id: "CAD-1", title: "Fix onboarding", status: "doing", project: "cadence", status_source: "file", priority: "P1",
  blocked_by: [], relates: [], refs: [], container: false, ready: true, blocked: false,
  created: "2026-09-27", rev: "fixture", counts: { comments: 0, artifacts: 0, refs: 0 },
  checks: { done: 1, total: 3 },
};
const props: React.ComponentProps<typeof Board> = {
  issues: { data: [card], status: "ok", error: null, asOf: Date.now(), inFlight: false },
  projects: [{ key: "cadence", prefix: "CAD", components: [], repos: [], issues: 1 }],
  agents: null, health: null, project: "cadence", view: "list", query: "", readOnly: false,
  actor: "operator", filters: NO_FILTERS, onRetry: () => {}, onView: () => {}, onQuery: () => {},
  onFilters: () => {}, onOpen: () => {}, onMove: () => {}, onCreated: () => {}, onError: () => {}, onAgents: () => {},
};
const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const scenario = process.env.CAD_BOARD_SCENARIO ?? "all";
async function render(extra: Partial<typeof props> = {}) {
  await React.act(() => root.render(React.createElement(Board, { ...props, ...extra })));
}
function button(text: string): HTMLButtonElement | undefined {
  return Array.from(host.querySelectorAll("button")).find((item) => item.textContent?.trim() === text);
}
async function run() {
  if (scenario === "all" || scenario === "create") {
    await render();
    assert(button("New issue"), "the default list has a visible New issue action");
  }
  if (scenario === "all" || scenario === "navigation") {
    await render();
    const full = Array.from(host.querySelectorAll('a[href="/projects/cadence/issues/CAD-1"]')).find((link) => link.textContent?.includes(card.title));
    assert(full, "the issue title opens the full issue");
    assert(host.querySelector('button[aria-label="Preview CAD-1"]'), "quick peek is an explicit secondary action");
  }
  if (scenario === "all" || scenario === "group") {
    await render();
    const filters = button("filters");
    assert(filters, "filters toggle exists");
    await React.act(() => filters.click());
    assert(!button("group by epic"), "list mode does not offer a board-only grouping action");
  }
  if (scenario === "all" || scenario === "card") {
    await render({ view: "kanban" });
    const article = host.querySelector("article");
    assert(article, "kanban renders an issue card");
    assert(article.getAttribute("role") !== "button", "kanban card is not a button containing a link");
    assert(Array.from(article.querySelectorAll("a")).some((link) => link.textContent?.includes(card.title)), "card title opens full issue");
    assert(article.querySelector('button[aria-label="Preview CAD-1"]'), "card preview is explicit");
  }
  if (scenario === "all" || scenario === "form") {
    let created = 0;
    await render({ view: "list", onCreated: () => { created++; } });
    await React.act(() => button("New issue")?.click());
    const form = host.querySelector("form.board-new-issue");
    const title = form?.querySelector('input[name="title"]') as HTMLInputElement | null;
    assert(form && title, "create form has a named title field");
    await React.act(async () => {
      Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")!.set!.call(title, "Improve onboarding");
      title.dispatchEvent(new Event("input", { bubbles: true }));
    });
    const priorityLabel = Array.from(form.querySelectorAll("label")).find((label) => label.textContent === "Priority");
    const priorityTrigger = priorityLabel && document.getElementById(priorityLabel.htmlFor);
    assert(priorityTrigger, "priority has a labeled selector");
    await React.act(() => priorityTrigger.click());
    const options = document.querySelector('[role="listbox"]');
    assert(options, "priority options open");
    await React.act(() => options.dispatchEvent(new win.KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true })));
    assert(host.querySelector("form.board-new-issue") === form && title.value === "Improve onboarding", "Escape closes only the picker and keeps the issue draft");
    let attempts = 0;
    const original = api.create;
    (api as any).create = async (request: { project: string; title: string; priority: string }) => {
      assert(request.project === "cadence" && request.title === "Improve onboarding" && request.priority === "P2", "create sends the visible project, title and priority");
      attempts++;
      if (attempts === 1) throw new Error("offline");
      return { card };
    };
    await React.act(async () => { form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })); });
    assert(attempts === 1, "create sent one request");
    assert(host.querySelector('[role="alert"]')?.textContent?.includes("offline"), "failed create explains the error inline");
    assert((host.querySelector('input[name="title"]') as HTMLInputElement).value === "Improve onboarding", "failed create preserves draft");
    await React.act(async () => { form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })); });
    assert(Number(attempts) === 2 && created === 1 && !host.querySelector("form.board-new-issue"), "successful create reports once and closes the form");
    (api as any).create = original;
    await React.act(() => button("New issue")?.click());
    const newTitle = host.querySelector('form.board-new-issue input[name="title"]');
    assert(newTitle, "create form can open again");
    await React.act(() => newTitle.dispatchEvent(new win.KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true })));
    assert(!host.querySelector("form.board-new-issue"), "Escape in Title cancels the new draft");
    await render({ readOnly: true });
    assert(!button("New issue"), "read-only workspace does not offer creation");
  }
  await React.act(() => root.unmount());
  if (scenario === "all" || scenario === "drawer") {
    const opener = document.createElement("button");
    opener.textContent = "Open preview";
    document.body.append(opener);
    opener.focus();
    const drawerHost = document.createElement("div");
    document.body.append(drawerHost);
    const drawerRoot = createRoot(drawerHost);
    let closed = 0, retried = 0;
    const close = () => { closed++; };
    const retry = () => { retried++; };
    const href = "/projects/cadence/issues/CAD-1";
    await React.act(() => drawerRoot.render(React.createElement(Drawer, {
      id: card.id, detail: null, href, onClose: close, onRetry: retry,
      readState: { data: null, status: "failed", error: "offline", asOf: null, inFlight: false },
    })));
    const dialog = drawerHost.querySelector("dialog");
    assert(dialog?.open, "quick peek uses a native modal dialog");
    assert(dialog.querySelector('[role="alert"]')?.textContent?.includes("offline"), "failed preview explains its read error");
    await React.act(() => Array.from(dialog.querySelectorAll("button")).find((item) => item.textContent === "Retry")?.click());
    assert(retried === 1, "failed preview can retry");
    assert(dialog.querySelector('a[href="/projects/cadence/issues/CAD-1"]')?.textContent?.includes("Open full issue"), "full issue remains reachable after preview failure");
    const cached: import("../src/lib/types").IssueDetail = {
      ...card, frontmatter: {}, body: "Saved summary", path: "issue.md", comments: [], notes_chain: [], artifacts: [], activity: [],
      links: { children: [], blocked_by: [], blocks: [], relates: [], duplicates: [] },
    };
    await React.act(() => drawerRoot.render(React.createElement(Drawer, {
      id: card.id, detail: cached, href, onClose: close, onRetry: retry,
      readState: { data: cached, status: "ok", error: null, asOf: Date.now(), inFlight: true },
    })));
    assert(dialog.querySelector('[role="status"]')?.textContent?.includes("Refreshing preview"), "cached detail is labeled while revalidating");
    await React.act(() => drawerRoot.render(React.createElement(Drawer, {
      id: card.id, detail: cached, href, onClose: close, onRetry: retry,
      readState: { data: cached, status: "stale", error: "offline", asOf: Date.now(), inFlight: false },
    })));
    assert(dialog.querySelector('[role="status"]')?.textContent?.includes("out of date"), "stale detail is labeled after failure");
    await React.act(() => dialog.dispatchEvent(new Event("cancel", { bubbles: false, cancelable: true })));
    assert(closed === 1, "Escape cancel closes the preview");
    await React.act(() => drawerRoot.unmount());
    assert(document.activeElement === opener, "closing preview returns focus to its trigger");
  }
  console.log(`issueBoardUX ${scenario}: passed`);
}
run().catch((error) => { console.error(error); throw error; });
export {};
