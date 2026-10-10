export {};
/**
 * CAD-1129 reviewer acceptance (cc13-sonnet-spec794): the Apps home and the
 * Explorer against the JSON the daemon really sends (captured from
 * `app_favorites_get` / `app_catalog_list` / `app_home`), not a mock shaped
 * like the client's own types.
 */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/apps" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "SVGElement", "navigator", "MutationObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history"])
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
const AppsComponent = (require("../src/features/apps/Apps") as typeof import("../src/features/apps/Apps")).default;
const Apps = (props: React.ComponentProps<typeof AppsComponent>) =>
  React.createElement(LocaleProvider, null, React.createElement(AppsComponent, props));
const ExplorerComponent = (require("../src/features/explorer/Explorer") as typeof import("../src/features/explorer/Explorer")).default;
const Explorer = (props: React.ComponentProps<typeof ExplorerComponent>) =>
  React.createElement(LocaleProvider, null, React.createElement(ExplorerComponent, props));

const json = (value: unknown) => new Response(JSON.stringify(value), { status: 200, headers: { "Content-Type": "application/json" } });
// Shapes copied from the daemon: `app_home`, `app_favorites_get`, `app_catalog_list`.
const home = {
  installations: [
    { install_id: "inst-a", name: "crm", title: "CRM", project: null, tagline: "Draft one reviewed email brief", icon: "assets/crm.svg",
      attention: { state: "ok", message: null, action: null, count: 0 } },
    { install_id: "inst-b", name: "social-content", title: "Social Content", project: null, tagline: "Plan posts", icon: null,
      attention: { state: "ok", message: null, action: null, count: 0 } },
  ],
};
let favorites: unknown = { owner: "operator", is_default: false, favorites: [{ install_id: "inst-a", position: 0, opened_at: null }] };
const catalog = {
  catalog: [
    { id: "crm", source_kind: "builtin", name: "crm", title: "CRM", version: "0.1.0", digest: "sha256:" + "0".repeat(64), trust: "cadence",
      featured: true, state: "available", request_count: 0,
      listing: { tagline: "Draft one reviewed email brief", category: "customers" }, access: [], never: "" },
  ],
};
const writes: { path: string; body: unknown }[] = [];
globalThis.fetch = async (input: any, init?: any) => {
  const path = new URL(String(input), "http://localhost").pathname;
  if (init?.method === "POST") {
    writes.push({ path, body: JSON.parse(String(init.body)) });
    if (path === "/api/app-favorites") return json({ owner: "operator", favorites: [] });
    return json({});
  }
  if (path === "/api/app-home") return json(home);
  if (path === "/api/app-favorites") return json(favorites);
  if (path === "/api/app-catalog") return json(catalog);
  if (path === "/api/app-requests") return json({ requests: [] });
  throw new Error(`Unexpected request ${path}`);
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
  await React.act(() => root.render(React.createElement(Apps, { project: "all", viewer: operator })));
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
  // 2. Pinning another app sends install-id strings, keeping the saved ones.
  await check("pin round-trip", async () => {
    const star = host.querySelector("button[aria-label='Pin Social Content'], button[aria-label='Unpin Social Content']");
    assert(star instanceof HTMLElement, "a pin control exists for the second app");
    await React.act(() => star.click());
    await flush();
    const put = writes.find((w) => w.path === "/api/app-favorites");
    assert(put, "pin writes the favourites");
    const ids = (put.body as { install_ids: unknown[] }).install_ids;
    assert(
      ids.length === 2 && ids.every((id) => typeof id === "string"),
      `the saved favourites are sent back as install-id strings; sent ${JSON.stringify(ids)}`,
    );
  });
  // 3. A catalog card opens its detail page.
  await check("card opens detail", async () => {
    await React.act(() => root.render(React.createElement(Explorer, { viewer: operator })));
    await flush();
    const card = host.querySelector(".ccard");
    assert(card instanceof HTMLElement, "the catalog card renders");
    await React.act(() => card.click());
    await flush();
    assert(
      location.pathname === "/apps/catalog/crm",
      `clicking a catalog card opens /apps/catalog/crm; location is ${location.pathname}${location.hash}`,
    );
  });
  // 4. The featured row shows for the daemon's `featured: true`.
  await check("featured row", async () => {
    await React.act(() => root.render(React.createElement(Explorer, { viewer: operator })));
    await flush();
    assert(host.querySelector(".feat .fcard"), "the daemon's featured entry (featured: true) renders in the Featured row");
  });
  if (failures.length) throw new Error(failures.join("\n"));
}
const proc = require("process") as { exit(code: number): never };
main().then(() => proc.exit(0), (error) => {
  console.error(error);
  proc.exit(1);
});
