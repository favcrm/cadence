export {};
/**
 * Apps favorites persistence and public-directory navigation against the
 * daemon favorites/home shapes and the public catalogue wire contract.
 */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/apps" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLAnchorElement", "HTMLInputElement", "HTMLSelectElement", "SVGElement", "navigator", "MutationObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module");
const originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof import("../screen-ui/src/navigation");
const { hostNavigation } = require("../src/ui/hostNavigation") as typeof import("../src/ui/hostNavigation");
const AppsComponent = (require("../src/features/apps/Apps") as typeof import("../src/features/apps/Apps")).default;
const Apps = (props: React.ComponentProps<typeof AppsComponent>) =>
  React.createElement(LocaleProvider, null,
    React.createElement(NavigationProvider, { adapter: hostNavigation, children: React.createElement(AppsComponent, props) }));
const ExplorerComponent = (require("../src/features/explorer/Explorer") as typeof import("../src/features/explorer/Explorer")).default;
const Explorer = (props: React.ComponentProps<typeof ExplorerComponent>) =>
  React.createElement(LocaleProvider, null,
    React.createElement(NavigationProvider, { adapter: hostNavigation, children: React.createElement(ExplorerComponent, props) }));

const json = (value: unknown) => new Response(JSON.stringify(value), { status: 200, headers: { "Content-Type": "application/json" } });
// Legacy installation/favorites response shapes from the daemon.
const home = {
  installations: [
    { install_id: "inst-a", name: "crm", title: "CRM", project: null, tagline: "Draft one reviewed email brief", icon: "assets/crm.svg",
      attention: { state: "ok", message: null, action: null, count: 0 } },
    { install_id: "inst-b", name: "social-content", title: "Social Content", project: null, tagline: "Plan posts", icon: null,
      attention: { state: "ok", message: null, action: null, count: 0 } },
  ],
};
let favorites: unknown = { owner: "operator", is_default: false, favorites: [{ install_id: "inst-a", position: 0, opened_at: null }] };
const catalog = { catalog: [{
  id: "crm", source_kind: "builtin", retired: true,
  retired_reason: "This embedded catalogue entry is retired.",
  name: "crm", title: "CRM", trust: "cadence", featured: false, state: "available", request_count: 0,
}] };
// Current public catalogue producer contract, separate from retired tombstones.
const publishedApp = {
  id: "social-content", app: "social-content", title: "Social Content", summary: "Draft posts", version: "0.9.7",
  listing: {
    tagline: "Draft a reviewed caption", category: "marketing", tags: ["social", "instagram"],
    publisher: { name: "Cadence" }, about: "Social Content turns a real source into a reviewed caption.",
    can: ["Pull one public Instagram post"],
    setup: [{ slot: "source", label: "Public Instagram read", help: "Reads public profile." }],
    data: { stores: ["Caption drafts"], personal: false }, access_notes: { source: "Reads public profile." },
  },
  digest: "sha256:" + "ab".repeat(32), installable: true,
};
const writes: { path: string; body: unknown }[] = [];
globalThis.fetch = async (input: any, init?: any) => {
  const path = new URL(String(input), "http://localhost").pathname;
  if (init?.method === "POST") {
    const body: unknown = JSON.parse(String(init.body));
    writes.push({ path, body });
    if (path === "/api/app-favorites") {
      const ids = (body as { install_ids?: unknown }).install_ids;
      if (!Array.isArray(ids) || !ids.every((id) => typeof id === "string"))
        return new Response(JSON.stringify({ error: "install_ids must be strings" }), { status: 400 });
      favorites = { owner: "operator", is_default: false, favorites: ids.map((id, position) => ({ install_id: id, position, opened_at: null })) };
      return json(favorites);
    }
    return json({});
  }
  if (path === "/api/app-home") return json(home);
  if (path === "/api/apps") return json({ apps: [] });
  if (path === "/api/overview") return json({ needs_me: [] });
  if (path === "/api/app-installations") return json([
    { install_id: "inst-a", name: "crm", title: "CRM", removed: null },
    { install_id: "inst-b", name: "social-content", title: "Social Content", removed: null },
  ]);
  if (path === "/api/app-favorites") return json(favorites);
  if (path === "/api/app-catalog") return json(catalog);
  if (path === "/api/app-requests") return json({ requests: [] });
  if (path === "/api/public-apps") return json({ ok: true, data: { apps: [publishedApp] } });
  if (path === "/api/public-apps/social-content") return json({ ok: true, data: { app: publishedApp } });
  if (path === "/api/public-apps/social-content/versions") return json({
    app_id: publishedApp.id, app: publishedApp.app, recommended_version: publishedApp.version,
    releases: [{ version: publishedApp.version, digest: publishedApp.digest, installable: true, unavailableReason: null, release_notes: null }],
  });
  throw new Error(`Unexpected request ${path}`);
}
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
const operator = { readOnly: false, operator: true };

