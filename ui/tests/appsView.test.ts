/** Exercise the installed-app screen and its team editor with real API shapes. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/apps/site/blog-post" });
for (const name of [
  "window",
  "document",
  "Node",
  "Element",
  "HTMLElement",
  "HTMLInputElement",
  "HTMLSelectElement",
  "SVGElement",
  "navigator",
  "MutationObserver",
  "Event",
  "MouseEvent",
  "KeyboardEvent",
  "location",
  "history",
]) {
  Object.defineProperty(globalThis, name, {
    value: name === "window" ? win : win[name],
    configurable: true,
    writable: true,
  });
}
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, {
    value: win[name].bind(win),
    configurable: true,
  });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module");
const originalRequire = loader.prototype.require;
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
const AppDetail = (
  require("../src/features/apps/AppDetail") as typeof import("../src/features/apps/AppDetail")
).default;
const Apps = (
  require("../src/features/apps/Apps") as typeof import("../src/features/apps/Apps")
).default;
const { resources } =
  require("../src/lib/resources") as typeof import("../src/lib/resources");
const { navigate } =
  require("../src/lib/useLocation") as typeof import("../src/lib/useLocation");
const app: import("../src/lib/types").AppDetail = {
  project: "site",
  name: "blog-post",
  title: "Blog post",
  approved: false,
  approval: "unapproved",
  connections: [{ slot: "publish", bound: "local" }],
  team: { writer: "w1", reviewer: "r1" },
  workflows: [
    {
      name: "blog-post/write",
      label: "New post",
      ok: true,
      inputs: [
        { name: "topic", ask: "What should the post be about?" },
        { name: "writer", ask: "Agent that writes the post" },
        { name: "reviewer", ask: "Agent that reviews the post" },
      ],
      steps: [
        { title: "Draft", agent: "writer" },
        { title: "Review", agent: "reviewer" },
      ],
      uses: ["publish"],
      distinct: ["writer", "reviewer"],
    },
  ],
};
const campaignApp = {
  ...app,
  project: "campaign",
  team: { writer: "w1", reviewer: "r1" },
};
const writes: { path: string; body: unknown }[] = [];
let failSave = false;
let failRefresh = false;
let finishSave: (() => void) | null = null;
let holdSave = false;
const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
globalThis.fetch = async (input, init) => {
  const url = new URL(String(input), "http://localhost");
  if (init?.method === "POST") {
    const body = JSON.parse(String(init.body));
    writes.push({ path: url.pathname, body });
    if (holdSave)
      await new Promise<void>((resolve) => {
        finishSave = resolve;
      });
    if (failSave) return json({ error: "Team could not be saved" }, 400);
    app.team = Object.fromEntries(
      body.team.map((pair: string) => pair.split("=")),
    );
    return json({ ok: true });
  }
  if (url.pathname.endsWith("/runs")) return json({ runs: [] });
  if (url.pathname.endsWith("/outputs"))
    return json({ items: [], pending: [] });
  if (url.pathname === "/api/agents")
    return json({
      agents: ["w1", "w2", "r1", "r2"].map((alias) => ({
        alias,
        state: "idle",
      })),
      by_issue: {},
    });
  if (url.pathname === "/api/apps/site/blog-post")
    return failRefresh
      ? json({ error: "Refresh unavailable" }, 503)
      : json(app);
  if (url.pathname === "/api/apps/campaign/blog-post") return json(campaignApp);
  if (url.pathname === "/api/apps")
    return json({
      apps: [app, { ...app, project: "campaign" }].map((a) => ({
        ...a,
        workflows: [],
      })),
    });
  // CAD-1129: the Apps home reads the workspace explorer projection,
  // not the legacy /api/apps list.
  if (url.pathname === "/api/app-home")
    return json({
      installations: [
        {
          install_id: "inst-site",
          name: app.name,
          title: app.title,
          project: "site",
          tagline: "Draft one reviewed post",
          icon: null,
          attention: { state: "ok", message: null, action: null, count: 0 },
        },
        {
          install_id: "inst-campaign",
          name: app.name,
          title: app.title,
          project: "campaign",
          tagline: "Draft one reviewed post",
          icon: null,
          attention: { state: "ok", message: null, action: null, count: 0 },
        },
      ],
    });
  if (url.pathname === "/api/app-favorites")
    return json({ owner: "operator", is_default: false, favorites: [] });
  if (url.pathname === "/api/app-requests") return json({ requests: [] });
  throw new Error(`Unexpected request ${url.pathname}`);
};
function assert(value: unknown, what: string): asserts value {
  if (!value) throw new Error(what);
}
const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const flush = async () => {
  await React.act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
};
function button(text: string) {
  const found = Array.from(host.querySelectorAll("button")).find(
    (b) => b.textContent?.trim() === text,
  );
  assert(found, `button ${text} exists`);
  return found;
}
async function render(
  viewer = { readOnly: false, operator: true },
  project = "site",
) {
  await React.act(() =>
    root.render(
      React.createElement(AppDetail, {
        project,
        name: "blog-post",
        viewer,
        onOpenIssue: () => {},
        onHome: () => {},
      }),
    ),
  );
  await flush();
}
async function change(role: string, value: string) {
  const select = host.querySelector(`#team-${role}`);
  assert(select instanceof HTMLSelectElement, `${role} picker exists`);
  await React.act(() => {
    select.value = value;
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
}
async function main() {
  await resources.agents.revalidate();
  await render();
  assert(
    !host.querySelector("[aria-label='needs you']"),
    "installation approval is not duplicated in Needs you",
  );
  assert(
    host.textContent?.includes("Complete setup in Settings"),
    "empty posts points to setup",
  );
  assert(
    button("New post").disabled,
    "installation approval still blocks starting",
  );
  const settings = host.querySelector("a[href$='?tab=settings']");
  assert(settings instanceof HTMLElement, "settings has a real navigable link");
  await React.act(() => settings.click());
  await flush();
  assert(
    location.search === "?tab=settings",
    "setup navigation persists the selected section in the URL",
  );
  assert(
    host.querySelector("nav[aria-label='App sections'] a[aria-current='page']")
      ?.textContent === "Settings",
    "shared section navigation identifies current page",
  );
  assert(
    Array.from(host.querySelectorAll("button")).filter((b) =>
      b.textContent?.includes("Approve app"),
    ).length === 1,
    "one installation approval action in Settings",
  );
  assert(button("Save team").disabled, "unchanged saved team cannot save");
  await change("writer", "w2");
  assert(
    !button("Save team").disabled,
    "choosing another agent enables Save team",
  );
  await change("writer", "w1");
  assert(
    button("Save team").disabled,
    "reverting to the saved assignment clears the edit",
  );
  await change("writer", "w2");
  holdSave = true;
  await React.act(() => button("Save team").click());
  await flush();
  assert(button("Saving…").disabled, "saving is disabled while pending");
  assert(
    host.querySelector<HTMLSelectElement>("#team-writer")?.disabled,
    "pending save freezes its assignments",
  );
  assert(
    writes.length === 1 && writes[0].path === "/api/apps/site/blog-post/team",
    "save preserves the exact installation route",
  );
  assert(
    JSON.stringify(writes[0].body) ===
      JSON.stringify({ team: ["writer=w2", "reviewer=r1"] }),
    "save sends the intended team",
  );
  assert(finishSave, "save reached the pending request");
  failRefresh = true;
  await React.act(async () => finishSave!());
  await flush();
  holdSave = false;
  assert(
    button("Save team").disabled &&
      host.querySelector<HTMLSelectElement>("#team-writer")?.value === "w2",
    "confirmed save retains the new team even if the refresh fails",
  );
  assert(
    host.querySelector("[role='status']")?.textContent === "Team saved.",
    "save success is announced",
  );
  failRefresh = false;
  failSave = true;
  await change("reviewer", "r2");
  await React.act(() => button("Save team").click());
  await flush();
  assert(
    host.querySelector("[role='alert']")?.textContent ===
      "Team could not be saved",
    "save failure is visible and announced",
  );
  assert(
    !button("Save team").disabled &&
      host.querySelector<HTMLSelectElement>("#team-reviewer")?.value === "r2",
    "failed save retains the draft for retry",
  );
  failSave = false;
  await render({ readOnly: true, operator: true });
  assert(
    button("Save team").disabled &&
      host.querySelector<HTMLSelectElement>("#team-reviewer")?.disabled,
    "a read-only board disables team writes even for an operator",
  );
  await React.act(() => button("Save team").click());
  await React.act(() => button("Add worker").click());
  await flush();
  assert(
    [...writes].length === 2,
    "read-only controls never send the retained draft",
  );
  await resources.app("campaign/blog-post").revalidate();
  await React.act(() => navigate("/apps/campaign/blog-post?tab=settings"));
  await render({ readOnly: false, operator: true }, "campaign");
  assert(
    host.querySelector<HTMLSelectElement>("#team-reviewer")?.value === "r1" &&
      button("Save team").disabled,
    "switching to a cached installation resets the previous team's unsaved draft",
  );
  await React.act(() => navigate("/apps/site/blog-post"));
  app.approved = true;
  app.approval = "approved";
  await React.act(async () => {
    await resources.app("site/blog-post").invalidate();
  });
  await flush();
  await render();
  assert(
    !host.querySelector("[aria-label='setup required']") &&
      !button("New post").disabled,
    "ready app removes setup without changing its readiness rule",
  );
  await render({ readOnly: true, operator: true });
  assert(
    host.textContent?.includes(
      "New posts cannot be started on this read-only board.",
    ) && !host.textContent?.includes("Sign in as the operator"),
    "already signed-in read-only operators get accurate board-access copy",
  );
  await render({ readOnly: true, operator: false });
  assert(
    host.textContent?.includes("Sign in as the operator to start a post."),
    "ready read-only empty state explains access",
  );
  await React.act(() => navigate("/apps/site/blog-post?tab=settings"));
  assert(
    button("Save team").disabled &&
      host.querySelector<HTMLSelectElement>("#team-writer")?.disabled,
    "read-only team controls are disabled",
  );
  assert(
    [...writes].length === 2,
    "reading and navigation do not write or approve",
  );
  await React.act(() =>
    root.render(
      React.createElement(Apps, {
        project: "all",
        viewer: { readOnly: true, operator: false },
      }),
    ),
  );
  await flush();
  assert(
    host.textContent?.includes("Project: site") &&
      host.textContent?.includes("Project: campaign"),
    "same-name installations show their project associations",
  );
  // CAD-1129: the home opens each installation's own page — distinct
  // install ids, no shared route.
  const instLinks = Array.from(host.querySelectorAll("a")).filter((a) =>
    a.getAttribute("href")?.startsWith("/app-installations/"),
  );
  assert(
    instLinks.length === 2 &&
      new Set(instLinks.map((a) => a.getAttribute("href"))).size === 2 &&
      instLinks.some((a) => a.getAttribute("href") === "/app-installations/inst-site") &&
      instLinks.some((a) => a.getAttribute("href") === "/app-installations/inst-campaign"),
    "home preserves separate installation routes by install id",
  );
  assert(
    !host.textContent?.includes("New post"),
    "list action opens the app instead of promising an immediately available run",
  );
  await React.act(() => root.unmount());
  console.log("apps view tests passed");
}
main().catch(async (error) => {
  await React.act(() => root.unmount());
  throw error;
});
export {};
