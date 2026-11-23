/**
 * CAD-2286 review: the kickoff dialog closes on Escape from the document, but
 * not on an Escape something inside has already handled. An open Select's
 * Escape closes its list and stops there, so it never reaches the document;
 * any other inner handler that cancels Escape leaves the dialog open too.
 */
declare function require(name: string): any;
export {};

function ok(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/" });
  for (const name of [
    "window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLButtonElement", "SVGElement",
    "navigator", "MutationObserver", "Event", "MouseEvent", "KeyboardEvent", "PointerEvent", "location", "history",
    "localStorage", "sessionStorage",
  ])
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener", "dispatchEvent"])
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

  const loader = require("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    if (id === "react-markdown") return { __esModule: true, default: () => null };
    if (id === "remark-gfm") return { __esModule: true, default: () => undefined };
    return originalRequire.apply(this, arguments);
  };
  // The options read is best-effort; refusing it keeps the local catalogue.
  globalThis.fetch = (async () =>
    new Response(JSON.stringify({ error: { code: "not_found", message: "not here" } }), {
      status: 404,
      headers: { "Content-Type": "application/json" },
    })) as typeof fetch;

  const React = require("react") as typeof import("react");
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
  const KickoffDialog = (require("../src/features/issues/KickoffDialog") as typeof import("../src/features/issues/KickoffDialog")).default;
  const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof import("../screen-ui/src/navigation");
  const { hostNavigation } = require("../src/ui/hostNavigation") as typeof import("../src/ui/hostNavigation");

  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const closed = { count: 0 };
  /** A call, so an earlier assertion never narrows the count. */
  const closes = () => closed.count;
  await React.act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(NavigationProvider, {
          adapter: hostNavigation,
          children: React.createElement(KickoffDialog, {
            id: "CAD-1",
            title: "A ticket",
            body: "",
            groups: ["master"],
            seats: [],
            blocked: null,
            onClose: () => { closed.count += 1; },
            onWrite: () => undefined,
            onDone: () => undefined,
            planHref: "/plan",
          }),
        }),
      ),
    );
  });
  await React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
  const escape = () => new win.KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });

  // An open Select's Escape closes its list only.
  const trigger = host.querySelector('button[role="combobox"]') as HTMLButtonElement | null;
  ok(trigger, "the dialog has a Select");
  await React.act(async () => { trigger.click(); });
  ok(document.querySelector('[role="listbox"]'), "the list is open");
  await React.act(async () => { trigger.dispatchEvent(escape()); });
  ok(!document.querySelector('[role="listbox"]'), "Escape closes the list");
  ok(closes() === 0, `Escape on an open list leaves the dialog open (closed ${closed.count})`);

  // An Escape another inner handler already cancelled is not the dialog's.
  const handled = escape();
  handled.preventDefault();
  await React.act(async () => { document.dispatchEvent(handled); });
  ok(closes() === 0, `a cancelled Escape leaves the dialog open (closed ${closed.count})`);

  // A plain Escape closes it.
  await React.act(async () => { document.dispatchEvent(escape()); });
  ok(closes() === 1, `a plain Escape closes the dialog (closed ${closed.count})`);

  await React.act(async () => { root.unmount(); });
  console.log("kickoff dialog escape checks passed");
}

void main().catch((err: unknown) => {
  setTimeout(() => {
    throw err;
  }, 0);
});
