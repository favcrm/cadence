import type * as HappyDom from "happy-dom";
import type * as ReactTypes from "react";
import type * as ReactDomClient from "react-dom/client";
import type * as Locale from "../src/lib/locale";
import type * as Manage from "../src/features/explorer/ManageApp";
import type * as Navigation from "../screen-ui/src/navigation";
import type * as HostNavigation from "../src/ui/hostNavigation";

declare function require(name: string): unknown;
const { Window } = require("happy-dom") as typeof HappyDom;
const win = new Window({ url: "http://localhost/apps/manage/install-accounting" });
for (const [name, value] of Object.entries({
  window: win, document: win.document, Node: win.Node, Element: win.Element, HTMLElement: win.HTMLElement,
  HTMLInputElement: win.HTMLInputElement, HTMLSelectElement: win.HTMLSelectElement, navigator: win.navigator,
  Event: win.Event, location: win.location, history: win.history,
})) Object.defineProperty(globalThis, name, { value, configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
// The CJS test runner cannot load this package's ESM-tagged CJS icon entry.
const loader = require("module") as { prototype: { require: (this: unknown, id: string) => unknown } };
const originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
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

const installId = "install-accounting";
const path = `/api/app-installations/${installId}`;
const digestA = "sha256:" + "a".repeat(64);
const digestB = "sha256:" + "b".repeat(64);
let install = {
  install_id: installId, title: "Accounting", name: "accounting", version: "0.5.19", summary: "",
  state: "active", approved: true, digest: digestA, catalog_generation: "a",
  files: ["app.md", "screens/main/screens.json"], connection_slots: [], capabilities: null,
  storage_kind: "workspace",
};
const keeper = { id: "keeper", name: "Bookkeeper", status: "active", revision: 1 };
const reviewer = { id: "reviewer", name: "Accounting Reviewer", status: "active", revision: 1 };
const pin = (profile: typeof keeper) => ({ profile_id: profile.id, profile_revision: profile.revision, profile });
const owners = new Map<string, { revision: number; digest: string; profile: typeof keeper | null }>([
  ["owner-a", { revision: 3, digest: digestA, profile: keeper }],
]);
let owner = "owner-a";
let reads = 0;
let unconfirmed = false;
let conflict = false;
const writes: unknown[] = [];
let removePreviewCalls = 0;
let removeCalls = 0;
const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status });

globalThis.fetch = async (input, init) => {
  const url = new URL(String(input), win.location.href).pathname;
  if (url === `${path}/remove-preview`) {
    removePreviewCalls += 1;
    return json({ install_id: installId, title: "Accounting", generation: "a", digest: digestA });
  }
  if (url === `${path}/remove`) {
    removeCalls += 1;
    return json({ ok: true });
  }
  if (url === path) return json(install);
  if (url === "/api/app-installations") return json([install]);
  if (url === "/api/app-home") return json({ installations: [{ install_id: installId, attention: { state: "ok" } }] });
  if (url === "/api/agent-profiles") return json({ profiles: [keeper, reviewer, { ...keeper, id: "retired", status: "archived" }] });
  if (url === `${path}/upgrade/check`) return json({ name: "accounting", version: "0.5.20", digest: digestB, structural_diff: { added: [], changed: [], removed: [] } });
  if (url === `${path}/upgrade`) {
    install = { ...install, digest: digestB, version: "0.5.20", catalog_generation: "b" };
    return json(install);
  }
  if (url === `${path}/agent`) {
    const state = owners.get(owner);
    const revision = state?.revision ?? 0;
    const stale = !!state && state.digest !== install.digest;
    if (init?.method !== "POST") {
      reads += 1;
      return json({ install_id: installId, digest: install.digest, revision, stale, agent: !stale && state?.profile ? pin(state.profile) : null, consultations: true });
    }
    const body = JSON.parse(String(init.body));
    writes.push(body);
    if (conflict) {
      conflict = false;
      owners.set(owner, { revision: revision + 1, digest: install.digest, profile: null });
      return json({ error: "conflict" }, 409);
    }
    if (body.expected_digest !== install.digest || body.expected_revision !== revision) return json({ error: "conflict" }, 409);
    const profile = [keeper, reviewer].find(row => row.id === body.profile_id) ?? null;
    owners.set(owner, { revision: revision + 1, digest: install.digest, profile });
    return json({ install_id: installId, digest: install.digest, revision: revision + 1, agent: profile ? pin(unconfirmed ? keeper : profile) : null });
  }
  return json({}, 404);
};

