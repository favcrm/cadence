import type * as HappyDom from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDomClient from "react-dom/client";
import type * as Navigation from "../screen-ui/src/navigation";
import type * as ProjectsOverviewModule from "../src/features/projects/ProjectsOverview";
import type * as ApiModule from "../src/lib/api";
import type * as ApiErrorModule from "../src/lib/apiError";
import type * as LocaleModule from "../src/lib/locale";
import type * as PrivateSessionModule from "../src/lib/privateSession";
import type * as ProjectCreateModule from "../src/features/projects/projectCreate";
import type { ResourceState } from "../src/lib/cache";
import type { CreateProjectResponse, IssueCard } from "../src/lib/types";

declare function require(name: string): unknown;

function ok(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}

function eq<T>(actual: T, expected: T, message: string): void {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(`${message}: ${JSON.stringify(actual)} !== ${JSON.stringify(expected)}`);
  }
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

function setupDom(url = "http://localhost/projects") {
  const { Window } = require("happy-dom") as typeof HappyDom;
  const win = new Window({ url });
  for (const name of [
    "window",
    "document",
    "Node",
    "Element",
    "HTMLElement",
    "HTMLInputElement",
    "HTMLButtonElement",
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
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", {
    value: true,
    configurable: true,
  });
  Object.defineProperty(globalThis, "requestAnimationFrame", {
    value: (fn: FrameRequestCallback) => {
      fn(0);
      return 0;
    },
    configurable: true,
  });
  return win;
}

