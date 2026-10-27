/**
 * CAD-1811 Browser QA regression: at 360px the shared detail's six-tab
 * strip wraps instead of clipping horizontally. The Home origin keeps the
 * review drawer kind, so the narrow rule must cover both variants.
 */
import type { ResourceState } from "../src/lib/cache";
import type { IssueDetail } from "../src/lib/types";

declare function require(name: string): any;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLSelectElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history"])
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"])
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

  const loader = require("module"), originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    if (id === "react-markdown") return { __esModule: true, default: () => null };
    if (id === "remark-gfm") return { __esModule: true, default: () => undefined };
    return originalRequire.apply(this, arguments);
  };
  const React = require("react") as typeof import("react");
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
  const Detail = (require("../src/features/work-items/WorkItemDetail") as typeof import("../src/features/work-items/WorkItemDetail")).default;

  const detail: IssueDetail = {
    id: "D-1", project: "demo", title: "Ship the rail", status: "doing", status_source: "direct", priority: "P2",
    owner: "operator", blocked_by: [], relates: [], refs: [], paths: [], tags: [], container: false, ready: true, blocked: false,
    created: "2026-09-29T00:00:00Z", rev: "fnv1a:0123456789abcdef",
    checks: { done: 0, total: 1 }, work: { type: "task", type_source: "implicit", weight: 1 },
    frontmatter: {}, body: "## Acceptance\n\n- [ ] one", path: "issue.md",
    links: { children: [], blocked_by: [], blocks: [], relates: [], duplicates: [] },
    comments: [], notes_chain: [], artifacts: [], activity: [], commits: [], commits_skipped: [],
  };
  const readState: ResourceState<IssueDetail> = { data: detail, status: "ok", error: null, asOf: Date.now(), inFlight: false };

  // The shipped narrow rule, applied the way the bundle applies it.
  const fs = require("fs");
  const path = require("path");
  const css: string = fs.readFileSync(path.join((globalThis as any).process.cwd(), "src/features/work-items/workItems.css"), "utf8");
  const style = document.createElement("style");
  style.textContent = css;
  document.head.append(style);

  async function check(origin: "home" | "myWork") {
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    await React.act(async () => root.render(
      React.createElement(LocaleProvider, null, React.createElement(Detail, {
        id: "D-1", origin, detail, readState, href: "/projects/demo/issues/D-1",
        onClose: () => {}, readOnly: false, onOpen: () => {},
      })),
    ));
    await React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
    const kind = origin === "home" ? "review" : "work-item";
    const drawer = host.querySelector(`[data-drawer="${kind}"]`);
    assert(drawer, `${origin}: the shared detail renders its ${kind} drawer`);
    const tabs = drawer.querySelector(".crm-drawer-tabs") as HTMLElement | null;
    assert(tabs, `${origin}: the tab strip renders`);
    const buttons = Array.from(tabs.querySelectorAll('[role="tab"]'));
    assert(buttons.length === 6, `${origin}: six tabs render (Overview, Code, Evidence, Approvals, Activity, Audit), got ${buttons.length}`);
    const wrapped = win.getComputedStyle(tabs).flexWrap;
    assert(wrapped === "wrap", `${origin}: the tab strip wraps at narrow widths, got flex-wrap: ${wrapped}`);
    await React.act(async () => { root.unmount(); });
    host.remove();
  }

  await check("home");
  await check("myWork");

  console.log("work-item narrow tab-strip checks passed");
}

void main();
