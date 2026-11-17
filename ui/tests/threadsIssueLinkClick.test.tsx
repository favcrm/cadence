/** CAD-2387: a known issue id in a Threads agent message is a real button and
 *  clicking it calls the shared work-item opener with that id. */
declare function require(name: string): any;

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/threads" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "localStorage", "sessionStorage"]) {
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  }
  for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"]) {
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  }
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
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const { EntryRow } = require("../src/features/threads/ThreadsPage") as typeof import("../src/features/threads/ThreadsPage");
  const { OpenWorkItem } = require("../src/features/threads/IssueRefLink") as typeof import("../src/features/threads/IssueRefLink");
  loader.prototype.require = originalRequire;

  const opened: string[] = [];
  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);
  await React.act(async () => {
    root.render(
      <OpenWorkItem.Provider value={(id: string) => opened.push(id)}>
        <EntryRow entry={{ seq: 1, role: "agent", kind: "message", text: "see CAD-2380 and ZZZ-9" }} issueProjects={new Map([["CAD-2380", "cadence"]])} />
      </OpenWorkItem.Provider>,
    );
  });
  const buttons = Array.from(host.querySelectorAll("button.lnk")) as HTMLElement[];
  if (buttons.length !== 1 || buttons[0].textContent !== "CAD-2380") throw new Error("exactly the known id is a button");
  await React.act(async () => buttons[0].click());
  if (opened.length !== 1 || opened[0] !== "CAD-2380") throw new Error(`opener called with ${JSON.stringify(opened)}`);

  // A new opener function and a new, equal issue map (App re-render, issues
  // refresh) must keep the same button node: no remount, so focus survives.
  const reopened: string[] = [];
  await React.act(async () => {
    root.render(
      <OpenWorkItem.Provider value={(id: string) => reopened.push(id)}>
        <EntryRow entry={{ seq: 1, role: "agent", kind: "message", text: "see CAD-2380 and ZZZ-9" }} issueProjects={new Map([["CAD-2380", "cadence"]])} />
      </OpenWorkItem.Provider>,
    );
  });
  const after = host.querySelector("button.lnk") as HTMLElement | null;
  if (after !== buttons[0] || !buttons[0].isConnected) throw new Error("the issue button was remounted by a re-render");
  await React.act(async () => buttons[0].click());
  if (reopened.length !== 1 || reopened[0] !== "CAD-2380") throw new Error("the latest opener is not the one called");
  await React.act(async () => root.unmount());
  host.remove();
  console.log("CAD-2387 threads issue link click checks passed");
}

void main();
