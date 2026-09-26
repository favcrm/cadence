/**
 * Searchable Select focus (CAD-617 review). Escape and a click outside
 * must put focus back on the trigger. With the search field open, closing
 * used to drop focus on document.body — these assertions fail if that
 * restore is removed.
 *
 * happy-dom is installed on globalThis before React loads. Imports are
 * hoisted, so the component is required, not imported.
 */
declare function require(name: string): any;

function assert(cond: unknown, what: string): void {
  if (!cond) throw new Error(what);
}

const happy = require("happy-dom") as {
  Window: new (options?: { url?: string }) => {
    document: Document;
    Node: typeof Node;
    Element: typeof Element;
    HTMLElement: typeof HTMLElement;
    HTMLInputElement: typeof HTMLInputElement;
    HTMLButtonElement: typeof HTMLButtonElement;
    DocumentFragment: typeof DocumentFragment;
    SVGElement: typeof SVGElement;
    navigator: Navigator;
    MutationObserver: typeof MutationObserver;
    Event: typeof Event;
    KeyboardEvent: typeof KeyboardEvent;
    MouseEvent: typeof MouseEvent;
    PointerEvent: typeof PointerEvent;
    FocusEvent: typeof FocusEvent;
    getComputedStyle: typeof getComputedStyle;
  };
};
const win = new happy.Window({ url: "http://localhost/" });
const g = globalThis as Record<string, unknown>;
function install(name: string, value: unknown): void {
  Object.defineProperty(g, name, { value, configurable: true, writable: true });
}
install("window", win);
install("document", win.document);
install("Node", win.Node);
install("Element", win.Element);
install("HTMLElement", win.HTMLElement);
install("HTMLInputElement", win.HTMLInputElement);
install("HTMLButtonElement", win.HTMLButtonElement);
install("DocumentFragment", win.DocumentFragment);
install("SVGElement", win.SVGElement);
install("navigator", win.navigator);
install("MutationObserver", win.MutationObserver);
install("Event", win.Event);
install("KeyboardEvent", win.KeyboardEvent);
install("MouseEvent", win.MouseEvent);
install("PointerEvent", win.PointerEvent);
install("FocusEvent", win.FocusEvent);
install("getComputedStyle", win.getComputedStyle.bind(win));
install("IS_REACT_ACT_ENVIRONMENT", true);

// The icon package's "cjs" build is still .js under "type": "module", so
// Node refuses to require it. Focus does not depend on the glyphs.
const nodeModule = require("module");
const originalRequire = nodeModule.prototype.require;
nodeModule.prototype.require = function (this: unknown, id: string) {
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};

const react = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const { Select } = require("../src/ui/Select") as typeof import("../src/ui/Select");
const { act } = react;

const options = [
  { value: "grok", label: "grok-4.7-high" },
  { value: "swe", label: "swe-2-high" },
  { value: "glm", label: "openrouter/z-ai/glm-5.3-flash" },
];

function openSearchable(): HTMLButtonElement {
  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);
  act(() => {
    root.render(
      react.createElement(Select, {
        "aria-label": "model",
        searchable: true,
        value: "grok",
        onChange: () => undefined,
        options,
      }),
    );
  });
  const trigger = host.querySelector("button");
  if (!trigger) throw new Error("missing trigger");
  act(() => {
    trigger.dispatchEvent(new win.MouseEvent("click", { bubbles: true }));
  });
  const search = document.querySelector("input[aria-label='Search options']");
  assert(search instanceof HTMLElement, "searchable select shows a search field");
  assert(document.activeElement === search, "opening a searchable select focuses the search field");
  return trigger;
}

function searchField(): HTMLInputElement {
  const search = document.querySelector("input[aria-label='Search options']");
  if (!(search instanceof HTMLInputElement)) throw new Error("search field is not open");
  return search;
}

// Escape closes and returns focus to the trigger, not the page.
{
  const trigger = openSearchable();
  const search = searchField();
  act(() => {
    search.dispatchEvent(new win.KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
  });
  assert(document.querySelector("input[aria-label='Search options']") === null, "escape closes the list");
  assert(document.activeElement === trigger, "escape focuses the trigger");
  document.body.replaceChildren();
}

// Clicking outside closes and returns focus to the trigger.
{
  const trigger = openSearchable();
  act(() => {
    document.body.dispatchEvent(new win.PointerEvent("pointerdown", { bubbles: true }));
  });
  assert(document.querySelector("input[aria-label='Search options']") === null, "click outside closes the list");
  assert(document.activeElement === trigger, "click outside focuses the trigger");
  document.body.replaceChildren();
}

// Tab closes and leaves focus alone so it can move to the next control.
{
  const trigger = openSearchable();
  const search = searchField();
  let prevented = false;
  act(() => {
    const event = new win.KeyboardEvent("keydown", { key: "Tab", bubbles: true, cancelable: true });
    search.dispatchEvent(event);
    prevented = event.defaultPrevented;
  });
  assert(document.querySelector("input[aria-label='Search options']") === null, "tab closes the list");
  assert(prevented === false, "tab is not cancelled");
  assert(document.activeElement !== trigger, "tab does not pull focus back to the trigger");
}

export {};
