export {};
/** Update reads/actions exercise the real component and shared Button. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/settings/update" });
for (const name of [
  "window",
  "document",
  "Node",
  "Element",
  "HTMLElement",
  "SVGElement",
  "navigator",
  "MutationObserver",
  "Event",
  "MouseEvent",
  "location",
  "history",
])
  Object.defineProperty(globalThis, name, {
    value: name === "window" ? win : win[name],
    configurable: true,
    writable: true,
  });
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, {
    value: win[name].bind(win),
    configurable: true,
  });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module"),
  originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons")
    return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } =
  require("react-dom/client") as typeof import("react-dom/client");
const Update = (
  require("../src/features/settings/Update") as typeof import("../src/features/settings/Update")
).default;
type Status = import("../src/lib/types").UpdateStatus;
const sample = (): Status => ({
  current: { version: "0.1.0", sha: "old" },
  check: {
    current: "old",
    current_version: "0.1.0",
    target: "new",
    target_version: "0.2.0",
    up_to_date: false,
    changes: ["Clearer navigation"],
    change_count: 1,
    schema: { current: 1, target: 1, migration: false },
    lease: null,
    waiters: [],
    blockers: [],
  },
  checked_at: 1700000000,
  checking: false,
  update_available: true,
  change_count: 1,
  changes: [],
  migration: false,
  blockers: [],
  running: false,
  lines: [],
  result: null,
  error: null,
  pending: null,
  waiting: [],
});
const json = (data: unknown, code = 200) =>
  new Response(JSON.stringify(data), {
    status: code,
    headers: { "Content-Type": "application/json" },
  });
const gets: { resolve: (r: Response) => void }[] = [];
const posts: { path: string; resolve: (r: Response) => void }[] = [];
let hold = false,
  fail = false,
  status = sample();
globalThis.fetch = async (input, init) => {
  const path = new URL(String(input), "http://localhost").pathname;
  if (init?.method === "POST")
    return new Promise<Response>((resolve) => posts.push({ path, resolve }));
  if (path !== "/api/update") throw new Error(`Unexpected read ${path}`);
  if (hold) return new Promise<Response>((resolve) => gets.push({ resolve }));
  return fail
    ? json({ error: "Board temporarily unavailable" }, 503)
    : json(status);
};
const timers = new Map<number, { fn: () => void; ms: number }>();
let timerId = 0;
win.setInterval = (fn: () => void, ms: number) => {
  timers.set(++timerId, { fn, ms });
  return timerId;
};
win.clearInterval = (id: number) => timers.delete(id);
const host = document.createElement("div");
document.body.append(host);
let root = createRoot(host);
const assert: (v: unknown, why: string) => asserts v = (v, why) => {
  if (!v) throw new Error(why);
};
const flush = () =>
  React.act(async () => {
    await new Promise((r) => setTimeout(r, 0));
  });
const button = (label: string) =>
  Array.from(host.querySelectorAll("button")).find(
    (b) => b.textContent?.trim() === label,
  );
const click = async (el: Element | undefined) => {
  assert(el, "click target exists");
  await React.act(async () =>
    el.dispatchEvent(new MouseEvent("click", { bubbles: true })),
  );
  await flush();
};
const render = async (readOnly = false, operator = true) => {
  await React.act(async () =>
    root.render(
      React.createElement(Update, { viewer: { readOnly, operator } }),
    ),
  );
  await flush();
};
const fresh = async (s = sample(), readOnly = false, operator = true) => {
  await React.act(async () => root.unmount());
  root = createRoot(host);
  status = s;
  fail = false;
  hold = false;
  gets.length = posts.length = 0;
  await render(readOnly, operator);
};
const text = () => host.textContent ?? "";
async function main() {
  hold = true;
  await render();
  assert(
    !text().includes("no cadence link installed"),
    "An unresolved read must not claim there is no installation",
  );
  assert(text().includes("Loading"), "Initial read communicates loading");
  gets[0].resolve(json(status));
  await flush();
  await fresh(sample(), true, true);
  assert(
    button("Check for updates")?.disabled,
    "Read-only operator cannot trigger a POST check",
  );
  assert(
    button("Update")?.disabled,
    "Read-only operator cannot trigger an update",
  );
  await click(button("Check for updates"));
  await click(button("Update"));
  assert(posts.length === 0, "Read-only controls issue no writes");
  await fresh(sample(), false, false);
  assert(
    button("Check for updates")?.disabled && button("Update")?.disabled,
    "Agent viewer cannot write",
  );
  await React.act(async () => root.unmount());
  root = createRoot(host);
  fail = true;
  hold = false;
  await render();
  assert(
    text().includes("Update status unavailable") && !text().includes("Loading"),
    "A failed initial read stops loading and remains retryable",
  );
  fail = false;
  await click(button("Retry status"));
  assert(
    text().includes("Update available"),
    "Initial failure can recover with a read-only retry",
  );
  await fresh();
  fail = true;
  await click(button("Refresh status"));
  assert(
    text().includes("Last known") &&
      text().includes("Board temporarily unavailable"),
    "Failed refresh retains visibly stale release data",
  );
  assert(button("Update")?.disabled, "Stale status cannot start an update");
  fail = false;
  await click(button("Retry status"));
  assert(
    !text().includes("Last known") && !button("Update")?.disabled,
    "Successful read retry restores fresh actions",
  );
  hold = true;
  await click(button("Refresh status"));
  await click(button("Refresh status"));
  gets[1].resolve(
    json({ ...sample(), current: { version: "newer read", sha: "2" } }),
  );
  await flush();
  gets[0].resolve(
    json({ ...sample(), current: { version: "older read", sha: "1" } }),
  );
  await flush();
  assert(
    text().includes("newer read") && !text().includes("older read"),
    "Older overlapping reads cannot overwrite a newer observation",
  );
  await fresh();
  hold = true;
  await click(button("Refresh status"));
  await click(button("Update"));
  posts[0].resolve(json({ started: true }));
  await flush();
  gets[1].resolve(json({ ...sample(), running: true }));
  await flush();
  gets[0].resolve(json(sample()));
  await flush();
  assert(
    text().includes("Update in progress") && button("Updating…")?.disabled,
    "A read begun before starting cannot replace observed run status",
  );
  await fresh();
  const updating = button("Update")!;
  await React.act(async () => {
    updating.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    updating.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  assert(
    Number(posts.length) === 1 && posts[0].path === "/api/update",
    "Rapid repeated clicks start only one request",
  );
  assert(
    text().includes("Starting"),
    "Start request is visible before it is acknowledged",
  );
  posts[0].resolve(json({ started: true }));
  await flush();
  assert(
    text().includes("Update requested") && button("Update requested")?.disabled,
    "Acknowledgement without run evidence is pending, not finished",
  );
  assert(
    [...timers.values()].some((t) => t.ms === 2000),
    "Acknowledged request polls promptly",
  );
  status = { ...sample(), running: true, lines: ["Preparing release"] };
  await React.act(async () => [...timers.values()][0].fn());
  await flush();
  assert(
    text().includes("Update in progress"),
    "Observed run replaces local acknowledgement",
  );
  status = {
    ...sample(),
    error: "health probe failed",
    lines: ["health failed"],
  };
  await React.act(async () => [...timers.values()][0].fn());
  await flush();
  assert(
    text().includes("Update failed") && !text().includes("Update complete"),
    "Failure does not announce success",
  );
  await fresh();
  await click(button("Check for updates"));
  assert(
    button("Checking…")?.disabled && button("Update")?.disabled,
    "A manual check excludes a concurrent start",
  );
  posts[0].resolve(json({ error: "Another update is running" }, 409));
  await flush();
  assert(
    text().includes("Request failed") &&
      text().includes("Another update is running") &&
      !button("Check for updates")?.disabled,
    "Server rejection is visible and releases the local action lock",
  );
  await click(button("Update"));
  posts[1].resolve(json({ started: false }));
  await flush();
  assert(
    text().includes("did not acknowledge") &&
      !text().includes("Update requested"),
    "A false start receipt never becomes a pending or successful run",
  );
  const pending = {
    ...sample(),
    pending: {
      phase: "draining",
      target: "new",
      from: "old",
      by: "operator",
      since: 1700000000,
    },
    waiting: [
      { alias: "dev-1", message: "job", state: "running", age_secs: 120 },
    ],
  };
  await fresh(pending);
  assert(
    button("Updating…")?.disabled && button("Check for updates")?.disabled,
    "Another process's pending marker disables duplicate writes",
  );
  assert(text().includes("dev-1 (2m)"), "Drain shows active agent turns");
  const advisory = sample();
  advisory.check!.blockers = ["waiting for 1 turn (the drain waits)"];
  await fresh(advisory);
  assert(
    !button("Update")?.disabled && text().includes("waiting for 1 turn"),
    "Advisory drain warnings do not incorrectly forbid an update",
  );
  const checking = { ...sample(), checking: true };
  await fresh(checking);
  assert(
    button("Checking…")?.disabled && button("Update")?.disabled,
    "Background check is an explicit busy state",
  );
  assert(
    [...timers.values()].some((t) => t.ms === 2000),
    "Background checking polls promptly",
  );
  await fresh({
    ...sample(),
    result: { rolled_back: true, health: { ok: false } },
  });
  assert(
    text().includes("Rolled back") && !text().includes("Update complete"),
    "Rollback is never success",
  );
  await fresh({
    ...sample(),
    result: { rolled_back: false, health: { ok: true } },
  });
  assert(
    text().includes("Update complete"),
    "Health-confirmed result announces success",
  );
  await fresh({
    ...sample(),
    result: {
      rolled_back: false,
      health: null,
      install: null,
      restart: null,
      check: { up_to_date: true },
    },
  });
  assert(
    text().includes("No update needed") && !text().includes("Update complete"),
    "No-op result is distinct from a completed install",
  );
  await fresh({ ...sample(), result: { rolled_back: false } });
  assert(
    text().includes("Run ended") && !text().includes("Update complete"),
    "Unknown report cannot imply a healthy update",
  );
  await fresh({
    ...sample(),
    current: null as unknown as Status["current"],
    check: null,
  });
  assert(
    text().includes("Installation unavailable"),
    "Null current observation is distinct from a confirmed missing installation",
  );
  await fresh({ ...sample(), lines: ["line one"] });
  const log = host.querySelector("pre")!;
  const disclosure = log.closest("details")!;
  await React.act(async () => {
    disclosure.open = true;
    disclosure.dispatchEvent(new Event("toggle"));
  });
  await flush();
  assert(
    disclosure.open,
    "Scroll-following is exercised with the visible log open",
  );
  assert(
    log && log.getAttribute("tabindex") === "0",
    "Progress log is keyboard reachable",
  );
  Object.defineProperty(log, "scrollHeight", {
    value: 1000,
    configurable: true,
  });
  Object.defineProperty(log, "clientHeight", {
    value: 200,
    configurable: true,
  });
  log.scrollTop = 200;
  await React.act(async () =>
    log.dispatchEvent(new Event("scroll", { bubbles: true })),
  );
  status = { ...sample(), lines: ["line one", "line two"] };
  await click(button("Refresh status"));
  assert(
    log.scrollTop === 200,
    "New lines do not pull the reader away from earlier progress",
  );
  await click(button("Jump to latest"));
  assert(
    Number(log.scrollTop) === 1000,
    "Explicit latest control resumes following",
  );
  hold = true;
  await click(button("Refresh status"));
  const last = gets.at(-1)!;
  await React.act(async () => root.unmount());
  assert(timers.size === 0, "Unmount clears polling");
  root = createRoot(host);
  hold = false;
  status = { ...sample(), current: { version: "fresh mount", sha: "3" } };
  await render();
  last.resolve(
    json({ ...sample(), current: { version: "unmounted read", sha: "4" } }),
  );
  await flush();
  assert(
    text().includes("fresh mount") && !text().includes("unmounted read"),
    "Late unmounted response cannot affect a new instance",
  );
  await React.act(async () => root.unmount());
  console.log("updateWorkspace: PASS");
}
void main().catch((e: unknown) => {
  console.error(e);
  (globalThis as any).process.exitCode = 1;
});
