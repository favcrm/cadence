/** CAD-2826: independent mounted consumer checks. The HTTP responses below are
 * synthetic; receipt fields come from the shared, shipped host contract. */
import type * as HappyDom from "happy-dom";
import type * as ReactTypes from "react";
import type * as ReactDomClient from "react-dom/client";
import type * as Locale from "../src/lib/locale";
import type * as Manage from "../src/features/explorer/ManageApp";
import type * as Navigation from "../screen-ui/src/navigation";
import type * as HostNavigation from "../src/ui/hostNavigation";
import type * as WorkspaceApps from "../src/features/workspace-apps/workspaceApps";

declare function require(name: string): unknown;
const { Window } = require("happy-dom") as typeof HappyDom;
const win = new Window({ url: "https://tenant.example.test/apps/manage/install_books" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "SVGElement", "navigator", "MutationObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage", "localStorage"] as const)
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"] as const)
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module") as { prototype: { require: (this: unknown, id: string) => unknown } };
const originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.call(this, id);
};
const React = require("react") as typeof ReactTypes;
const { createRoot } = require("react-dom/client") as typeof ReactDomClient;
const { LocaleProvider } = require("../src/lib/locale") as typeof Locale;
const ManageApp = (require("../src/features/explorer/ManageApp") as typeof Manage).default;
const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof Navigation;
const { hostNavigation } = require("../src/ui/hostNavigation") as typeof HostNavigation;
const { readAppHomeFresh, readAppHomeSnapshot, readInstallationsGuarded, readInstallationsSnapshot } = require("../src/features/workspace-apps/workspaceApps") as typeof WorkspaceApps;
const fixture = JSON.parse((require("node:fs") as { readFileSync(path: string, encoding: string): string }).readFileSync("tests/fixtures/hosted-private-app-removal.json", "utf8")) as {
  remove_receipt: Record<string, unknown>; restore_receipt: Record<string, unknown>; restore_expired: Record<string, unknown>;
};
const path = "/api/app-installations/install_books";
const base = { install_id: "install_books", name: "books", title: "Books", version: "1.0.0", summary: "", state: "active", executable: true, removed_at: null, restore_until: null, approved: true, digest: `sha256:${"a".repeat(64)}`, catalog_generation: "a", files: [], connection_slots: [], capabilities: null, storage_kind: "workspace", source_kind: "private", removed: null };
let source: string | undefined = "private";
let writes: string[] = [];
let replyLost = false;
let readUnavailable = false;
let removed = false;
let expireOnRestore = false;
let failRefreshAfterRestore = false;
let refreshUnavailable = false;
let failedRefreshes = 0;
let removedReads = 0;
let changedBundleDuringRestore = false;
let replacementDigest: string | undefined;
const removedAt = Date.now() - 1000;
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
globalThis.fetch = async (input, init) => {
  const url = new URL(String(input), win.location.href).pathname;
  if (init?.method === "POST") {
    writes.push(url);
    if (replyLost) {
      readUnavailable = true;
      return json({}, 503);
    }
    if (url.endsWith("/restore")) {
      if (expireOnRestore) return json(fixture.restore_expired, 409);
      if (changedBundleDuringRestore) {
        removed = false;
        replacementDigest = `sha256:${"b".repeat(64)}`;
        return json({}, 503);
      }
      if (failRefreshAfterRestore) refreshUnavailable = true;
      return json({ ...fixture.restore_receipt, digest: base.digest, installation: { ...base, ...(fixture.restore_receipt.installation as object) } });
    }
    return json(fixture.remove_receipt);
  }
  const install = removed
    ? { ...base, source_kind: source, state: "removed", executable: false, removed_at: removedAt, restore_until: removedAt + 30 * 24 * 60 * 60 * 1000 }
    : { ...base, source_kind: source, digest: replacementDigest ?? base.digest };
  if (url === path) {
    if (removed && !readUnavailable) removedReads += 1;
    return readUnavailable ? json({}, 503) : json(install);
  }
  if (refreshUnavailable && (url === "/api/app-installations" || url === "/api/app-home")) {
    failedRefreshes += 1;
    return json({}, 503);
  }
  if (url === "/api/app-installations") return json([install]);
  if (url === "/api/app-home") return json({ installations: [{ ...install, attention: removed
    ? { state: "removed", message: "Removed", action: "restore", count: 0 }
    : { state: "ok", message: null, action: null, count: 0 } }] });
  return json({}, 404);
};
function assert(value: unknown, why: string): asserts value { if (!value) throw new Error(`hosted private removal: ${why}`); }
const host = document.createElement("div"); document.body.append(host);
const button = (text: string) => [...host.querySelectorAll("button")].find(el => el.textContent?.trim() === text);
async function scenario(options: { capability?: boolean; operator?: boolean | null; readOnly?: boolean; kind?: string }) {
  source = options.kind; writes = [];
  const root = createRoot(host);
  await React.act(async () => {
    root.render(React.createElement(LocaleProvider, null,
      React.createElement(NavigationProvider, { adapter: hostNavigation, children: React.createElement(ManageApp, {
        installId: "install_books", viewer: { operator: options.operator === undefined ? true : options.operator, readOnly: options.readOnly ?? false },
        automaticUpdatesEnabled: false, appAgentSession: null, hosted: true, hostedRemovalEnabled: options.capability,
      }) })));
  });
  return root;
}
async function main() {
  try {
    for (const options of [
      { capability: false, kind: "private" },
      { capability: true, kind: "public" },
      { capability: true, kind: undefined },
      { kind: "private" },
      { capability: true, kind: "private", operator: false },
      { capability: true, kind: "private", operator: null },
      { capability: true, kind: "private", readOnly: true },
    ]) {
      const root = await scenario(options);
      assert(!button("Remove app…"), `no removal control for ${JSON.stringify(options)}`);
      assert(writes.length === 0, "denied rendering must not mutate");
      await React.act(async () => { root.unmount(); });
    }
    const root = await scenario({ capability: true, kind: "private" });
    await readAppHomeFresh(undefined, "operator");
    assert(readAppHomeSnapshot("operator")?.[0]?.attention.state === "ok", "warm home starts with the installation live");
    assert(button("Remove app…"), "an eligible owner can open removal");
    await React.act(async () => { button("Remove app…")!.click(); });
    assert(writes.length === 0, "opening confirmation does not mutate");
    assert(button("Cancel"), "confirmation offers cancel");
    await React.act(async () => { button("Cancel")!.click(); });
    assert(writes.length === 0, "cancel does not mutate");
    await React.act(async () => { button("Remove app…")!.click(); });
    assert(button("Remove Books"), "confirmation names the private installation");
    await React.act(async () => { button("Remove Books")!.click(); });
    assert(Number(writes.length) === 1 && writes[0] === `${path}/remove`, "confirmation sends exactly one hosted remove request");
    assert(host.textContent?.includes("Removed — its data is retained."), "a validated receipt confirms removal");
    assert(readAppHomeSnapshot("operator")?.[0]?.attention.state === "removed", "confirmed removal updates the retained home before the next read");
    assert(readInstallationsSnapshot()?.[0]?.state === "removed", "confirmed removal updates the retained launcher list before the next read");
    await React.act(async () => { root.unmount(); });

    removed = true;
    const restored = await scenario({ capability: true, kind: "private" });
    await readAppHomeFresh(undefined, "operator");
    assert(readAppHomeSnapshot("operator")?.[0]?.attention.state === "removed", "warm home starts removed");
    assert(readInstallationsSnapshot()?.[0]?.state === "removed", "warm launcher starts removed");
    failRefreshAfterRestore = true;
    assert(button("Restore"), "a private installation within its window can be restored");
    await React.act(async () => { button("Restore")!.click(); });
    assert(Number(writes.length) === 1 && writes[0] === `${path}/restore`, "restore sends exactly one hosted request");
    assert(host.textContent?.includes("Restored — connection grants must be set up again."), "the validated restore receipt is confirmed");
    await Promise.allSettled([readAppHomeFresh(undefined, "operator"), readInstallationsGuarded()]);
    assert(failedRefreshes > 0, "a launcher or home refresh actually failed after confirmation");
    assert(document.activeElement?.textContent?.includes("Restored — connection grants"), "focus moves to the restore result");
    assert(readAppHomeSnapshot("operator")?.[0]?.install_id === base.install_id && readAppHomeSnapshot("operator")?.[0]?.attention.state === "ok", "restored home retains identity despite failed refresh");
    assert(readInstallationsSnapshot()?.[0]?.install_id === base.install_id && readInstallationsSnapshot()?.[0]?.digest === base.digest && readInstallationsSnapshot()?.[0]?.state === "active", "restored launcher retains identity and digest despite failed refresh");
    await React.act(async () => { restored.unmount(); });
    failRefreshAfterRestore = false;
    refreshUnavailable = false;

    const expired = await scenario({ capability: true, kind: "private" });
    expireOnRestore = true;
    const readsBeforeExpiry = removedReads;
    assert(button("Restore"), "the pre-write read is still inside the restore window");
    await React.act(async () => { button("Restore")!.click(); });
    assert(Number(writes.length) === 1 && writes[0] === `${path}/restore`, "server expiry is not retried");
    assert(removedReads > readsBeforeExpiry, "the server refused after a fresh in-window detail read");
    assert(host.textContent?.includes("The restore window has closed.") && !button("Restore"), "server 409 closes restore without claiming success");
    assert(!host.textContent?.includes("Restored — connection grants"), "expiry does not confirm restore");
    assert(document.activeElement?.textContent?.includes("The restore window has closed."), "focus moves to the expiry result");
    await React.act(async () => { expired.unmount(); });
    expireOnRestore = false;

    const replaced = await scenario({ capability: true, kind: "private" });
    changedBundleDuringRestore = true;
    await React.act(async () => { button("Restore")!.click(); });
    assert(Number(writes.length) === 1, "an uncertain restore of a replaced bundle is not replayed");
    assert(host.textContent?.includes("The requested change was not confirmed."), "active readback at another digest does not confirm the requested restore");
    assert(!host.textContent?.includes("Restored — connection grants"), "a different installed digest is not a restore success");
    await React.act(async () => { replaced.unmount(); });
    changedBundleDuringRestore = false;
    replacementDigest = undefined;
    removed = false;

    const uncertain = await scenario({ capability: true, kind: "private" });
    replyLost = true;
    await React.act(async () => { button("Remove app…")!.click(); });
    await React.act(async () => { button("Remove Books")!.click(); });
    assert(button("Check again") && !button("Remove app…"), "an unconfirmed result offers a read, not a repeated write");
    assert(!host.textContent?.includes("Off — the app is removed."), "a missing receipt does not report removal");
    readUnavailable = false;
    await React.act(async () => { button("Check again")!.click(); });
    assert(Number(writes.length) === 1 && !!button("Remove app…"), "authoritative active readback unlocks without replaying remove");
    assert(!host.textContent?.includes("Restored — connection grants"), "unchanged state is not a restore success");
    await React.act(async () => { uncertain.unmount(); });
  } finally { await win.happyDOM.close(); }
  console.log("Hosted private app removal guard checks passed");
}
void main();
