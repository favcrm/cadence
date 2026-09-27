export {};
/** Real Setup and shared stores: loading, readiness, project refresh and copy feedback. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/setup" });
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
const Setup = (
  require("../src/features/setup/Setup") as typeof import("../src/features/setup/Setup")
).default;
const { setupResource } =
  require("../src/features/setup/setupApi") as typeof import("../src/features/setup/setupApi");
const { resources } =
  require("../src/lib/resources") as typeof import("../src/lib/resources");
type Report = import("../src/features/setup/checks").SetupReport;
type Check = import("../src/features/setup/checks").SetupCheck;
const check = (
  name: string,
  group: Check["group"],
  status: Check["status"] = "ok",
  fix: string | null = null,
): Check => ({
  check: name,
  group,
  status,
  detail: `${name} observation`,
  fix,
});
const ready = (): Report => ({
  checks: [
    check("state_dir", "environment"),
    check("tracker", "environment"),
    check("daemon", "environment"),
    check("ui", "environment"),
    check("claude", "provider"),
    check("master", "master"),
    check("master_login", "master"),
  ],
  master: { providers: [{ bin: "claude", ready: true, start: null }] },
  checked_at: 1700000000000,
  detect_only: true,
  ran_now: true,
  age_ms: 0,
  recheck_in_ms: 0,
});
let report = ready(),
  projects: { key: string }[] = [],
  hold = false,
  fail = false,
  projectFail = false,
  denied = false,
  setupReads = 0,
  projectReads = 0;
const pending: { path: string; resolve: (response: Response) => void }[] = [];
const json = (data: unknown, status = 200) =>
  new Response(JSON.stringify(data), {
    status,
    headers: { "Content-Type": "application/json" },
  });
globalThis.fetch = async (input) => {
  const path = new URL(String(input), "http://localhost").pathname;
  if (path === "/api/setup") setupReads++;
  else if (path === "/api/projects") projectReads++;
  else throw new Error(`Unexpected Setup request: ${path}`);
  if (hold)
    return new Promise<Response>((resolve) => pending.push({ path, resolve }));
  if (path === "/api/setup")
    return denied
      ? json({ error: "host only" }, 403)
      : fail
        ? json({ error: "Sample probe failure" }, 503)
        : json(report);
  return projectFail
    ? json({ error: "Sample project failure" }, 503)
    : json({ projects });
};
const host = document.createElement("div");
document.body.append(host);
let root = createRoot(host);
const assert: (value: unknown, why: string) => asserts value = (value, why) => {
  if (!value) throw new Error(why);
};
const flush = async () => {
  for (let i = 0; i < 2; i++)
    await React.act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
};
const text = () => host.textContent ?? "";
const button = (match: RegExp) =>
  Array.from(host.querySelectorAll("button")).find((b) =>
    match.test(b.textContent?.trim() ?? ""),
  );
const click = async (control: Element | undefined | null) => {
  assert(control, "Click target exists");
  await React.act(async () =>
    control.dispatchEvent(new MouseEvent("click", { bubbles: true })),
  );
  await flush();
};
const render = async (readOnly: boolean | null = false) => {
  await React.act(async () =>
    root.render(
      React.createElement(Setup, {
        readOnly,
        settingsHref: "/settings/models",
      }),
    ),
  );
  await flush();
};
const seed = async () => {
  await React.act(async () => {
    setupResource.write(() => report);
    resources.projects.write(
      () => projects as import("../src/lib/types").Project[],
    );
  });
};
const fresh = async () => {
  await React.act(async () => root.unmount());
  root = createRoot(host);
  await seed();
  await render();
};
const selection = () =>
  host.querySelector('nav button[aria-current="step"]')?.textContent ?? "";
const chosen = require("process").argv[2] ?? "all";
async function run() {
  if (chosen === "all" || chosen === "loading") {
    await render(null);
    assert(setupReads === 0, "Unresolved access makes no setup read");
    await render(true);
    assert(
      setupReads === 0 && text().includes("host"),
      "Read-only view stays on host instructions without probes",
    );
    hold = true;
    await render();
    const legacyHome = button(/Go to Home/);
    if (legacyHome) await click(legacyHome);
    assert(
      !text().includes("Every required check passes"),
      "Loading must not claim required checks pass when Home is selected",
    );
    assert(
      !text().includes("Required checks passed"),
      "Loading does not claim readiness",
    );
    hold = false;
    await React.act(async () =>
      pending
        .splice(0)
        .forEach((p) =>
          p.resolve(json(p.path === "/api/setup" ? report : { projects })),
        ),
    );
    await flush();
  }
  if (chosen === "all" || chosen === "empty") {
    report = { ...ready(), checks: [], master: { providers: [] } };
    projects = [{ key: "site" }];
    await fresh();
    assert(
      !host.querySelector("nav")?.textContent?.includes("✓"),
      "Empty check groups must not get completed step markers",
    );
    assert(
      !text().includes("Required checks passed"),
      "Empty report is not ready",
    );
    report = ready();
    report.checks = report.checks.filter((c) => c.check !== "claude");
    report.checks.push(check("codex", "provider"));
    report.master = undefined;
    await fresh();
    const agents = button(/Agent CLIs/);
    assert(
      agents &&
        !agents.textContent?.includes("✓") &&
        !/\bReady\b/.test(agents.textContent ?? ""),
      "Unsupported ready CLI does not complete the master-CLI requirement",
    );
  }
  if (chosen === "all" || chosen === "projects") {
    report = ready();
    projects = [];
    await fresh();
    await click(button(/First project|Projects/));
    assert(
      /No project|first project/i.test(text()),
      "Empty project list is explicit",
    );
    const before = projectReads;
    projects = [{ key: "new-site" }];
    await click(button(/^Check again|^re-check/));
    assert(
      projectReads > before && text().includes("new-site"),
      "Re-check refreshes and displays newly registered projects",
    );
  }
  if (chosen === "all" || chosen === "clipboard") {
    report = ready();
    report.checks.find((c) => c.check === "tracker")!.status = "missing";
    report.checks.find((c) => c.check === "tracker")!.fix = "cadence setup";
    projects = [{ key: "site" }];
    await fresh();
    await click(button(/Environment/));
    Object.defineProperty(navigator, "clipboard", {
      value: {
        writeText: async () => {
          throw new Error("Clipboard unavailable");
        },
      },
      configurable: true,
    });
    await click(
      Array.from(host.querySelectorAll("button")).find((b) =>
        /copy/i.test(b.getAttribute("aria-label") ?? ""),
      ),
    );
    assert(
      /select and copy the command/i.test(text()),
      "Clipboard failure gives a selectable fallback",
    );
    Object.defineProperty(navigator, "clipboard", {
      value: undefined,
      configurable: true,
    });
    await click(
      Array.from(host.querySelectorAll("button")).find((b) =>
        /copy/i.test(b.getAttribute("aria-label") ?? ""),
      ),
    );
    assert(
      /select and copy the command/i.test(text()),
      "Missing clipboard API gives a selectable fallback",
    );
    let copied = "";
    Object.defineProperty(navigator, "clipboard", {
      value: {
        writeText: async (value: string) => {
          copied = value;
        },
      },
      configurable: true,
    });
    await click(
      Array.from(host.querySelectorAll("button")).find((b) =>
        /copy/i.test(b.getAttribute("aria-label") ?? ""),
      ),
    );
    assert(
      copied === "cadence setup" && /Copied/.test(text()),
      "Copy sends the exact command and reports success",
    );
  }
  if (chosen === "all") {
    report = ready();
    report.checks.find((c) => c.check === "master")!.status = "missing";
    report.master!.providers[0].start =
      "cadence master start --provider claude --unconfined";
    report.master!.providers[0].warning = "No filesystem sandbox";
    projects = [{ key: "site" }];
    await fresh();
    assert(
      selection().includes("Master agent"),
      "Initial ready groups lead to the first actionable section",
    );
    assert(
      text().includes("--unconfined") &&
        text().includes("No filesystem sandbox"),
      "Provider-qualified command and warning remain visible",
    );
    await click(button(/Environment/));
    report = ready();
    await click(button(/^Check again/));
    assert(
      selection().includes("Environment"),
      "Re-check keeps the chosen section stable",
    );
    assert(
      text().includes("Required checks passed"),
      "Fresh required checks and registered project show readiness",
    );
    fail = true;
    await click(button(/^Check again/));
    assert(
      text().includes("Sample probe failure") &&
        !text().includes("Required checks passed"),
      "Failed re-check keeps observations without current readiness claims",
    );
    fail = false;
    projectFail = true;
    await click(button(/^Check again/));
    await click(button(/Projects/));
    assert(
      text().includes("Sample project failure") &&
        !text().includes("Required checks passed"),
      "Project failure is distinct from no projects or overall readiness",
    );
    projectFail = false;
    report = { ...ready(), ran_now: false, age_ms: 2200, recheck_in_ms: 2800 };
    await click(button(/^Check again/));
    assert(
      text().includes("Recent report reused (3s old)") &&
        text().includes("3s cooldown"),
      "A reused report carries the host's cache age and cooldown",
    );
    hold = true;
    const before = setupReads;
    await click(button(/^Check again/));
    await click(button(/^Check again/));
    assert(
      setupReads === before + 1 && !text().includes("Required checks passed"),
      "Busy re-check cannot duplicate probes or claim readiness",
    );
    hold = false;
    await React.act(async () =>
      pending
        .splice(0)
        .forEach((p) =>
          p.resolve(json(p.path === "/api/setup" ? report : { projects })),
        ),
    );
    await flush();
    denied = true;
    await click(button(/^Check again/));
    assert(
      text().includes("Continue on the host") &&
        !host.querySelector("nav") &&
        !text().includes("state_dir observation"),
      "HTTP host-only refusal hides cached checks and refresh controls",
    );
  }
  await React.act(async () => root.unmount());
  await win.happyDOM.close();
  console.log(`setupWorkspace ${chosen}: passed`);
}
run().catch(async (error) => {
  await React.act(async () => root.unmount());
  await win.happyDOM.close();
  console.error(error);
  throw error;
});
