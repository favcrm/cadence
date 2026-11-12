/** CAD-2255: archived projects are discoverable only through a proved capability. */
import type * as HappyDom from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDomClient from "react-dom/client";
import type * as Locale from "../src/lib/locale";
import type * as Navigation from "../screen-ui/src/navigation";
import type * as HostNavigation from "../src/ui/hostNavigation";
import type * as ProjectsOverviewModule from "../src/features/projects/ProjectsOverview";
import type { Project } from "../src/lib/types";

declare function require(name: string): unknown;

function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}
let act: typeof ReactModule.act;
async function settle(check: () => void) {
  for (let n = 0; n < 200; n++) {
    try {
      check();
      return;
    } catch (error) {
      if (n === 199) throw error;
      await act(async () => {
        await Promise.resolve();
      });
    }
  }
}

async function main() {
  const { Window } = require("happy-dom") as typeof HappyDom;
  const win = new Window({ url: "http://localhost/projects" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage", "localStorage"] as const)
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener"] as const)
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  Object.defineProperty(globalThis, "requestAnimationFrame", { value: (fn: FrameRequestCallback) => { fn(0); return 0; }, configurable: true });
  const loader = require("module") as { prototype: { require: (this: unknown, id: string) => unknown } };
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string): unknown {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.call(this, id);
  };
  const React = require("react") as typeof ReactModule;
  act = React.act;
  const { createRoot } = require("react-dom/client") as typeof ReactDomClient;
  const { LocaleProvider } = require("../src/lib/locale") as typeof Locale;
  const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof Navigation;
  const { hostNavigation } = require("../src/ui/hostNavigation") as typeof HostNavigation;
  const ProjectsOverview = (require("../src/features/projects/ProjectsOverview") as typeof ProjectsOverviewModule).default;

  const json = (value: unknown, status = 200) =>
    new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });

  const active: Project = {
    key: "sample",
    prefix: "SMP",
    components: ["web"],
    repos: [],
    issues: 3,
  };
  const archivedRow = {
    key: "legacy-app",
    prefix: "LEG",
    components: ["web"],
    repos: [{ path: "legacy" }],
    issues: 42,
    archived: true,
  };
  let archivedAnswer: { body: unknown; status?: number } = {
    body: { archive_supported: true, projects: [archivedRow] },
  };
  let lifecycleAnswer: { body: unknown; status?: number } = {
    body: {
      archive_capability: "cad2255.project-archive.v1",
      project: "legacy-app",
      archived: true,
      revision: 2,
      archived_at: "2026-05-05T00:00:00Z",
      archived_by: "owner@example.test",
      changed_at: "2026-05-05T00:00:00Z",
      changed_by: "owner@example.test",
      blockers: [],
    },
  };

  globalThis.fetch = (async (input: unknown) => {
    const url = new URL(typeof input === "string" ? input : String((input as Request).url), "http://localhost");
    if (url.pathname === "/api/projects" && url.search === "?archived=1") {
      return json(archivedAnswer.body, archivedAnswer.status ?? 200);
    }
    if (url.pathname === "/api/milestones") return json({ milestones: [] });
    const lifecycle = /^\/api\/projects\/([^/]+)\/lifecycle$/.exec(url.pathname);
    if (lifecycle) return json(lifecycleAnswer.body, lifecycleAnswer.status ?? 200);
    throw new Error(`Unexpected ${url.pathname}${url.search}`);
  }) as typeof fetch;

  const emptyIssues = { data: [], status: "empty" as const, error: null, asOf: Date.now(), inFlight: false };
  let host = document.createElement("div");
  let root = createRoot(host);

  async function mount(project: string, archivePending?: "reading" | "unproved") {
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
    await act(async () => {
      root.render(React.createElement(NavigationProvider, {
        adapter: hostNavigation,
        children: React.createElement(LocaleProvider, null,
          React.createElement(ProjectsOverview, {
            project,
            projects: [active],
            issues: emptyIssues,
            archivePending,
            onOpenIssue: () => undefined,
            onRetry: () => undefined,
          })),
      }));
    });
  }
  async function unmount() {
    await act(async () => root.unmount());
    host.remove();
  }
  const href = (text: string) =>
    Array.from(host.querySelectorAll("a")).find((item) => item.textContent?.includes(text))?.getAttribute("href") ?? null;

  /* 1. A proved capability lists archived rows and routes to them. */
  archivedAnswer = { body: { archive_supported: true, projects: [archivedRow] } };
  await mount("all");
  await settle(() => assert(host.textContent?.includes("legacy-app"), "the archived row is listed"));
  assert(host.textContent?.includes("Archived projects"), "the archived section is discoverable");
  assert(
    Array.from(host.querySelectorAll(".chip")).some((chip) => chip.textContent?.trim() === "Archived"),
    "the row is marked archived, not active",
  );
  assert(href("legacy-app") === "/projects/legacy-app", "the row opens the archived project by key");
  assert(href("Open settings") === "/projects/legacy-app/settings", "the row offers its settings route for restore");
  await unmount();

  /* 2. An older server that ignores the query says unavailable — it never
        claims there are no archived projects. */
  archivedAnswer = { body: { projects: [active] } };
  await mount("all");
  await settle(() => assert(host.textContent?.includes("does not offer an archived-project view"), "the missing capability is called out"));
  assert(!host.textContent?.includes("No archived projects."), "an ignored query is not an empty archived list");
  assert(href("legacy-app") === null, "no archived row is invented from the active list");
  await unmount();

  /* 3. A proved empty list is an honest empty state. */
  archivedAnswer = { body: { archive_supported: true, projects: [] } };
  await mount("all");
  await settle(() => assert(host.textContent?.includes("No archived projects."), "the proved empty list says so"));
  await unmount();

  /* 4. An archived project page loads by key even though it is not in the
        active list, and points at its settings for restore. */
  archivedAnswer = { body: { archive_supported: true, projects: [archivedRow] } };
  await mount("legacy-app");
  await settle(() => assert(host.textContent?.includes("This project is archived"), "the archived project page says it is archived"));
  assert(href("Open settings to restore") === "/projects/legacy-app/settings", "the page routes to its restore control");
  await unmount();

  /* 5. Without the capability the project page is unchanged — no false
        "active" or "archived" claim for an older server. */
  lifecycleAnswer = { body: { archive_unavailable: true } };
  await mount("legacy-app");
  await settle(() => assert(host.textContent?.includes("legacy-app"), "the project page still renders"));
  assert(!host.textContent?.includes("This project is archived"), "an unavailable capability is not shown as archived");
  assert(!host.textContent?.includes("could not be proved"), "an unavailable capability is silent on the project page");
  await unmount();

  /* 6. A slug whose archive state is unproved shows that state and never
        presents the shared list's emptiness as the project's work. */
  await mount("mystery", "unproved");
  await settle(() => assert(host.textContent?.includes("could not be proved"), "the unproved archive state is stated"));
  assert(!host.textContent?.includes("No open issues in this project."), "unproved work is not reported as empty");
  assert(!host.textContent?.includes("No active epics yet."), "unproved epics are not reported as empty");
  await unmount();

  console.log("project archived discovery checks passed");
}

void main().catch((error: unknown) => {
  console.error(error);
  throw error;
});