const failures: string[] = [];
async function check(name: string, body: () => Promise<void>) {
  try {
    await body();
  } catch (error) {
    failures.push(`${name}: ${error instanceof Error ? error.message : String(error)}`);
  }
}
async function main() {
  await React.act(() => root.render(React.createElement(Apps, { viewer: operator })));
  await flush();
  // 1. A favourite the daemon saved is shown in Favorites.
  await check("favourites render", async () => {
    const favSection = host.querySelector("section[aria-label='Favorites']");
    assert(favSection, "Favorites section renders");
    assert(
      favSection.querySelector("a[aria-label='Open CRM']"),
      `the favourite the daemon saved (inst-a) is listed under Favorites; got: ${favSection.textContent}`,
    );
  });
  // 2. An installed card opens the app itself; the settings action opens Manage.
  await check("installed card links", async () => {
    await flush();
    const grid = host.querySelector(".apps-grid");
    assert(grid, "the installed gallery renders");
    const crm = Array.from(grid.querySelectorAll(".app-card"))
      .find((card) => card.textContent?.includes("CRM"));
    assert(crm, `the CRM installed card renders; got: ${grid.textContent}`);
    const open = crm.querySelector("a.app-card-link");
    assert(open instanceof HTMLAnchorElement, "the card's main link renders");
    assert(new URL(open.href).pathname === "/apps/crm",
      `the card opens the app at /apps/<key>, got ${new URL(open.href).pathname}`);
    const settings = crm.querySelector("a.app-card-settings");
    assert(settings instanceof HTMLAnchorElement, "the operator's settings action renders");
    assert(new URL(settings.href).pathname === "/apps/manage/inst-a",
      `settings opens Manage, got ${new URL(settings.href).pathname}`);
  });
  // 3. Choosing another favorite preserves both it and the existing favorite after reload.
  await check("pin round-trip", async () => {
    const label = Array.from(host.querySelectorAll("label")).find((row) => row.textContent?.trim() === "Social Content");
    const summary = label?.closest("details")?.querySelector("summary");
    const checkbox = label?.querySelector("input[type='checkbox']");
    assert(summary instanceof HTMLElement && checkbox instanceof HTMLInputElement, "the app is available in the favorites chooser");
    await React.act(() => summary.click());
    await React.act(() => checkbox.click());
    await flush();
    await React.act(() => root.render(React.createElement("div")));
    await React.act(() => root.render(React.createElement(Apps, { viewer: operator })));
    await flush();
    const saved = host.querySelector("section[aria-label='Favorites']");
    assert(saved?.querySelector("a[aria-label='Open CRM']") && saved.querySelector("a[aria-label='Open Social Content']"),
      "both the original and newly chosen favorite survive a fresh read");
  });
  // 4. A member opens the app but gets no operator settings action; an
  //    empty project list adds no section of its own.
  await check("member card links", async () => {
    await React.act(() => root.render(React.createElement(Apps, { viewer: { readOnly: false, operator: false } })));
    await flush();
    const grid = host.querySelector(".apps-grid");
    assert(grid, "the installed gallery renders for a member");
    const crm = Array.from(grid.querySelectorAll(".app-card"))
      .find((card) => card.textContent?.includes("CRM"));
    assert(crm, "the CRM card renders for a member");
    const open = crm.querySelector("a.app-card-link");
    assert(open instanceof HTMLAnchorElement && new URL(open.href).pathname === "/apps/crm",
      "a member's card opens the app at /apps/<key>");
    assert(!crm.querySelector("a.app-card-settings"), "a member gets no operator settings action");
    assert(!host.querySelector("section[aria-label='Project apps']"),
      "a confirmed-empty project list adds no Project apps section");
  });
  // 5. A published card is one link to native detail, not an inline install action.
  await check("card opens detail", async () => {
    await React.act(() => root.render(React.createElement(Explorer, { viewer: operator })));
    await flush();
    await flush();
    const card = Array.from(host.querySelectorAll<HTMLAnchorElement>("a[href]"))
      .find((link) => new URL(link.href).pathname === "/apps/catalog/social-content");
    assert(card instanceof HTMLAnchorElement, `the published card is a native detail link; rendered ${host.textContent}`);
    assert(!card.querySelector("button, a, input, select"), "the whole card has no competing interactive descendants");
    await React.act(() => card.click());
    await flush();
    assert(location.pathname === "/apps/catalog/social-content", "following the card stays on the board's native detail route");
    assert(!writes.some((write) => write.path === "/api/app-installations/upload" || write.path.endsWith("/upgrade")),
      "opening a card never applies an installation");
  });
  if (failures.length) throw new Error(failures.join("\n"));
}
const proc = require("process") as { exit(code: number): never };
main().then(() => proc.exit(0), (error) => {
  console.error(error);
  proc.exit(1);
});
