export {};
/** CAD-980 — HorizontalStrip scrollStep behavior regression. Mounted, measures
 *  the actual scrollBy() distance/behavior — not prop presence or class names.
 *  No fetch/provider; synthetic DOM only. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent"]) {
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
}
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const { HorizontalStrip } = require("../src/features/workspace-apps/HorizontalStrip") as typeof import("../src/features/workspace-apps/HorizontalStrip");

function assert(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }
const host = document.createElement("div"); document.body.append(host);
const root = createRoot(host);
const flush = () => React.act(async () => { await new Promise((r) => setTimeout(r, 0)); });

// Force overflow + capture scrollBy arguments on the live element.
function instrument(el: HTMLElement, clientWidth = 1000) {
  Object.defineProperties(el, {
    clientWidth: { configurable: true, value: clientWidth },
    scrollWidth: { configurable: true, value: 4000 },
  });
  const calls: { left: number; behavior: string }[] = [];
  (el as any).scrollBy = (arg: { left: number; behavior: string }) => { calls.push(arg); el.scrollLeft += arg.left; };
  // surfaces visible arrows
  el.scrollLeft = 0;
  return calls;
}

let mounts = 0;
async function mount(scrollStep?: number, reducedMotion = false) {
  (win as any).matchMedia = (q: string) => ({ matches: reducedMotion && q.includes("reduce") });
  await React.act(async () => {
    root.render(React.createElement(HorizontalStrip, {
      key: `s${++mounts}`,
      label: "Posts planned",
      scrollStep,
      children: ["a", "b", "c", "d", "e", "f"].map((k) => React.createElement("div", { key: k }, k)),
    }));
  });
  await flush();
  const el = host.querySelector(".wa-strip") as HTMLElement;
  assert(el, "strip rendered");
  const calls = instrument(el);
  // expose the "forward" arrow by forcing overflow state
  await React.act(async () => { el.dispatchEvent(new win.Event("scroll")); });
  await flush();
  const after = Array.from(host.querySelectorAll("button")).find((b) => b.getAttribute("aria-label")?.includes("Scroll forward"));
  const before = Array.from(host.querySelectorAll("button")).find((b) => b.getAttribute("aria-label")?.includes("Scroll back"));
  return { calls, after, before, el };
}
const click = (b: Element | undefined | null) =>
  React.act(async () => { (b as HTMLElement)?.dispatchEvent(new win.MouseEvent("click", { bubbles: true })); });

async function main() {
  // 1. Default (no scrollStep): viewport step = max(240, 1000*0.8)=800.
  let m = await mount(undefined);
  await click(m.after);
  assert(m.calls[0]?.left === 800, `default step 800 (80% of 1000), got ${m.calls[0]?.left}`);
  assert(m.calls[0].behavior === "smooth", "default smooth when not reduced-motion");

  // 2. Explicit scrollStep=248 → forward +248, backward -248.
  m = await mount(248);
  await click(m.after);
  assert(m.calls[0]?.left === 248, `explicit step 248 forward, got ${m.calls[0]?.left}`);
  m.el.scrollLeft = 248; await React.act(async () => { m.el.dispatchEvent(new win.Event("scroll")); }); await flush();
  const before = Array.from(host.querySelectorAll("button")).find((b) => b.getAttribute("aria-label")?.includes("Scroll back"));
  await click(before);
  assert(m.calls[m.calls.length - 1]?.left === -248, `explicit step -248 backward, got ${m.calls[m.calls.length - 1]?.left}`);

  // 3. Reduced-motion preserved: explicit step still 248 but behavior "auto".
  m = await mount(248, true);
  await click(m.after);
  assert(m.calls[0]?.left === 248 && m.calls[0].behavior === "auto", `reduced-motion step 248 auto, got ${JSON.stringify(m.calls[0])}`);

  console.log("horizontalStrip scrollStep QA: PASS");
}
void main().then(async () => { await React.act(async () => root.unmount()); });
