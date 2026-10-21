import type * as ReactTypes from "react";
import type { Root } from "react-dom/client";
import type * as MyWorkModule from "../src/features/home/MyWork";
import type * as LocaleModule from "../src/lib/locale";
import type * as ApiModule from "../src/lib/api";
import type { MyWorkPayload } from "../src/lib/api";
import type { MyWorkView } from "../src/lib/myWorkQuery";

declare function require<T>(name: string): T;

function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}

/** CAD-1703: a tab or filter picked while a search debounces is merged into it, never reverted. */
async function main(): Promise<void> {
  type Dom = Window & Pick<typeof globalThis, "Node" | "Element" | "HTMLElement" | "SVGElement" | "Event"> & {
    happyDOM: { waitUntilComplete(): Promise<void> };
  };
  const { Window: BrowserWindow } = require<{ Window: new (options: { url: string }) => Dom }>("happy-dom");
  const win = new BrowserWindow({ url: "http://localhost/my-work" });
  Object.defineProperties(globalThis, {
    window: { value: win, configurable: true }, document: { value: win.document, configurable: true },
    navigator: { value: win.navigator, configurable: true }, location: { value: win.location, configurable: true },
    history: { value: win.history, configurable: true }, sessionStorage: { value: win.sessionStorage, configurable: true },
    localStorage: { value: win.localStorage, configurable: true }, Node: { value: win.Node, configurable: true },
    Element: { value: win.Element, configurable: true }, HTMLElement: { value: win.HTMLElement, configurable: true },
    SVGElement: { value: win.SVGElement, configurable: true }, Event: { value: win.Event, configurable: true },
    addEventListener: { value: win.addEventListener.bind(win), configurable: true },
    removeEventListener: { value: win.removeEventListener.bind(win), configurable: true },
    IS_REACT_ACT_ENVIRONMENT: { value: true, configurable: true },
  });
  const loader = require<{ prototype: { require: (this: unknown, id: string) => unknown } }>("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.call(this, id);
  };
  const React = require<typeof ReactTypes>("react");
  const { act } = require<{ act: (fn: () => unknown) => Promise<void> }>("react");
  const { createRoot } = require<{ createRoot: (host: Element) => Root }>("react-dom/client");
  const MyWork = require<typeof MyWorkModule>("../src/features/home/MyWork").default;
  const { LocaleProvider } = require<typeof LocaleModule>("../src/lib/locale");
  const { api } = require<typeof ApiModule>("../src/lib/api");
  const { setImmediate: nextTurn } = require<{ setImmediate: (callback: () => void) => void }>("node:timers");
  // The test lib is ES2023: Promise.withResolvers is not typed there.
  const settle = async () => { for (let i = 0; i < 6; i += 1) await new Promise<void>((resolve) => nextTurn(resolve)); };

  const empty: MyWorkPayload = {
    owner: "operator", issues: [], total: 0, next_cursor: null,
    counts: { active: 0, done: 0, dropped: 0, all: 0 }, status_counts: {}, projects: [],
  };
  api.myWork = () => Promise.resolve(empty);

  const host = win.document.createElement("div");
  win.document.body.appendChild(host);
  const root = createRoot(host as unknown as Element);
  const views: MyWorkView[] = [];
  let view: MyWorkView = { tab: "done" };
  const render = () => root.render(React.createElement(LocaleProvider, null,
    React.createElement(MyWork, {
      view,
      onView: (next: MyWorkView) => { views.push(next); view = next; render(); },
      onOpen: () => undefined,
      onDetails: () => undefined,
    })));
  await act(async () => { render(); });
  await act(async () => { await settle(); });
  const input = win.document.querySelector<HTMLInputElement>("#my-work-search");
  assert(input, "search box renders");
  const setter = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(input), "value")!.set!;
  await act(async () => {
    setter.call(input, "smtp");
    input.dispatchEvent(new win.Event("input", { bubbles: true }) as unknown as Event);
  });
  const allTab = [...win.document.querySelectorAll<HTMLButtonElement>(".my-work-tab")].find((b) => b.textContent?.startsWith("All"));
  assert(allTab, "All tab renders");
  await act(async () => { allTab.click(); });
  // The debounce runs on the window's timers; happy-dom flushes them on demand.
  await act(async () => { await win.happyDOM.waitUntilComplete(); });
  assert(view.tab === "all" && view.search === "smtp",
    `the search merges into the tab picked while typing (got ${JSON.stringify(view)}; history ${JSON.stringify(views)})`);
  await act(async () => { root.unmount(); });

  console.log("my work paging checks passed");
}

void main().catch((error: unknown) => {
  console.error(error);
  require<{ exitCode?: number }>("process").exitCode = 1;
});
