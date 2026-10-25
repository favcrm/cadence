/**
 * CAD-1811: a Threads issue reference opens the shared work-item detail on a
 * plain click and keeps the canonical issue page for modified clicks.
 */
declare function require(name: string): any;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/threads" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "SVGElement", "navigator", "Event", "MouseEvent", "KeyboardEvent", "location", "history"])
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

  const loader = require("module"), originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    return originalRequire.apply(this, arguments);
  };
  const React = require("react") as typeof import("react");
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const { IssueRefLink, OpenWorkItem } = require("../src/features/threads/IssueRefLink") as typeof import("../src/features/threads/IssueRefLink");
  const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof import("../screen-ui/src/navigation");
  const { hostNavigation } = require("../src/ui/hostNavigation") as typeof import("../src/ui/hostNavigation");

  async function render(node: React.ReactNode) {
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    await React.act(async () => root.render(
      React.createElement(NavigationProvider, { adapter: hostNavigation, children: node }),
    ));
    return {
      host,
      done: async () => { await React.act(async () => { root.unmount(); }); host.remove(); },
    };
  }

  {
    const opened: string[] = [];
    const view = await render(
      React.createElement(OpenWorkItem.Provider, { value: (id: string) => opened.push(id) },
        React.createElement(IssueRefLink, { id: "D-5", project: "demo" })),
    );
    const link = view.host.querySelector("a");
    assert(link?.getAttribute("href") === "/projects/demo/issues/D-5", "the reference is a real canonical link");
    let prevented = false;
    await React.act(async () => {
      const event = new MouseEvent("click", { bubbles: true, cancelable: true, button: 0 });
      link!.addEventListener("click", (e) => { prevented = e.defaultPrevented; }, { once: true });
      // The listener above runs after React's handler.
      link!.dispatchEvent(event);
      prevented = event.defaultPrevented;
    });
    assert(opened.length === 1 && opened[0] === "D-5", "a plain click opens the shared detail");
    assert(prevented, "a plain click does not also navigate to the full page");

    await React.act(async () => {
      link!.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true, button: 0, metaKey: true }));
    });
    assert(opened.length === 1, "a modified click keeps the canonical link and opens no detail");
    await view.done();
  }

  {
    const view = await render(React.createElement(IssueRefLink, { id: "D-5", project: "demo" }));
    assert(view.host.querySelector("a"), "without an opener the reference stays a link");
    await React.act(async () => {
      view.host.querySelector("a")!.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true, button: 0 }));
    });
    await view.done();
  }

  {
    const view = await render(
      React.createElement(OpenWorkItem.Provider, { value: () => { throw new Error("must not open"); } },
        React.createElement(IssueRefLink, { id: "D-5", project: null })),
    );
    assert(!view.host.querySelector("a"), "an unresolved issue stays plain text");
    assert(view.host.textContent === "D-5", "the unresolved reference still shows its id");
    await view.done();
  }

  console.log("threads issue-reference checks passed");
}

void main();
export {};
