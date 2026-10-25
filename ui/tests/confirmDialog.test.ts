export {};
/**
 * CAD-1810: the shared ConfirmDialog must take focus on open, keep Tab
 * inside, close on Escape from anywhere in the dialog, and restore focus
 * to the opener.
 */
declare function require(name: string): any;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/settings/connections" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLSelectElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage", "localStorage"])
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener"])
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  Object.defineProperty(globalThis, "requestAnimationFrame", { value: (fn: () => void) => setTimeout(fn, 0), configurable: true });
  const loader = require("module"), originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, arguments);
  };

  const React = require("react") as typeof import("react");
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
  const ConfirmDialog = (require("../src/features/app-shell/shared/ConfirmDialog") as typeof import("../src/features/app-shell/shared/ConfirmDialog")).default;

  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  const flush = () => React.act(async () => { await sleep(0); });
  const settle = async (check: () => void, budget = 4000) => {
    const end = Date.now() + budget;
    for (;;) {
      await flush();
      try { check(); return; } catch (e) { if (Date.now() > end) throw e; await sleep(20); }
    }
  };
  const click = async (el: Element) => {
    await React.act(async () => { el.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
    await flush();
  };
  const key = async (el: Element, init: KeyboardEventInit) => {
    await React.act(async () => {
      el.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init }));
    });
    await flush();
  };

  const renderHarness = async (gated: boolean) => {
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    const Harness = () => {
      const [open, setOpen] = React.useState(false);
      return React.createElement(React.Fragment, null,
        React.createElement("button", { id: "open-confirm", onClick: () => setOpen(true) }, "Open"),
        open
          ? React.createElement(ConfirmDialog, {
              title: "Confirm",
              body: "Body",
              expect: gated ? "7" : undefined,
              match: "7",
              inputLabel: "Code",
              confirmLabel: "Delete",
              pending: false,
              error: null,
              onConfirm: () => setOpen(false),
              onCancel: () => setOpen(false),
            })
          : null,
      );
    };
    await React.act(async () => { root.render(React.createElement(LocaleProvider, null, React.createElement(Harness))); });
    const opener = host.querySelector("#open-confirm") as HTMLButtonElement;
    assert(opener, "opener renders");
    opener.focus();
    await click(opener);
    await settle(() => assert(host.querySelector('[role="dialog"]'), "dialog opens"));
    return { host, root, opener, dialog: host.querySelector('[role="dialog"]') as HTMLElement };
  };

  // No typed gate: the dialog container takes focus, and Tab wraps.
  {
    const { root, opener, dialog } = await renderHarness(false);
    assert(document.activeElement === dialog, "dialog container receives initial focus");
    const buttons = Array.from(dialog.querySelectorAll("button")) as HTMLButtonElement[];
    assert(buttons.length === 2, "Cancel and Confirm render");
    const [first, last] = [buttons[0]!, buttons[1]!];

    await key(dialog, { key: "Tab" });
    assert(document.activeElement === first, "Tab from the container moves to Cancel");
    await key(last, { key: "Tab" });
    assert(document.activeElement === first, "Tab from the last control wraps to Cancel");
    await key(first, { key: "Tab", shiftKey: true });
    assert(document.activeElement === last, "Shift+Tab from the first control wraps to Confirm");

    await key(last, { key: "Escape" });
    await settle(() => assert(!document.querySelector('[role="dialog"]'), "Escape closes the dialog"));
    assert(document.activeElement === opener, "focus returns to the opener");
    await React.act(async () => root.unmount());
  }

  // Typed gate: the input takes initial focus; Shift+Tab wraps inside.
  {
    const { root, opener, dialog } = await renderHarness(true);
    const input = dialog.querySelector("input") as HTMLInputElement;
    assert(document.activeElement === input, "typed gate focuses its input");
    const buttons = Array.from(dialog.querySelectorAll("button")) as HTMLButtonElement[];
    const last = buttons.filter((button) => !button.disabled).at(-1)!;
    await key(input, { key: "Tab", shiftKey: true });
    assert(document.activeElement === last, "Shift+Tab from the gated input wraps inside the dialog");
    await key(last, { key: "Escape" });
    await settle(() => assert(!document.querySelector('[role="dialog"]'), "Escape closes the gated dialog"));
    assert(document.activeElement === opener, "gated dialog restores the opener");
    await React.act(async () => root.unmount());
  }

  console.log("confirm dialog focus checks passed");
}
void main();
