/** CAD-2888: Settings -> Members optional initial project & role assignment on workspace invite. */
import type * as HappyDom from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDomClient from "react-dom/client";
import type * as Locale from "../src/lib/locale";
import type * as PrivateSession from "../src/lib/privateSession";
import type * as MembersModule from "../src/features/settings/Members";

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

function setupDom() {
  const { Window } = require("happy-dom") as typeof HappyDom;
  const win = new Window({ url: "http://localhost/settings/members" });
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
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true, configurable: true });
  Object.defineProperty(globalThis, "requestAnimationFrame", {
    value: (fn: FrameRequestCallback) => {
      fn(0);
      return 0;
    },
    configurable: true,
  });
}

async function selectValue(trigger: HTMLButtonElement, optionText: string) {
  await act(async () => {
    trigger.click();
  });
  await settle(() => {
    const option = Array.from(document.querySelectorAll<HTMLElement>('[role="option"]')).find(
      (el) => el.textContent?.trim() === optionText,
    );
    assert(option, `Expected Select option "${optionText}"`);
  });
  const option = Array.from(document.querySelectorAll<HTMLElement>('[role="option"]')).find(
    (el) => el.textContent?.trim() === optionText,
  )!;
  await act(async () => {
    option.click();
  });
}

async function main() {
  setupDom();

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
  const Members = (require("../src/features/settings/Members") as typeof MembersModule).default;
  allowPrivateReads();

  const json = (value: unknown, status = 200) =>
    new Response(JSON.stringify(value), {
      status,
      headers: { "Content-Type": "application/json" },
    });

  const projects = [
    { key: "alpha", name: "Alpha Launch" },
    { key: "docs", name: "Documentation" },
    { key: "ops", name: "Operations" },
  ];

  type StoredInvite = {
    id: string;
    email: string;
    role: "owner" | "member";
    expiresAt: string;
    project_grants?: Array<{ project: string; role: "owner" | "member" | "viewer" }>;
  };

  let invites: StoredInvite[] = [
    {
      id: "inv_seed",
      email: "existing@example.test",
      role: "member",
      expiresAt: "2026-12-05T00:00:00Z",
      project_grants: [{ project: "archived-slug", role: "viewer" }],
    },
  ];
  const postedBodies: Array<Record<string, unknown>> = [];
  let failValidationOnce = false;

  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = new URL(
      typeof input === "string" ? input : String((input as Request).url),
      "http://localhost",
    );
    const method = init?.method ?? "GET";
    if (url.pathname === "/api/team/members" && method === "GET") {
      return json({
        members: [
          {
            userId: "usr_owner",
            name: "Ada Owner",
            email: "ada@example.test",
            role: "owner",
            joinedAt: "2026-01-01T00:00:00Z",
          },
        ],
      });
    }
    if (url.pathname === "/api/team/invites" && method === "GET") {
      return json({ invites });
    }
    if (url.pathname === "/api/team/invites" && method === "POST") {
      const body = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>;
      postedBodies.push(body);
      if (failValidationOnce) {
        failValidationOnce = false;
        return json(
          { error: { code: "validation_failed", message: "project \"ops\" does not exist" } },
          400,
        );
      }
      const created: StoredInvite = {
        id: `inv_${postedBodies.length}`,
        email: String(body.email),
        role: body.role === "owner" ? "owner" : "member",
        expiresAt: "2026-12-06T00:00:00Z",
        ...(Array.isArray(body.project_grants)
          ? {
              project_grants: body.project_grants as Array<{
                project: string;
                role: "owner" | "member" | "viewer";
              }>,
            }
          : {}),
      };
      invites = [...invites, created];
      return json({ invite: created });
    }
    return json({ error: `Unexpected ${method} ${url.pathname}` }, 404);
  }) as typeof fetch;

  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);

  await act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(Members, {
          viewer: { operator: true, readOnly: false },
          projects,
        }),
      ),
    );
  });

  await settle(() => {
    assert(container.textContent?.includes("existing@example.test"), "Seeded invite should render");
    assert(container.textContent?.includes("archived-slug"), "Fallback project slug should render on row");
    assert(container.textContent?.includes("Viewer"), "Role label should render on row");
  });

  // Open seeded invite drawer and confirm Initial projects fallback displays slug + role.
  const seedRow = container.querySelector<HTMLButtonElement>('[data-record-id="inv_seed"]');
  assert(seedRow, "Seed invite row missing");
  await act(async () => {
    seedRow.click();
  });
  await settle(() => {
    const section = document.querySelector('[aria-label="Initial projects"]');
    assert(section, "Initial projects section should appear in InviteDrawer");
    assert(section.textContent?.includes("archived-slug"), "Should fall back to project slug");
    assert(section.textContent?.includes("Viewer"), "Should show Viewer role in InviteDrawer");
  });
  await act(async () => {
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 300));
  });
  await settle(() => {
    assert(!document.querySelector('[aria-label="Initial projects"]'), "InviteDrawer should close");
  });

  // 1) Sending an invite with 0 projects omits project_grants from request payload.
  const inviteBtn = Array.from(container.querySelectorAll<HTMLButtonElement>("button")).find(
    (b) => b.textContent?.includes("Invite"),
  );
  assert(inviteBtn, "Invite button missing");
  await act(async () => {
    inviteBtn.click();
  });
  await settle(() => {
    assert(document.querySelector('input[type="email"]'), "NewInviteDrawer should open");
  });

  const emailInput1 = document.querySelector<HTMLInputElement>('input[type="email"]')!;
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
  await act(async () => {
    setter.call(emailInput1, "plain@example.test");
    emailInput1.dispatchEvent(new Event("input", { bubbles: true }));
  });
  const sendBtn1 = Array.from(document.querySelectorAll<HTMLButtonElement>("button")).find(
    (b) => b.textContent?.trim() === "Send invite",
  )!;
  await act(async () => {
    sendBtn1.click();
  });
  await settle(() => {
    assert(postedBodies.length === 1, "Expected 1st POST /api/team/invites");
  });
  assert(
    postedBodies[0]!.email === "plain@example.test" && !("project_grants" in postedBodies[0]!),
    `Expected 0-project invite to omit project_grants, got ${JSON.stringify(postedBodies[0])}`,
  );

  // 2) Open drawer, verify Workspace Owner calm note, add 2 projects, change role, remove 1, test validation_failed, then submit.
  await act(async () => {
    inviteBtn.click();
  });
  await settle(() => {
    assert(document.querySelector('input[type="email"]'), "NewInviteDrawer should reopen");
  });

  // Switch workspace role to Owner and check calm note
  const wsRoleTrigger = document.querySelector<HTMLButtonElement>('button[aria-label="Role"]')!;
  await selectValue(wsRoleTrigger, "Owner: also manages members and settings");
  await settle(() => {
    assert(
      document.body.textContent?.includes("Workspace Owners already have full access to all projects."),
      "Expected calm note when Workspace Owner role selected",
    );
  });
  await selectValue(wsRoleTrigger, "Member: works on the board");

  // Add Alpha Launch as Project Owner
  await selectValue(document.querySelector<HTMLButtonElement>('button[aria-label="Project"]')!, "Alpha Launch");
  await selectValue(
    document.querySelector<HTMLButtonElement>('button[aria-label="Project role"]')!,
    "Project Owner",
  );
  const addBtn = Array.from(document.querySelectorAll<HTMLButtonElement>("button")).find(
    (b) => b.textContent?.trim() === "Add",
  )!;
  await act(async () => {
    addBtn.click();
  });

  // Add Documentation as Viewer
  await selectValue(
    document.querySelector<HTMLButtonElement>('button[aria-label="Project"]')!,
    "Documentation",
  );
  await selectValue(document.querySelector<HTMLButtonElement>('button[aria-label="Project role"]')!, "Viewer");
  await act(async () => {
    addBtn.click();
  });

  // Change Documentation role inline to Member, and add+remove Operations
  await selectValue(
    document.querySelector<HTMLButtonElement>('button[aria-label="Role for Documentation"]')!,
    "Member",
  );
  await selectValue(document.querySelector<HTMLButtonElement>('button[aria-label="Project"]')!, "Operations");
  await act(async () => {
    addBtn.click();
  });
  const removeOps = document.querySelector<HTMLButtonElement>('button[aria-label="Remove Operations"]')!;
  await act(async () => {
    removeOps.click();
  });

  // Enter email and test server validation_failed keeps drawer open with inline alert
  const emailInput2 = document.querySelector<HTMLInputElement>('input[type="email"]')!;
  await act(async () => {
    setter.call(emailInput2, "scoped@example.test");
    emailInput2.dispatchEvent(new Event("input", { bubbles: true }));
  });
  failValidationOnce = true;
  const sendBtn2 = Array.from(document.querySelectorAll<HTMLButtonElement>("button")).find(
    (b) => b.textContent?.trim() === "Send invite",
  )!;
  await act(async () => {
    sendBtn2.click();
  });
  await settle(() => {
    assert(
      document.body.textContent?.includes("One or more selected projects are no longer available."),
      "validation_failed with project grants should render alert and keep drawer open",
    );
    assert(document.querySelector('input[type="email"]'), "Drawer should stay open on validation_failed");
  });

  // Resubmit cleanly
  await act(async () => {
    sendBtn2.click();
  });
  await settle(() => {
    assert(postedBodies.length === 3, "Expected retry POST to succeed");
    assert(container.textContent?.includes("scoped@example.test"), "Pending invite row should render");
    assert(container.textContent?.includes("Alpha Launch"), "Pending invite chip should display Alpha Launch");
    assert(container.textContent?.includes("Documentation"), "Pending invite chip should display Documentation");
  });

  const grants = postedBodies[2]!.project_grants as Array<{ project: string; role: string }>;
  assert(
    grants.length === 2 &&
      grants[0]!.project === "alpha" &&
      grants[0]!.role === "owner" &&
      grants[1]!.project === "docs" &&
      grants[1]!.role === "member",
    `Unexpected project_grants payload: ${JSON.stringify(grants)}`,
  );

  // 3) Check zh-TW locale rendering
  localStorage.setItem("cadence-locale", "zh-TW");
  await act(async () => {
    root.render(React.createElement("div"));
  });
  await act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(Members, {
          viewer: { operator: true, readOnly: false },
          projects,
        }),
      ),
    );
  });
  await settle(() => {
    assert(container.textContent?.includes("成員"), "Members heading should render in zh-TW");
  });
  const inviteBtnZh = Array.from(container.querySelectorAll<HTMLButtonElement>("button")).find(
    (b) => b.textContent?.includes("邀請"),
  )!;
  assert(inviteBtnZh, "Expected zh-TW Invite button");
  await act(async () => {
    inviteBtnZh.click();
  });
  await settle(() => {
    assert(
      document.body.textContent?.includes("初始專案存取權限（選填）"),
      "zh-TW translation for Initial project access should render",
    );
  });

  localStorage.removeItem("cadence-locale");
  await act(async () => {
    root.unmount();
  });
  console.log("CAD-2888 membersInviteProjects tests passed.");
}

main().catch((err) => {
  console.error(err);
  throw err;
});
