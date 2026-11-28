/** CAD-2884: Hosted Project Team & 3-role Access Management, Project Owner delegation, Viewer read-only mode, and Overview owner surface. */
import type * as HappyDom from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDomClient from "react-dom/client";
import type * as Locale from "../src/lib/locale";
import type * as PrivateSession from "../src/lib/privateSession";
import type * as Resources from "../src/lib/resources";
import type * as Navigation from "../screen-ui/src/navigation";
import type * as ProjectSettingsModule from "../src/features/projects/ProjectSettings";
import type * as ProjectsOverviewModule from "../src/features/projects/ProjectsOverview";
import type * as PolicyContractModule from "../src/features/projects/policyContract";
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
  const { resources } = require("../src/lib/resources") as typeof Resources;
  const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof Navigation;
  const ProjectSettings = (
    require("../src/features/projects/ProjectSettings") as typeof ProjectSettingsModule
  ).default;
  const ProjectsOverview = (
    require("../src/features/projects/ProjectsOverview") as typeof ProjectsOverviewModule
  ).default;
  const { readPolicy } = require("../src/features/projects/policyContract") as typeof PolicyContractModule;
  allowPrivateReads();

  const rosterMembers = [
    { userId: "usr_ws", name: "Wayne Owner", email: "wayne@example.test", role: "owner", joinedAt: "2026-01-01" },
    { userId: "usr_powner", name: "Alice Chen", email: "alice@example.test", role: "member", joinedAt: "2026-01-02" },
    { userId: "usr_editor", name: "Bob Lin", email: "bob@example.test", role: "member", joinedAt: "2026-01-03" },
    { userId: "usr_viewer", name: "Carol Wu", email: "carol@example.test", role: "member", joinedAt: "2026-01-04" },
  ];

  const projectMeta: Project = {
    key: "alpha",
    name: "Alpha Project",
    prefix: "ALP",
    goal: "Ship multi-role access.",
    preset: "software",
    components: [],
    repos: [],
    issues: 1,
  };

  type PolicyPayload = {
    state: "ok";
    project: string;
    visibility: "workspace-visible" | "private";
    grants: string[];
    members: Array<{ userId: string; role: "owner" | "member" | "viewer" }>;
    revision: number;
    provenance: string;
    updated_by: string;
    updated_at: string;
    private_available: boolean;
    my_role: "workspace_owner" | "owner" | "member" | "viewer";
    capabilities: {
      can_manage: boolean;
      can_write: boolean;
      can_edit_details: boolean;
      can_archive: boolean;
    };
  };

  let policy: PolicyPayload = {
    state: "ok",
    project: "alpha",
    visibility: "workspace-visible",
    grants: [],
    members: [{ userId: "usr_powner", role: "owner" }],
    revision: 1,
    provenance: "hosted-owner-attested",
    updated_by: "usr_ws",
    updated_at: "2026-11-28T00:00:00Z",
    private_available: true,
    my_role: "workspace_owner",
    capabilities: {
      can_manage: true,
      can_write: true,
      can_edit_details: true,
      can_archive: true,
    },
  };

  let lastPut: Record<string, unknown> | null = null;
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
    const method = init?.method ?? "GET";
    if (url.pathname === "/api/projects" && method === "GET") {
      return json({ projects: [projectMeta] });
    }
    if (url.pathname === "/api/team/members" && method === "GET") {
      return json({ members: rosterMembers });
    }
    if (url.pathname === "/api/projects/alpha/policy" && method === "GET") {
      return json(policy);
    }
    if (url.pathname === "/api/projects/alpha/policy" && method === "PUT") {
      const body = JSON.parse(String(init?.body ?? "{}")) as {
        visibility: "workspace-visible" | "private";
        grants: string[];
        members?: Array<{ userId: string; role: "owner" | "member" | "viewer" }>;
      };
      lastPut = body;
      policy = {
        ...policy,
        visibility: body.visibility,
        grants: [...body.grants],
        members: body.members ? body.members.map((m) => ({ ...m })) : policy.members,
        revision: policy.revision + 1,
      };
      return json(policy);
    }
    if (url.pathname === "/api/projects/alpha/lifecycle" && method === "GET") {
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
    return json({ error: `Unexpected ${method} ${url.pathname}` }, 404);
  }) as typeof fetch;

  // 1. Workspace Owner switches existing workspace-visible project to private, adds Viewer, saves.
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const toasts: string[] = [];

  resources.projects.write(() => [projectMeta]);
  await act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(ProjectSettings, {
          project: "alpha",
          projectInfo: projectMeta,
          block: null,
          sessionBlock: null,
          hosted: true,
          onToast: (text: string) => toasts.push(text),
        }),
      ),
    );
  });

  await settle(() => {
    assert(
      host.textContent?.includes("Project Team & Access") &&
        host.textContent?.includes("Workspace Owners always have full access to all projects.") &&
        host.textContent?.includes("Alice Chen"),
      `expected Project Team card and owner note, got: ${host.textContent}`,
    );
  });

  const buttonByText = (text: string) =>
    Array.from(host.querySelectorAll("button")).find((btn) =>
      btn.textContent?.includes(text),
    ) as HTMLButtonElement | undefined;

  // Toggle Private (unblocked by private_available: true).
  const privateBtn = Array.from(host.querySelectorAll("button.ps-visibility")).find((b) =>
    b.textContent?.includes("Private"),
  ) as HTMLButtonElement | undefined;
  assert(privateBtn && !privateBtn.disabled, "Private option should be enabled");
  await act(async () => {
    privateBtn.click();
  });

  // Add Carol Wu as Viewer.
  const memberSelect = host.querySelector<HTMLButtonElement>('[aria-label="Grant a workspace member"]');
  const roleSelect = host.querySelector<HTMLButtonElement>('[aria-label="Project role"]');
  assert(memberSelect && roleSelect, "expected member picker and role selector");
  await act(async () => memberSelect.click());
  const carolOption = Array.from(document.querySelectorAll('[role="option"]')).find((el) =>
    el.textContent?.includes("Carol Wu"),
  ) as HTMLElement | undefined;
  assert(carolOption, "Carol Wu appears in picker");
  await act(async () => carolOption.click());

  await act(async () => roleSelect.click());
  const viewerOption = Array.from(document.querySelectorAll('[role="option"]')).find(
    (el) => el.textContent?.trim() === "Viewer",
  ) as HTMLElement | undefined;
  assert(viewerOption, "Viewer role option exists");
  await act(async () => viewerOption.click());

  await act(async () => buttonByText("Add")!.click());
  await settle(() => {
    assert(host.textContent?.includes("Carol Wu"), "Carol Wu added to team roster");
  });

  const saveBtn = buttonByText("Save policy");
  assert(saveBtn && !saveBtn.disabled, "Save policy should be enabled when private_available=true");
  await act(async () => saveBtn.click());
  await settle(() => {
    assert(lastPut !== null, "PUT /policy should be sent");
    assert(lastPut.visibility === "private", "saved visibility should be private");
    assert(
      JSON.stringify(lastPut.members) ===
        JSON.stringify([
          { userId: "usr_powner", role: "owner" },
          { userId: "usr_viewer", role: "viewer" },
        ]),
      `unexpected saved members: ${JSON.stringify(lastPut.members)}`,
    );
    assert(toasts.includes("Project access saved."), "expected save toast");
  });

  // 2. Non-workspace-owner Project Owner (`block="Only the operator..."`, `sessionBlock=null`)
  // has Project Details, Team, and Archive unlocked, and cannot demote/remove last owner.
  policy = {
    ...policy,
    my_role: "owner",
    capabilities: {
      can_manage: true,
      can_write: true,
      can_edit_details: true,
      can_archive: true,
    },
  };
  await act(async () => {
    root.render(React.createElement("div"));
  });
  resources.projectPolicy("alpha").write(() => policy);
  await act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(ProjectSettings, {
          project: "alpha",
          projectInfo: projectMeta,
          block: "Only the operator can change who can see a project — this session is not the operator's.",
          sessionBlock: null,
          hosted: true,
          onToast: () => {},
        }),
      ),
    );
  });

  await settle(() => {
    assert(buttonByText("Archive project") && !buttonByText("Archive project")!.disabled, "Project Owner can archive");
    assert(buttonByText("Save details") !== undefined, "Project Owner sees unlocked details form");
    assert(
      host.textContent?.includes("A project must keep at least one Project Owner."),
      "last Project Owner safeguard notice shown",
    );
  });
  const aliceRoleTrigger = host.querySelector<HTMLButtonElement>(
    '[aria-label="Project role · Alice Chen"]',
  );
  assert(aliceRoleTrigger?.disabled === true, "last Project Owner role selector must be disabled");

  // 3. Viewer (`my_role="viewer"`) sees read-only Viewer notice and no edit controls.
  policy = {
    ...policy,
    my_role: "viewer",
    capabilities: {
      can_manage: false,
      can_write: false,
      can_edit_details: false,
      can_archive: false,
    },
  };
  await act(async () => {
    root.render(React.createElement("div"));
  });
  resources.projectPolicy("alpha").write(() => policy);
  await act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(ProjectSettings, {
          project: "alpha",
          projectInfo: projectMeta,
          block: "Only the operator can change who can see a project — this session is not the operator's.",
          sessionBlock: null,
          hosted: true,
          onToast: () => {},
        }),
      ),
    );
  });

  await settle(() => {
    assert(
      host.textContent?.includes("You have read-only Viewer access to this project."),
      "Viewer read-only message must appear",
    );
    assert(buttonByText("Save policy") === undefined, "Viewer cannot save policy");
    assert(buttonByText("Archive project") === undefined, "Viewer cannot archive");
  });

  // 4. ProjectsOverview displays Project Owner(s) and Read-only Viewer pill.
  const emptyIssues: ResourceState<IssueCard[]> = {
    status: "ok",
    data: [],
    error: null,
    inFlight: false,
    asOf: Date.now(),
  };
  await act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(
          NavigationProvider,
          { adapter: { navigate: () => {}, href: (h: string) => h, modifiedClicks: "browser" as const }, children: null },
          React.createElement(ProjectsOverview, {
            project: "alpha",
            projects: [projectMeta],
            issues: emptyIssues,
            onOpenIssue: () => {},
            onRetry: () => {},
            hosted: true,
            projectPolicy: readPolicy(policy, true),
          }),
        ),
      ),
    );
  });
  await settle(() => {
    assert(
      host.textContent?.includes("Project owner:") &&
        host.textContent?.includes("Alice Chen") &&
        host.textContent?.includes("Read-only Viewer"),
      `expected owner and Viewer badge on Overview, got: ${host.textContent}`,
    );
  });

  // 5. zh-TW locale renders translated Project Team & Access UI cleanly.
  localStorage.setItem("cadence-locale", "zh-TW");
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
          projectInfo: projectMeta,
          block: null,
          sessionBlock: null,
          hosted: true,
          onToast: () => {},
        }),
      ),
    );
  });
  await settle(() => {
    assert(
      host.textContent?.includes("專案團隊與存取權限") &&
        host.textContent?.includes("工作區擁有者對所有專案一律保有完整存取權。"),
      `expected zh-TW translations in Project Team UI, got: ${host.textContent}`,
    );
  });

  localStorage.removeItem("cadence-locale");
  await act(async () => {
    root.unmount();
  });
  console.log("projectTeamSettings tests passed");
}

main().catch((err) => {
  console.error(err);
  throw err;
});
