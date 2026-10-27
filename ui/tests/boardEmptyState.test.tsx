/**
 * CAD-1820: a tracker that has never had an issue must not also say
 * "No issues match this view." The two messages disagree about why the list is
 * empty (a new tracker vs filters that exclude everything), and a company
 * arriving on a freshly cut-over board reads the pair as a failed migration.
 */
import type * as HappyDom from "happy-dom";
import type * as ReactNs from "react";
import type * as ReactDomClient from "react-dom/client";
import type * as LocaleModule from "../src/lib/locale";
import type { IssueList as IssueListType } from "../src/features/projects/Board";

declare function require(name: string): unknown;
declare const process: { exit(code: number): never };
export {};

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

type ModuleLoader = {
  prototype: { require: (this: unknown, id: string) => unknown };
};

function namedExport(mod: unknown, name: string): unknown {
  assert(mod && typeof mod === "object" && name in mod, `module must export ${name}`);
  return Reflect.get(mod, name);
}

async function main() {
const { Window } = require("happy-dom") as typeof HappyDom;
const win = new Window({ url: "http://localhost/" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"] as const)
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"] as const)
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module") as ModuleLoader;
const originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, [id]);
};
const React = require("react") as typeof ReactNs;
const { createRoot } = require("react-dom/client") as typeof ReactDomClient;
const { LocaleProvider } = require("../src/lib/locale") as typeof LocaleModule;
const IssueList = namedExport(require("../src/features/projects/Board"), "IssueList") as typeof IssueListType;

const host = document.createElement("div");
document.body.appendChild(host);
const root = createRoot(host);

const render = async (trackerEmpty: boolean) => {
  await React.act(async () => {
    root.render(React.createElement(LocaleProvider, null,
      React.createElement(IssueList, {
        issues: [],
        project: "all",
        onOpen: () => {},
        trackerEmpty,
      })));
  });
  return host.textContent ?? "";
};

// A tracker that never had an issue: the kicker above the list carries the
// message; the list must not contradict it.
const fresh = await render(true);
assert(!fresh.includes("No issues match this view."), "a new tracker must not claim the view filtered everything out");

// A tracker with issues that this view filters out still gets the filter line.
const filtered = await render(false);
assert(filtered.includes("No issues match this view."), "a filtered empty view still explains the filter");
assert(!filtered.includes("the tracker has no issues yet"), "a filtered view must not claim the tracker is empty");

await React.act(async () => { root.unmount(); });
console.log("board empty-state checks passed");
}
void main().catch((e: unknown) => { console.error(e); process.exit(1); });