async function main() {
  const win = setupDom();

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
  const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof Navigation;
  const { api } = require("../src/lib/api") as typeof ApiModule;
  const { ApiError } = require("../src/lib/apiError") as typeof ApiErrorModule;
  const { LocaleProvider, translateUiText } = require("../src/lib/locale") as typeof LocaleModule;
  const { allowPrivateReads } = require("../src/lib/privateSession") as typeof PrivateSessionModule;
  const {
    PROJECT_PRESETS,
    createProjectErrorMessage,
    deriveProjectPrefix,
    slugifyProjectKey,
  } = require("../src/features/projects/projectCreate") as typeof ProjectCreateModule;
  const ProjectsOverview = (
    require("../src/features/projects/ProjectsOverview") as typeof ProjectsOverviewModule
  ).default;

  allowPrivateReads();

  // 1. Pure helpers: slugifyProjectKey & deriveProjectPrefix
  eq(slugifyProjectKey("Customer Onboarding"), "customer-onboarding", "slugify spaces");
  eq(slugifyProjectKey("  --Q4 Launch!! 2026--  "), "q4-launch-2026", "slugify punctuation");
  eq(slugifyProjectKey("---"), "", "slugify dashes only");
  eq(slugifyProjectKey("中文專案"), "", "slugify non-ascii");
  eq(slugifyProjectKey(`${"a".repeat(31)}-tail`), "a".repeat(31), "slugify trims trailing dash at 32");
  eq(slugifyProjectKey("b".repeat(40)), "b".repeat(32), "slugify caps at 32");

  eq(deriveProjectPrefix("reminders"), "REM", "derive prefix");
  eq(deriveProjectPrefix("a-b"), "AB", "derive short hyphenated prefix");
  eq(deriveProjectPrefix("3d-art"), "", "derive refuses leading digit");
  eq(deriveProjectPrefix(""), "", "derive empty");
  eq(
    PROJECT_PRESETS.map((p) => p.value),
    ["general", "client_delivery", "marketing", "software", "operations"],
    "preset allowlist",
  );

  // 2. Error message mapping across native & hosted error shapes + zh-TW
  eq(
    createProjectErrorMessage(new ApiError("exists", 409, { code: "project_exists" })),
    "A project with this key already exists. Choose a different key in Advanced.",
    "project_exists copy",
  );
  eq(
    createProjectErrorMessage(new ApiError("conflict", 409, { code: "prefix_conflict" })),
    "That ID prefix is already used by another project. Choose a different prefix in Advanced.",
    "prefix_conflict copy",
  );
  eq(
    createProjectErrorMessage(new ApiError("forbidden", 403, { code: "not_permitted" })),
    "Only the workspace owner or operator can create projects.",
    "not_permitted copy",
  );
  eq(
    createProjectErrorMessage(new ApiError("owner required", 403, { code: "owner_required" })),
    "Only the workspace owner or operator can create projects.",
    "owner_required copy",
  );
  eq(
    createProjectErrorMessage(new ApiError("operator only", 403, { check: "operator_only" })),
    "Only the workspace owner or operator can create projects.",
    "operator_only guard_fail copy",
  );
  eq(
    createProjectErrorMessage(
      new ApiError("daemon missing", 501, { code: "capability_unavailable" }),
    ),
    "Project creation is not supported by this daemon yet.",
    "capability_unavailable copy",
  );
  eq(
    createProjectErrorMessage(
      new ApiError("Project name exceeds limit", 400, { code: "validation_failed" }),
    ),
    "Project name exceeds limit",
    "validation_failed preserves message",
  );
  eq(
    createProjectErrorMessage(new TypeError("Failed to fetch")),
    "Could not create project. Check your connection and try again.",
    "network failure fallback",
  );
  eq(
    createProjectErrorMessage(
      new ApiError("exists", 409, { code: "project_exists" }),
      (s) => translateUiText(s, "zh-TW"),
    ),
    "此專案代號已存在。請在「進階」改用其他代號。",
    "zh-TW translation",
  );

  // 3. api.createProject preserves error codes for BOTH native { code, error } and hosted { ok:false, error:{ code, message } }
  const jsonResponse = (body: unknown, status: number) =>
    new Response(JSON.stringify(body), {
      status,
      headers: { "Content-Type": "application/json" },
    });

  globalThis.fetch = (async () =>
    jsonResponse({ code: "prefix_conflict", error: "Prefix DEM is project 'demo''s" }, 409)) as typeof globalThis.fetch;
  let nativeErr: unknown = null;
  try {
    await api.createProject({ name: "Demo Two", key: "demo-two" });
  } catch (e) {
    nativeErr = e;
  }
  ok(nativeErr instanceof ApiError, "native error must be ApiError");
  eq(nativeErr.status, 409, "native status");
  eq(nativeErr.code, "prefix_conflict", "native code");
  ok(
    createProjectErrorMessage(nativeErr).includes("ID prefix is already used"),
    "native message mapped",
  );

  globalThis.fetch = (async () =>
    jsonResponse(
      { ok: false, error: { code: "project_exists", message: "Project 'demo' already exists" } },
      409,
    )) as typeof globalThis.fetch;
  let hostedErr: unknown = null;
  try {
    await api.createProject({ name: "Demo", key: "demo" });
  } catch (e) {
    hostedErr = e;
  }
  ok(hostedErr instanceof ApiError, "hosted error must be ApiError");
  eq(hostedErr.status, 409, "hosted status");
  eq(hostedErr.code, "project_exists", "hosted code");
  ok(
    createProjectErrorMessage(hostedErr).includes("already exists"),
    "hosted message mapped",
  );

  // 4. DOM & keyboard interaction for ProjectsOverview + NewProjectModal
  const emptyIssues: ResourceState<IssueCard[]> = {
    status: "empty",
    data: [],
    error: null,
    inFlight: false,
    asOf: Date.now(),
  };
  const navigated: string[] = [];
  const nav = {
    subscribe: () => () => {},
    snapshot: () => "/projects",
    navigate: (to: string) => navigated.push(to),
    replaceHash: () => {},
    href: (to: string) => to,
    modifiedClicks: "browser" as const,
  };

  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);

  const renderOverview = async (props: {
    canCreateProject: boolean;
    hosted: boolean;
    onProjectCreated?: (created: CreateProjectResponse) => void;
  }) => {
    const overviewEl = React.createElement(ProjectsOverview, {
      project: "all",
      projects: [],
      issues: emptyIssues,
      onOpenIssue: () => {},
      onRetry: () => {},
      canCreateProject: props.canCreateProject,
      hosted: props.hosted,
      onProjectCreated: props.onProjectCreated,
    });
    const localeEl = React.createElement(LocaleProvider, null, overviewEl);
    await act(async () => {
      root.render(React.createElement(NavigationProvider, { adapter: nav, children: localeEl }));
    });
  };

  // Hidden when canCreateProject=false
  await renderOverview({ canCreateProject: false, hosted: false });
  ok(
    !Array.from(container.querySelectorAll("button")).some((b) =>
      b.textContent?.includes("New project"),
    ),
    "New project button must be hidden when canCreateProject is false",
  );

  // Visible when canCreateProject=true on native board
  const createdEvents: CreateProjectResponse[] = [];
  let postCallCount = 0;
  let lastRequestBody: Record<string, unknown> = {};
  let lastHeaders: Record<string, string> = {};

  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = new URL(
      typeof input === "string" ? input : String((input as Request).url),
      "http://localhost",
    );
    if (url.pathname === "/api/projects" && init?.method === "POST") {
      postCallCount += 1;
      lastRequestBody = JSON.parse(String(init.body ?? "{}")) as Record<string, unknown>;
      lastHeaders = (init.headers as Record<string, string>) ?? {};
      if (postCallCount === 1) {
        return jsonResponse(
          { code: "project_exists", error: "Project 'customer-onboarding' exists" },
          409,
        );
      }
      return jsonResponse(
        {
          project: {
            key: String(lastRequestBody.key ?? "customer-onboarding"),
            name: String(lastRequestBody.name ?? "Customer Onboarding"),
            prefix: String(lastRequestBody.prefix ?? "CUS"),
            goal: lastRequestBody.goal ?? null,
            preset: lastRequestBody.preset ?? null,
            components: [],
            repos: [],
          },
          visibility: "workspace-visible",
          duplicate: false,
        },
        201,
      );
    }
    if (url.pathname === "/api/projects") {
      return jsonResponse({ projects: [] }, 200);
    }
    return jsonResponse({ error: "unexpected" }, 404);
  }) as typeof globalThis.fetch;

  await renderOverview({
    canCreateProject: true,
    hosted: false,
    onProjectCreated: (created) => createdEvents.push(created),
  });

  const newBtn = Array.from(container.querySelectorAll("button")).find((b) =>
    b.textContent?.includes("New project"),
  ) as HTMLButtonElement | undefined;
  ok(newBtn, "New project button must render for operator");

  await act(async () => {
    newBtn.focus();
    newBtn.click();
  });

  const dialog = () => container.querySelector('[role="dialog"]') as HTMLElement | null;
  ok(dialog(), "Clicking New project opens modal dialog");
  eq(dialog()!.getAttribute("aria-modal"), "true", "aria-modal");

  const nameInput = dialog()!.querySelector("input[required]") as HTMLInputElement;
  ok(document.activeElement === nameInput, "Project name input receives initial focus");

  const advancedToggle = Array.from(dialog()!.querySelectorAll("button")).find((b) =>
    b.textContent?.includes("Advanced"),
  ) as HTMLButtonElement;
  eq(advancedToggle.getAttribute("aria-expanded"), "false", "Advanced starts collapsed");

  // Type name & select preset, then submit -> 409 project_exists expands Advanced with inline alert
  await act(async () => {
    const setter = Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")?.set;
    setter?.call(nameInput, "Customer Onboarding");
    nameInput.dispatchEvent(new win.Event("input", { bubbles: true }) as unknown as Event);
  });

  const softwareBtn = Array.from(dialog()!.querySelectorAll("button")).find((b) =>
    b.textContent?.includes("Software"),
  ) as HTMLButtonElement;
  await act(async () => {
    softwareBtn.click();
  });

  const submitBtn = () =>
    Array.from(dialog()!.querySelectorAll("button")).find((b) =>
      b.getAttribute("type") === "submit",
    ) as HTMLButtonElement;
  await act(async () => {
    submitBtn().click();
  });

  await settle(() => {
    const alert = dialog()?.querySelector('[role="alert"]');
    ok(
      alert?.textContent?.includes("already exists"),
      `expected inline refusal banner, got ${alert?.textContent}`,
    );
    eq(
      advancedToggle.getAttribute("aria-expanded"),
      "true",
      "project_exists expands Advanced so key is editable",
    );
  });

  // Repository input is present on native board (!hosted)
  const inputs = Array.from(dialog()!.querySelectorAll("input"));
  ok(
    inputs.some((el) => el.getAttribute("placeholder") === "/path/to/git/checkout"),
    "Native board shows Repository input in Advanced",
  );

  // Edit Key & resubmit -> succeeds, closes modal, navigates to /projects/<key>
  const keyInput = inputs.find(
    (el) => el.getAttribute("placeholder") === "customer-onboarding",
  ) as HTMLInputElement;
  await act(async () => {
    const setter = Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")?.set;
    setter?.call(keyInput, "customer-onboarding-2");
    keyInput.dispatchEvent(new win.Event("input", { bubbles: true }) as unknown as Event);
  });
  await act(async () => {
    submitBtn().click();
  });

  await settle(() => {
    eq(dialog(), null, "Modal closes after successful project creation");
    eq(createdEvents.length, 1, "onProjectCreated fired");
    eq(navigated[0], "/projects/customer-onboarding-2", "navigated to created project");
  });
  eq(lastHeaders["X-Cadence-Board"], "1", "X-Cadence-Board header");
  eq(lastRequestBody.key, "customer-onboarding-2", "submitted key");
  eq(lastRequestBody.preset, "software", "submitted preset");
  eq(lastRequestBody.visibility, "workspace-visible", "submitted visibility");

  // Re-open on hosted board: Repository hidden, Escape closes & restores focus
  await renderOverview({ canCreateProject: true, hosted: true });
  const hostedNewBtn = Array.from(container.querySelectorAll("button")).find((b) =>
    b.textContent?.includes("New project"),
  ) as HTMLButtonElement;
  await act(async () => {
    hostedNewBtn.focus();
    hostedNewBtn.click();
  });
  const hostedAdvToggle = Array.from(dialog()!.querySelectorAll("button")).find((b) =>
    b.textContent?.includes("Advanced"),
  ) as HTMLButtonElement;
  await act(async () => {
    hostedAdvToggle.click();
  });
  ok(
    !Array.from(dialog()!.querySelectorAll("input")).some(
      (el) => el.getAttribute("placeholder") === "/path/to/git/checkout",
    ),
    "Hosted board omits Repository input",
  );
  await act(async () => {
    dialog()!.dispatchEvent(
      new win.KeyboardEvent("keydown", { key: "Escape", bubbles: true }) as unknown as KeyboardEvent,
    );
  });
  eq(dialog(), null, "Escape closes NewProjectModal");
  ok(document.activeElement === hostedNewBtn, "Focus returns to trigger button on close");

  await act(async () => {
    root.unmount();
  });
  console.log("projectCreate UI tests passed");
}

main().catch((err) => {
  console.error(err);
  throw err;
});