const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const selector = () => host.querySelector<HTMLElement>('section[aria-label="Default app agent"]');
const choice = () => selector()?.querySelector("select");
const save = () => selector()?.querySelector<HTMLButtonElement>('button[type="submit"]');
function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}
async function render(session: string | null, operator = true, hosted = false) {
  await React.act(async () => {
    root.render(React.createElement(LocaleProvider, null,
      React.createElement(NavigationProvider, { adapter: hostNavigation, children: React.createElement(ManageApp, {
        installId, viewer: { operator, readOnly: !operator }, automaticUpdatesEnabled: false, appAgentSession: session, hosted,
      }) })));
  });
}
const change = (el: HTMLInputElement | HTMLSelectElement | null | undefined, value: string) => React.act(async () => {
  assert(el, "The input is available");
  Object.getOwnPropertyDescriptor(Object.getPrototypeOf(el), "value")!.set!.call(el, value);
  el.dispatchEvent(new Event(el.tagName === "SELECT" ? "change" : "input", { bubbles: true }));
});
const click = (button: HTMLButtonElement | null | undefined) => React.act(async () => {
  assert(button && !button.disabled, "The action is available");
  button.click();
});
const updateButton = (label: string) => [...host.querySelectorAll("button")].find(button => button.textContent?.trim() === label);

async function main() {
  try {
    await render("owner-a");
    assert(selector()?.textContent?.includes("Current agent: Bookkeeper"), "Screen-only Accounting exposes the current role in Manage");
    assert(Boolean(updateButton("Remove app…")), "Native Manage shows Remove app button");
    await render("owner-a", true, true);
    assert(!updateButton("Remove app…"), "Hosted Manage hides the unsupported Remove app button");
    assert(host.textContent?.includes("Removing installed apps from a hosted workspace is not available yet. You can revoke tool access above."), "Hosted Manage explains removal is unavailable and points to revoking access above");
    assert(removePreviewCalls === 0 && removeCalls === 0, "Hosted Manage never calls remove-preview or remove");
    await render("owner-a", true, false);
    assert(!choice()?.querySelector('option[value="retired"]'), "Archived profiles are not offered");
    await change(choice(), "reviewer");
    await change(host.querySelector<HTMLInputElement>(".update-source"), "https://example.test/accounting");
    await click(updateButton("Check update"));
    await click(updateButton("Apply checked update"));
    assert(selector()?.textContent?.includes("The previous selection is no longer current"), "A package update refreshes and invalidates the displayed role");
    assert(choice()?.value === "reviewer" && writes.length === 0, "The unsaved choice survives the upgrade without being sent");

    conflict = true;
    await click(save());
    assert(selector()?.textContent?.includes("Review the current setting before saving again") && choice()?.value === "reviewer", "A CAS refusal refreshes the current setting without discarding the draft");
    assert(JSON.stringify(writes) === JSON.stringify([{ expected_digest: digestB, expected_revision: 3, profile_id: "reviewer" }]), "Only the explicit save writes, against the refreshed digest and revision");

    unconfirmed = true;
    await click(save());
    assert(selector()?.textContent?.includes("The save could not be confirmed") && selector()?.textContent?.includes("Current agent: Accounting Reviewer"), "A mismatched receipt needs a server readback, not a success notice");
    assert(save()?.disabled && Number(writes.length) === 2, "A committed but unconfirmed save is not resent");

    await change(choice(), "keeper");
    owner = "owner-b";
    await render("owner-b");
    assert(selector()?.textContent?.includes("No default agent is selected.") && !choice()?.value, "A new owner sees neither the previous selection nor their draft");
    const allowedReads = reads;
    await render(null);
    assert(!selector() && reads === allowedReads, "A platform operator without a private session does not read or see the setting");
    await render("member", false);
    assert(!selector() && reads === allowedReads, "Members do not read or see the owner setting");
  } finally {
    await React.act(async () => { root.unmount(); });
    await win.happyDOM.close();
  }
  console.log("Private app agent Manage checks passed");
}
void main();
