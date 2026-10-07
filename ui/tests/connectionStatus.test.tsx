declare function require(name: string): any;
export {};
function ok(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "SVGElement", "navigator"])
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  const loader = require("module"), originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, arguments);
  };
  const React = require("react") as typeof import("react");
  const { act } = require("react") as typeof import("react");
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const StatusChips = require("../src/ui/StatusChips").default;

  const mount = document.createElement("div");
  document.body.appendChild(mount);
  const root = createRoot(mount);
  const health = (daemon: string) => ({ ok: true, pm_present: true, projects: 1, issues: 1, daemon, embedded: false });
  const text = () => mount.textContent ?? "";
  const aria = () =>
    Array.from(mount.querySelectorAll("[aria-label]")).map((n) => n.getAttribute("aria-label")).join(" | ");

  await act(async () => {
    root.render(React.createElement(StatusChips, { readOnly: false, health: null, onRefresh: () => {}, variant: "header" }));
  });
  ok(aria().includes("Checking connection"), `pending header probe still checks: ${aria()}`);

  await act(async () => {
    root.render(React.createElement(StatusChips, { readOnly: false, health: "unavailable", onRefresh: () => {}, variant: "header" }));
  });
  ok(aria().includes("Connection unavailable"), `failed header probe is unavailable: ${aria()}`);
  ok(!aria().includes("Checking connection"), `failed probe is not "checking": ${aria()}`);

  await act(async () => {
    root.render(React.createElement(StatusChips, { readOnly: false, health: health("reachable"), onRefresh: () => {}, variant: "header" }));
  });
  ok(aria().includes("Daemon connected"), `reachable probe connects: ${aria()}`);
  await act(async () => {
    root.render(React.createElement(StatusChips, { readOnly: false, health: health("unreachable"), onRefresh: () => {}, variant: "header" }));
  });
  ok(aria().includes("Daemon disconnected"), `unreachable daemon disconnects: ${aria()}`);
  ok(!aria().includes("live activity"), `daemon probe does not judge the stream: ${aria()}`);
  ok(!aria().includes("unavailable"), `answered probe is not "unavailable": ${aria()}`);

  await act(async () => {
    root.render(React.createElement(StatusChips, { readOnly: false, health: null, onRefresh: () => {}, variant: "menu" }));
  });
  ok(!text().includes("Checking connection"), `pending menu probe stays quiet: ${text()}`);
  await act(async () => {
    root.render(React.createElement(StatusChips, { readOnly: false, health: "unavailable", onRefresh: () => {}, variant: "menu" }));
  });
  ok(text().includes("Connection unavailable"), `failed menu probe reads unavailable: ${text()}`);
  ok(text().includes("Refresh board"), `failed menu probe keeps the retry: ${text()}`);
  await act(async () => {
    root.render(React.createElement(StatusChips, { readOnly: false, health: health("reachable"), onRefresh: () => {}, variant: "menu" }));
  });
  ok(text().includes("Daemon connected"), `reachable menu probe connects: ${text()}`);

  await act(async () => {
    root.unmount();
  });
  console.log("connection status checks passed");
}

main().catch((e) => {
  setTimeout(() => {
    throw e;
  });
});
