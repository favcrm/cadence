export {};
/**
 * CAD-1692: keyboard focus follows a project pin toggle. Pinning moves
 * the row to the top, unpinning moves it back or hides it behind
 * "Show N more"; focus stays on that row's pin button, or lands on the
 * list toggle when the row leaves the list. Mounts the real ProjectList.
 */
declare function require(name: string): any;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLButtonElement", "SVGElement", "navigator", "MutationObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "localStorage", "CSS"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const { ProjectList } = require("../src/ui/NavList") as typeof import("../src/ui/NavList");
const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof import("../screen-ui/src/navigation");
const { hostNavigation } = require("../src/ui/hostNavigation") as typeof import("../src/ui/hostNavigation");

const projects = ["alpha", "beta", "gamma"].map((key) => ({ key, prefix: key.slice(0, 3).toUpperCase() }));
// Fixture projects carry only the fields the sidebar reads.
const props = (project: string | null) => ({
  project, projectHref: (k: string) => `/p/${k}`, projects, issues: { data: [], status: "ready", error: null },
}) as unknown as React.ComponentProps<typeof ProjectList>;

win.localStorage.clear();
const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const render = (project: string | null) => React.act(async () => {
  root.render(React.createElement(LocaleProvider, null, React.createElement(NavigationProvider, { adapter: hostNavigation, children:
    React.createElement(ProjectList, props(project)) })));
});
// By accessible name, the contract a keyboard or screen-reader user sees.
const pin = (key: string) => Array.from(host.querySelectorAll<HTMLButtonElement>("button.projpin"))
  .find((b) => / (\S+)$/.exec(b.getAttribute("aria-label") ?? "")?.[1] === key) ?? null;
const press = async (button: HTMLButtonElement) => {
  button.focus();
  await React.act(async () => { button.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
};

await render("beta");
assert(pin("gamma") !== null, `ProjectList mounted with pin buttons (got ${host.innerHTML.slice(0, 300)})`);

// Pin gamma: it jumps to the top; focus stays on its (now Unpin) button.
const gamma = pin("gamma")!;
await press(gamma);
assert(pin("gamma") !== null && pin("gamma")!.getAttribute("aria-pressed") === "true", "gamma is pinned");
assert(document.activeElement === pin("gamma"), `pinning keeps focus on the row's pin (focus on ${document.activeElement?.tagName})`);
assert(pin("gamma") === gamma, "the row moved, it did not remount");

// Pin beta (the project on screen), then unpin it: it stays visible, focus stays.
await press(pin("beta")!);
assert(document.activeElement === pin("beta"), "pinning a second project keeps focus");
await press(pin("beta")!);
assert(pin("beta") !== null && pin("beta")!.getAttribute("aria-pressed") === "false", "beta unpinned and still listed (it is on screen)");
assert(document.activeElement === pin("beta"), "unpinning a row that stays keeps focus on it");

// Unpin the last pin: the list returns to every project and gamma stays.
await press(pin("gamma")!);
assert(document.activeElement === pin("gamma"), "unpinning the last pin keeps focus on that row");

// Pinned-only with alpha and gamma pinned (beta on screen): unpinning gamma
// hides it behind Show N more, so focus lands on that toggle.
const { updateSidebarPrefs, DEFAULT_SIDEBAR_PREFS } = require("../src/features/app-shell/sidebarPrefs") as typeof import("../src/features/app-shell/sidebarPrefs");
await React.act(async () => { updateSidebarPrefs(() => ({ ...DEFAULT_SIDEBAR_PREFS, pinned: ["alpha", "gamma"] })); });
assert(pin("gamma")?.getAttribute("aria-pressed") === "true" && pin("beta") !== null && host.querySelector("button.projmore") === null, "setup: gamma pinned, beta shown (on screen), nothing hidden");
await press(pin("gamma")!);
assert(pin("gamma") === null, "an unpinned project off screen is hidden while pinned-only");
assert(host.querySelector("button.projmore")?.textContent === "Show 1 more", "gamma waits behind Show 1 more");
assert(document.activeElement?.classList.contains("projmore"), `focus lands on Show N more (focus on ${document.activeElement?.className || document.activeElement?.tagName})`);

// A mouse click (no keyboard focus on the button) never moves focus.
(document.activeElement as HTMLElement | null)?.blur();
await React.act(async () => { pin("beta")!.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
assert(document.activeElement === document.body, "a pointer toggle does not grab focus");

await React.act(async () => { root.unmount(); });
console.log("sidebar pin focus checks passed");
}
void main();
