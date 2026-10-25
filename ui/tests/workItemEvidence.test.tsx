/**
 * CAD-1811 review regression: the shared work-item detail never offers
 * artifact authoring. Evidence is recorded-facts only; uploads stay on the
 * canonical issue page, so only My work's bounded comment/status writes are
 * reachable from the shared detail.
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
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLDialogElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLSelectElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history"])
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
  type DetailProps = import("../src/features/work-items/WorkItemDetail").WorkItemDetailProps;

  const detail: IssueDetail = {
    id: "D-1", project: "demo", title: "Ship the rail", status: "doing", status_source: "direct", priority: "P2", owner: "operator",
    blocked_by: [], relates: [], refs: [], paths: [], tags: [], container: false, ready: true, blocked: false,
    created: "2026-09-29T00:00:00Z", rev: "fnv1a:0123456789abcdef",
    checks: { done: 0, total: 1 },
    work: { type: "task", type_source: "implicit", weight: 1 },
    frontmatter: {}, body: "## Acceptance\n\n- [ ] one", path: "issue.md", comments: [], notes_chain: [], activity: [],
    artifacts: [{ name: "evidence.txt", size: 12 }],
    links: { children: [], blocked_by: [], blocks: [], relates: [], duplicates: [] },
  };
  const loaded: ResourceState<IssueDetail> = { data: detail, status: "ok", error: null, asOf: Date.now(), inFlight: false };
  const drafts = new Map();
  const myWork = (membership: "assigned" | "checking" | "unassigned"): DetailProps["myWork"] => ({
    owner: membership === "assigned" ? "operator" : "",
    assignment: null,
    identityKey: "operator",
    drafts,
    draftKey: `operator:D-1`,
    membership,
    onWrite: () => {},
    onError: () => {},
    writeBlock: null,
    onDraftChange: () => {},
  });

  async function render(props: Partial<DetailProps>) {
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    const readState: ResourceState<IssueDetail> = props.readState ?? loaded;
    await React.act(async () => root.render(
      React.createElement(LocaleProvider, null, React.createElement(Detail, {
        id: "D-1",
        origin: "issue",
        detail,
        readState,
        href: "/projects/demo/issues/D-1",
        onClose: () => {},
        readOnly: false,
        onOpen: () => {},
        ...props,
      })),
    ));
    await React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
    const buttons = () => Array.from(host.querySelectorAll("button")) as HTMLButtonElement[];
    const tab = (label: string) => buttons().find((b) => b.textContent?.trim() === label);
    const done = async () => { await React.act(async () => { root.unmount(); }); host.remove(); };
    return { host, tab, done };
  }

  const uploads = (host: ParentNode) =>
    host.querySelectorAll('input[type="file"]').length > 0 ||
    Array.from(host.querySelectorAll("button")).some((b) => b.textContent?.trim() === "Attach");

  const staleState: ResourceState<IssueDetail> = { data: detail, status: "stale", error: "offline", asOf: Date.now(), inFlight: false };

  for (const origin of ["home", "threads", "issue"] as const) {
    const view = await render({ origin });
    assert(!view.host.querySelector("textarea"), `${origin}: no comment box without the My work opt-in`);
    await React.act(() => view.tab("Evidence")?.click());
    assert(!uploads(view.host), `${origin}: the shared Evidence tab offers no upload`);
    assert(view.host.textContent?.includes("evidence.txt"), `${origin}: recorded evidence is still listed`);
    await view.done();
  }

  {
    const view = await render({ origin: "myWork", myWork: myWork("unassigned") });
    assert(!view.host.querySelector("textarea"), "myWork/unassigned: quick actions are off");
    await React.act(() => view.tab("Evidence")?.click());
    assert(!uploads(view.host), "myWork/unassigned: no upload");
    await view.done();
  }
  {
    const view = await render({ origin: "myWork", myWork: myWork("checking") });
    assert(view.host.textContent?.includes("Confirming this is still assigned to you"), "myWork/checking: actions are paused with a reason");
    await React.act(() => view.tab("Evidence")?.click());
    assert(!uploads(view.host), "myWork/checking: no upload");
    await view.done();
  }
  {
    const view = await render({ origin: "myWork", myWork: myWork("assigned"), readState: staleState });
    const textarea = view.host.querySelector("textarea") as HTMLTextAreaElement | null;
    assert(textarea?.disabled, "myWork/stale: the bounded comment write is disabled");
    await React.act(() => view.tab("Evidence")?.click());
    assert(!uploads(view.host), "myWork/stale: no upload");
    await view.done();
  }
  {
    const view = await render({ origin: "myWork", myWork: myWork("assigned") });
    const textarea = view.host.querySelector("textarea") as HTMLTextAreaElement | null;
    assert(textarea && !textarea.disabled, "myWork/assigned: the bounded comment write stays available");
    await React.act(() => view.tab("Evidence")?.click());
    assert(!uploads(view.host), "myWork/assigned: Evidence still offers no upload");
    await view.done();
  }

  console.log("work-item evidence checks passed");
}

void main();
