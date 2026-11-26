/** CAD-2830: ProjectSettings archive/restore confirmation and ProjectsOverview Active|Archived tabs. */
import type * as HappyDom from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDomClient from "react-dom/client";
import type * as Locale from "../src/lib/locale";
import type * as PrivateSession from "../src/lib/privateSession";
import type * as Navigation from "../screen-ui/src/navigation";
import type * as ProjectSettingsModule from "../src/features/projects/ProjectSettings";
import type * as ProjectsOverviewModule from "../src/features/projects/ProjectsOverview";
import type { ResourceState } from "../src/lib/cache";
import type { IssueCard, Project } from "../src/lib/types";

declare function require(name: string): unknown;

function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}

let act: typeof ReactModule.act;
async function settle(check: () => void) {
  for (let n = 0; n < 100; n++) {
    try {
      check();
      return;
    } catch (error) {
      if (n === 99) throw error;
      await act(async () => {
        await Promise.resolve();
      });
    }
  }
}

async function main() {
  const { Window } = require("happy-dom") as typeof HappyDom;
  const win = new Window({ url: "http://localhost/projects/alpha/settings" });
  for (const name of [
    "window",
    "document",
    "Node",
    "Element",
    "HTMLElement",
    "HTMLInputElement",
    "HTMLSelectElement",
    "HTMLTextAreaElement",
    "SVGElement",
    "navigator",
    "MutationObserver",
    "ResizeObserver",
    "Event",
    "MouseEvent",
    "KeyboardEvent",
    "location",
    "history",
    "sessionStorage",
    "localStorage",
  ] as const) {
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  }
  for (const name of ["addEventListener", "removeEventListener"] as const) {
    Object.defineProperty(globalThis, name, {
      value: win[name].bind(win),
      configurable: true,
      writable: true,
    });
  }
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  Object.defineProperty(globalThis, "requestAnimationFrame", {
    value: (fn: FrameRequestCallback) => {
      fn(0);
      return 0;
    },
    configurable: true,
  });

  const loader = require("module") as {
    prototype: { require: (this: unknown, id: string) => unknown };
  };
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
  const { allowPrivateReads } = require("../src/lib/privateSession") as typeof PrivateSession;
  const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof Navigation;
  const ProjectSettings = (require("../src/features/projects/ProjectSettings") as typeof ProjectSettingsModule).default;
  const ProjectsOverview = (require("../src/features/projects/ProjectsOverview") as typeof ProjectsOverviewModule).default;
  allowPrivateReads();

  let lifecycle = {
    archive_capability: "cad2255.project-archive.v1",
    project: "alpha",
    archived: false,
    revision: 0,
    archived_at: null as string | null,
    archived_by: null as string | null,
    changed_at: null as string | null,
    changed_by: null as string | null,
    blockers: [] as string[],
  };
  let lifecycleStatus = 200;
  let lastArchiveBody: { confirmation: string; expected_revision: number } | null = null;
  let lastRestoreBody: { confirmation: string; expected_revision: number } | null = null;

  const json = (value: unknown, status = 200) =>
    new Response(JSON.stringify(value), {
      status,
      headers: { "Content-Type": "application/json" },
    });

  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = new URL(
      typeof input === "string" ? input : String((input as Request).url),
      "http://localhost",
    );
    if (url.pathname === "/api/projects/alpha/policy") {
      return json({
        project: "alpha",
        recorded: true,
        policy: { visibility: "workspace_visible", members: [], version: 1 },
        private_available: false,
      });
    }
    if (url.pathname === "/api/projects/alpha/lifecycle") {
      return json(
        lifecycleStatus === 200 ? lifecycle : { error: "not implemented" },
        lifecycleStatus,
      );
    }
    if (url.pathname === "/api/projects/alpha/archive" && init?.method === "POST") {
      const body = JSON.parse(String(init.body ?? "{}")) as {
        confirmation: string;
        expected_revision: number;
      };
      lastArchiveBody = body;
      lifecycle = {
        ...lifecycle,
        archived: true,
        revision: 1,
        archived_at: "2026-11-26T00:00:00Z",
        archived_by: "operator",
        changed_at: "2026-11-26T00:00:00Z",
        changed_by: "operator",
      };
      return json(lifecycle);
    }
    if (url.pathname === "/api/projects/alpha/restore" && init?.method === "POST") {
      const body = JSON.parse(String(init.body ?? "{}")) as {
        confirmation: string;
        expected_revision: number;
      };
      lastRestoreBody = body;
      lifecycle = {
        ...lifecycle,
        archived: false,
        revision: 2,
        changed_at: "2026-11-26T01:00:00Z",
        changed_by: "operator",
      };
      return json(lifecycle);
    }
    return json({ error: `unexpected ${url.pathname}` }, 404);
  }) as typeof globalThis.fetch;

  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  const toasts: string[] = [];
  const changes: boolean[] = [];

  await act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(ProjectSettings, {
          project: "alpha",
          block: null,
          hosted: false,
          openIssuesCount: 3,
          onLifecycleChange: (next) => changes.push(next.archived),
          onToast: (text: string) => toasts.push(text),
        }),
      ),
    );
  });

  await settle(() => {
    assert(
      container.textContent?.includes("Project lifecycle"),
      `expected lifecycle card, got: ${container.textContent}`,
    );
    assert(
      container.textContent?.includes("3 open issues remain"),
      `expected warn-only open issues notice, got: ${container.textContent}`,
    );
  });

  const startBtn = Array.from(container.querySelectorAll("button")).find((b) =>
    b.textContent?.includes("Archive project"),
  );
  assert(startBtn, "expected Archive project button");
  await act(async () => {
    startBtn.click();
  });

  const confirmInput = container.querySelector("input[type='text']") as HTMLInputElement | null;
  assert(confirmInput, "expected archive confirmation input");
  const confirmBtn = () =>
    Array.from(container.querySelectorAll("button")).find((b) =>
      b.textContent?.includes("Confirm archive"),
    );
  assert(confirmBtn()?.disabled === true, "Confirm archive must stay disabled before exact match");

  // Type wrong key first -> button remains disabled.
  await act(async () => {
    const desc = Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")?.set;
    desc?.call(confirmInput, "beta");
    confirmInput.dispatchEvent(new win.Event("input", { bubbles: true }) as unknown as Event);
  });
  assert(confirmBtn()?.disabled === true, "Confirm archive must stay disabled on mismatch");

  // Type exact project key -> submit becomes enabled and archives.
  await act(async () => {
    const desc = Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")?.set;
    desc?.call(confirmInput, "alpha");
    confirmInput.dispatchEvent(new win.Event("input", { bubbles: true }) as unknown as Event);
  });
  assert(confirmBtn()?.disabled === false, "Confirm archive must enable on exact match");
  await act(async () => {
    confirmBtn()!.click();
  });

  await settle(() => {
    assert(lastArchiveBody?.confirmation === "alpha", "expected exact key confirmation sent");
    assert(lastArchiveBody?.expected_revision === 0, "expected revision 0");
    assert(container.textContent?.includes("Restore project"), "expected 1-click Restore button");
    assert(
      container.textContent?.includes(
        "This project is archived. Restore it before changing its access policy.",
      ),
      "expected archived policy lock explanation",
    );
    assert(
      !Array.from(container.querySelectorAll("button")).some((b) =>
        b.textContent?.includes("Save policy"),
      ),
      "Save policy must be omitted while archived",
    );
  });

  // 1-click restore works without re-typing key and preserves prior-runs note.
  assert(
    container.textContent?.includes("Prior runs are not restarted"),
    "expected no-restart explanation on archived project",
  );
  const restoreBtn = Array.from(container.querySelectorAll("button")).find((b) =>
    b.textContent?.includes("Restore project"),
  );
  assert(restoreBtn && !restoreBtn.disabled, "expected enabled Restore project button");
  await act(async () => {
    restoreBtn.click();
  });

  await settle(() => {
    assert(lastRestoreBody?.confirmation === "alpha", "expected confirmation on restore");
    assert(lastRestoreBody?.expected_revision === 1, "expected revision 1 on restore");
    assert(changes.join(",") === "true,false", `expected lifecycle callback transitions, got ${changes}`);
  });

  // 403 (non-operator) & 501 omit lifecycle card cleanly; archived={true} still locks policy.
  lifecycleStatus = 403;
  await act(async () => {
    root.render(React.createElement("div"));
  });
  await act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(ProjectSettings, {
          project: "alpha",
          block: null,
          archived: true,
          hosted: false,
          onToast: () => {},
        }),
      ),
    );
  });
  await settle(() => {
    assert(!container.textContent?.includes("Project lifecycle"), "403 must omit lifecycle card");
    assert(!container.querySelector("[role='alert']"), "403 must not show error alert");
    assert(
      container.textContent?.includes(
        "This project is archived. Restore it before changing its access policy.",
      ),
      "archived prop must still lock policy card on 403 lifecycle",
    );
  });

  // 501 omits lifecycle card cleanly.
  lifecycleStatus = 501;
  await act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(ProjectSettings, {
          project: "alpha",
          block: null,
          hosted: false,
          onToast: () => {},
        }),
      ),
    );
  });
  // Unmount and remount to trigger fresh lifecycle probe for unsupported server.
  await act(async () => {
    root.render(React.createElement("div"));
  });
  await act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(ProjectSettings, {
          project: "alpha",
          block: null,
          hosted: false,
          onToast: () => {},
        }),
      ),
    );
  });
  await settle(() => {
    assert(!container.textContent?.includes("Project lifecycle"), "501 must omit lifecycle card");
  });

  // ProjectsOverview renders Active and Archived tabs separately.
  const emptyIssues: ResourceState<IssueCard[]> = {
    status: "empty",
    data: [],
    error: null,
    inFlight: false,
    asOf: Date.now(),
  };
  const archivedProj: Project = {
    key: "legacy",
    prefix: "LEG",
    components: [],
    repos: [],
    issues: 4,
    archived: true,
    archived_at: "2026-11-26T00:00:00Z",
    archived_by: "operator",
  };
  const nav = {
    subscribe: () => () => {},
    snapshot: () => "/projects?archived=1",
    navigate: () => {},
    replaceHash: () => {},
    href: (to: string) => to,
    modifiedClicks: "browser" as const,
  };
  await act(async () => {
    root.render(
      <NavigationProvider adapter={nav}>
        <LocaleProvider>
          <ProjectsOverview
            project="all"
            projects={[]}
            issues={emptyIssues}
            onOpenIssue={() => {}}
            onRetry={() => {}}
            archivedTab={true}
            activeHref="/projects"
            archivedHref="/projects?archived=1"
            archivedProjects={[archivedProj]}
            archivedSupported={true}
          />
        </LocaleProvider>
      </NavigationProvider>,
    );
  });
  assert(container.textContent?.includes("legacy"), "expected archived project row on Archived tab");
  assert(
    container.textContent?.includes("Restore in settings"),
    "expected Restore in settings link on Archived tab",
  );

  await act(async () => {
    root.unmount();
  });
  console.log("project archive UI tests passed");
}

main().catch((err) => {
  console.error(err);
  throw err;
});
