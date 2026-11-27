/** CAD-2872: ProjectSettings hosted Project details edit form, cache sync, locks, and error mapping. */
import type * as HappyDom from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDomClient from "react-dom/client";
import type * as Locale from "../src/lib/locale";
import type * as PrivateSession from "../src/lib/privateSession";
import type * as Resources from "../src/lib/resources";
import type * as ProjectSettingsModule from "../src/features/projects/ProjectSettings";
import type { Project } from "../src/lib/types";

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
  const { resources } = require("../src/lib/resources") as typeof Resources;
  const ProjectSettings = (
    require("../src/features/projects/ProjectSettings") as typeof ProjectSettingsModule
  ).default;
  allowPrivateReads();

  let currentProject: Project = {
    key: "alpha",
    name: "Alpha Launch",
    prefix: "ALP",
    goal: "Ship Q4 beta.",
    preset: "software",
    components: [],
    repos: [],
    issues: 2,
  };
  let patchCalls: Array<Record<string, unknown>> = [];
  let nextPatchFailure: { status: number; code: string; message: string } | null = null;

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
    if (url.pathname === "/api/projects" && (!init?.method || init.method === "GET")) {
      return json({ projects: [currentProject] });
    }
    if (url.pathname === "/api/projects/alpha" && init?.method === "PATCH") {
      const body = JSON.parse(String(init.body ?? "{}")) as Record<string, unknown>;
      patchCalls.push(body);
      if (nextPatchFailure) {
        const fail = nextPatchFailure;
        nextPatchFailure = null;
        return json(
          { ok: false, error: { code: fail.code, message: fail.message } },
          fail.status,
        );
      }
      currentProject = {
        ...currentProject,
        ...(typeof body.name === "string" ? { name: body.name } : {}),
        ...(body.goal !== undefined ? { goal: body.goal === "" || body.goal === null ? undefined : String(body.goal) } : {}),
        ...(body.preset !== undefined ? { preset: body.preset === "" || body.preset === null ? undefined : (String(body.preset) as Project["preset"]) } : {}),
      };
      return json({ project: currentProject });
    }
    if (url.pathname === "/api/projects/alpha/policy") {
      return json({
        project: "alpha",
        recorded: true,
        policy: { visibility: "workspace_visible", grants: [], revision: 1 },
        private_available: false,
      });
    }
    if (url.pathname === "/api/projects/alpha/lifecycle") {
      return json({
        archive_capability: "cad2255.project-archive.v1",
        project: "alpha",
        archived: false,
        revision: 0,
        archived_at: null,
        archived_by: null,
        changed_at: null,
        changed_by: null,
        blockers: [],
      });
    }
    if (url.pathname === "/api/team/members") {
      return json({ members: [] });
    }
    return json({ error: `unexpected ${url.pathname}` }, 404);
  }) as typeof globalThis.fetch;

  const setInputValue = (el: HTMLInputElement, value: string) => {
    const setter = Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")?.set;
    assert(setter, "HTMLInputElement value setter missing");
    setter.call(el, value);
    el.dispatchEvent(new win.Event("input", { bubbles: true }) as unknown as Event);
  };

  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  const toasts: string[] = [];

  const renderSettings = async (props?: Partial<{ block: string | null; archived: boolean; hosted: boolean | null }>) => {
    await act(async () => {
      root.render(
        React.createElement(
          LocaleProvider,
          null,
          React.createElement(ProjectSettings, {
            project: "alpha",
            projectInfo: currentProject,
            block: props?.block ?? null,
            archived: props?.archived ?? false,
            hosted: props?.hosted ?? true,
            onToast: (text: string) => toasts.push(text),
          }),
        ),
      );
    });
  };

  // 1. Native / non-hosted mode hides Project details card
  await renderSettings({ hosted: false });
  assert(
    !container.querySelector('[aria-label="Project details"]'),
    "Project details card should be hidden when hosted is false",
  );
  assert(
    container.querySelector("h1")?.textContent === "Alpha Launch",
    `expected display name in h1, got ${container.querySelector("h1")?.textContent}`,
  );

  // 2. Hosted owner renders Project details prefilled from projectInfo / resources.projects
  await renderSettings({ hosted: true });
  await settle(() => {
    assert(
      Boolean(container.querySelector('[aria-label="Project details"]')),
      "expected Project details card in hosted mode",
    );
  });

  const detailsCard = () =>
    container.querySelector('[aria-label="Project details"]') as HTMLElement;
  const inputs = () => Array.from(detailsCard().querySelectorAll("input")) as HTMLInputElement[];
  const nameInput = () => inputs()[0];
  const goalInput = () => inputs()[1];
  const saveBtn = () =>
    Array.from(detailsCard().querySelectorAll("button")).find((b) =>
      b.textContent?.includes("Save details"),
    ) as HTMLButtonElement | undefined;
  const resetBtn = () =>
    Array.from(detailsCard().querySelectorAll("button")).find((b) =>
      b.textContent?.includes("Reset"),
    ) as HTMLButtonElement | undefined;
  const presetBtn = (label: string) =>
    Array.from(detailsCard().querySelectorAll("button")).find((b) =>
      b.textContent?.trim() === label,
    ) as HTMLButtonElement | undefined;

  assert(nameInput().value === "Alpha Launch", `expected prefilled name, got ${nameInput().value}`);
  assert(goalInput().value === "Ship Q4 beta.", `expected prefilled goal, got ${goalInput().value}`);
  assert(
    presetBtn("Software")?.getAttribute("aria-pressed") === "true",
    "expected Software preset selected",
  );
  assert(saveBtn()?.disabled === true, "Save details should start disabled before editing");

  // 3. Edit fields + test Reset
  await act(async () => {
    setInputValue(nameInput(), "Temp Edit");
  });
  assert(resetBtn() !== undefined, "Reset button should appear when dirty");
  await act(async () => {
    resetBtn()?.click();
  });
  assert(nameInput().value === "Alpha Launch", "Reset should restore baseline name");

  // 4. Edit name, goal, preset and save
  await act(async () => {
    setInputValue(nameInput(), "Alpha Phoenix");
    setInputValue(goalInput(), "Zero-downtime rollout.");
    presetBtn("Operations")?.click();
  });
  assert(saveBtn()?.disabled === false, "Save details should enable when dirty");
  await act(async () => {
    saveBtn()?.click();
  });

  await settle(() => {
    assert(patchCalls.length === 1, `expected 1 PATCH call, got ${patchCalls.length}`);
    assert(toasts.includes("Project details saved."), `expected saved toast, got ${JSON.stringify(toasts)}`);
  });
  assert(
    patchCalls[0].name === "Alpha Phoenix" &&
      patchCalls[0].goal === "Zero-downtime rollout." &&
      patchCalls[0].preset === "operations",
    `unexpected PATCH payload: ${JSON.stringify(patchCalls[0])}`,
  );
  assert(
    container.querySelector("h1")?.textContent === "Alpha Phoenix",
    `expected updated h1, got ${container.querySelector("h1")?.textContent}`,
  );
  const cached = resources.projects.get().data?.find((item: Project) => item.key === "alpha");
  assert(
    cached?.name === "Alpha Phoenix" && cached?.goal === "Zero-downtime rollout." && cached?.preset === "operations",
    `expected resources.projects updated, got ${JSON.stringify(cached)}`,
  );

  // 5. Clear goal & toggle active preset off -> sends empty strings to clear on backend
  await act(async () => {
    setInputValue(goalInput(), "   ");
    presetBtn("Operations")?.click();
  });
  assert(
    presetBtn("Operations")?.getAttribute("aria-pressed") === "false",
    "clicking active preset should toggle it off",
  );
  await act(async () => {
    saveBtn()?.click();
  });
  await settle(() => {
    assert(patchCalls.length === 2, `expected 2 PATCH calls, got ${patchCalls.length}`);
  });
  assert(
    patchCalls[1].goal === "" && patchCalls[1].preset === "",
    `expected clearing payload { goal: "", preset: "" }, got ${JSON.stringify(patchCalls[1])}`,
  );

  // 6. Server refusal preserves edits and renders inline role="alert"
  nextPatchFailure = { status: 403, code: "owner_required", message: "Owner required" };
  await act(async () => {
    setInputValue(nameInput(), "Blocked Rename");
  });
  await act(async () => {
    saveBtn()?.click();
  });
  await settle(() => {
    const alert = detailsCard().querySelector('[role="alert"]');
    assert(
      alert?.textContent?.includes("Only a workspace owner can edit project details."),
      `expected mapped owner error alert, got ${alert?.textContent}`,
    );
  });
  assert(nameInput().value === "Blocked Rename", "failed save must preserve user edits");

  // 7. Read-only / archived state disables inputs & hides save button
  await renderSettings({ hosted: true, archived: true });
  await settle(() => {
    assert(nameInput().disabled === true, "archived project should disable name input");
    assert(goalInput().disabled === true, "archived project should disable goal input");
    assert(!saveBtn(), "archived project should hide Save details button");
    assert(
      detailsCard().textContent?.includes("This project is archived. Restore it before editing its details."),
      "expected archived notice inside Project details card",
    );
  });

  // 8. zh-TW locale coverage (mount fresh LocaleProvider with stored preference)
  await act(async () => {
    root.unmount();
  });
  win.localStorage.setItem("cadence-locale", "zh-TW");
  const zhRoot = createRoot(container);
  await act(async () => {
    zhRoot.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(ProjectSettings, {
          project: "alpha",
          projectInfo: currentProject,
          block: null,
          archived: false,
          hosted: true,
          onToast: (text: string) => toasts.push(text),
        }),
      ),
    );
  });
  await settle(() => {
    assert(
      container.textContent?.includes("專案詳情") &&
        container.textContent?.includes("儲存詳情"),
      `expected zh-TW translations in settings, got ${container.textContent}`,
    );
  });

  await act(async () => {
    zhRoot.unmount();
  });
  console.log("projectSettingsDetails tests passed");
}

main().catch((err) => {
  console.error(err);
  throw err;
});
