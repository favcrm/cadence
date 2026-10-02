export {};

declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const key of ["window", "document", "navigator", "HTMLElement", "Node", "Event", "KeyboardEvent", "location", "localStorage"]) {
  Object.defineProperty(globalThis, key, { value: key === "window" ? win : win[key], configurable: true });
}
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const calls: string[] = [];
Object.defineProperty(globalThis, "fetch", {
  configurable: true,
  value: async (url: string) => {
    calls.push(String(url));
    return new Response("{}", { status: 200, headers: { "Content-Type": "application/json" } });
  },
});
const moduleLoader = require("module");
const originalRequire = moduleLoader.prototype.require;
moduleLoader.prototype.require = function (this: unknown, id: string) {
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const { createElement, act } = require("react");
const { createRoot } = require("react-dom/client");
const { default: AccountMenu } = require("../src/ui/AccountMenu") as typeof import("../src/ui/AccountMenu");
const { default: VersionLine } = require("../src/ui/VersionLine") as typeof import("../src/ui/VersionLine");
const { releaseLabel, runningRelease } = require("../src/lib/fmt") as typeof import("../src/lib/fmt");
const { routePath } = require("../src/lib/router") as typeof import("../src/lib/router");

function assert(condition: unknown, message: string): void {
  if (!condition) throw new Error(message);
}
function mount(node: unknown) {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  act(() => root.render(node));
  return { host, done: () => { act(() => root.unmount()); host.remove(); } };
}
const href = routePath({ screen: "settings", section: "update" });
const line = (props: object) => createElement(VersionLine, { release: null, build: `0.1.0+${sha}`, href, updatePending: false, ...props });
const sha = "c78b8a0535e0ddbc58b77d9df4a757261413c842";

// releaseLabel is the Settings › Update formatter, moved unchanged.
assert(releaseLabel(`1.2.3+${sha}`, null) === "1.2.3+c78b8a0", "release shortens the build");
assert(releaseLabel(null, sha) === "c78b8a0", "sha-only falls back to short sha");
assert(releaseLabel(null, null) === "Not installed", "nothing known");
// Meta feeds it with no extra fetch.
assert(runningRelease({ version: "1.2.3", build_commit: sha }) === "1.2.3+c78b8a0", "meta release");
assert(runningRelease({ version: "1.2.3", build_commit: "unknown" }) === "1.2.3", "unknown commit keeps version");
assert(runningRelease({ version: `1.2.3+${sha}`, build_commit: sha }) === "1.2.3+c78b8a0", "no double append");
assert(runningRelease({ version: "", build_commit: sha }) === null, "no version -> fallback");
assert(runningRelease(null) === null, "no meta -> fallback");

// Release version.
{
  const { host, done } = mount(line({ release: "1.2.3+c78b8a0" }));
  const a = host.querySelector("[data-version-line]") as HTMLAnchorElement;
  assert(a.textContent === "v1.2.3+c78b8a0", `release text: ${a.textContent}`);
  assert(a.getAttribute("href") === "/settings/update", `links to Settings > Update: ${a.getAttribute("href")}`);
  assert(a.className.includes("num") && a.className.includes("text-micro") && a.className.includes("text-ink-500"), "quiet style");
  assert(!a.textContent?.includes("What's new"), "no What's new without an update");
  done();
}
// UI_BUILD fallback.
{
  const { host, done } = mount(line({}));
  assert(host.textContent === "build 0.1.0+c78b8a0", `fallback text: ${host.textContent}`);
  done();
}
// "unknown" (no build baked in) stays readable.
{
  const { host, done } = mount(line({ build: "unknown" }));
  assert(host.textContent === "build unknown", `unknown text: ${host.textContent}`);
  done();
}
// What's new only while pending.
{
  const { host, done } = mount(line({ release: "1.2.3", updatePending: true }));
  assert(host.textContent === "v1.2.3What's new", `pending text: ${host.textContent}`);
  done();
}
// Inside the real AccountMenu: clicking the version line closes the dialog, with no close code in VersionLine.
{
  const user = { name: "Fable Chen", email: "f@example.com", role: "operator", handle: "f", sub: "s" };
  const meta = { read_only: false, signed_in: true, actor: "x", session: { id: "s1", origin: "public", user } };
  const footer = createElement(VersionLine, { release: "1.2.3", build: "abc", href, updatePending: false });
  const { host, done } = mount(createElement(AccountMenu, { meta, actor: "x", mayWrite: true, onChange: () => undefined, trigger: "avatar", placement: "below-end", footer }));
  act(() => (host.querySelector("button[aria-haspopup=dialog]") as HTMLButtonElement).click());
  assert(host.querySelector("[role=dialog] [data-version-line]"), "version line is inside the open dialog");
  act(() => (host.querySelector("[data-version-line]") as HTMLElement).click());
  assert(!host.querySelector("[role=dialog]"), "clicking the version line closes the dialog");
  done();
}
console.log("versionLine tests passed");
