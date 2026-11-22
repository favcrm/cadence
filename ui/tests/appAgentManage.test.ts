/// <reference lib="es2024.promise" />
import type * as HappyDom from "happy-dom";
import type * as ReactTypes from "react";
import type * as ReactDomClient from "react-dom/client";
import type * as Locale from "../src/lib/locale";
import type * as Manage from "../src/features/explorer/ManageApp";
import type * as Navigation from "../screen-ui/src/navigation";
import type * as HostNavigation from "../src/ui/hostNavigation";
import type * as Consultation from "../src/features/home/AppConsultation";
import type * as PrivateSession from "../src/lib/privateSession";

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
const AppConsultation = (require("../src/features/home/AppConsultation") as typeof Consultation).default;
const { pausePrivateReads, allowPrivateReads, resetPrivateSession } = require("../src/lib/privateSession") as typeof PrivateSession;

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
const consultationPosts: { id: string; body: unknown }[] = [];
const consultationReads: string[] = [];
let acknowledgeConsultation = true;
let consultationRead: "failure" | "missing" | "unconfirmed" | "answer" = "failure";
let consultationReply = "  \n ";
let heldAgentRead: Promise<Response> | null = null;
const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status });

globalThis.fetch = async (input, init) => {
  const url = new URL(String(input), win.location.href).pathname;
  if (url.startsWith(`${path}/agent/consultations/`)) {
    const id = url.split("/").pop()!;
    if (init?.method === "POST") {
      consultationPosts.push({ id, body: JSON.parse(String(init.body)) });
      if (!acknowledgeConsultation) return json({}, 502);
      return json({ message: id, state: "accepted", duplicate: false }, 202);
    }
    consultationReads.push(id);
    if (consultationRead === "failure") return json({}, 503);
    if (consultationRead === "missing") return json({}, 404);
    return consultationRead === "unconfirmed"
      ? json({ message: id, state: "unconfirmed" })
      : json({ message: id, state: "answered", text: consultationReply });
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
      if (heldAgentRead) return heldAgentRead;
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
async function render(session: string | null, operator = true) {
  await React.act(async () => {
    root.render(React.createElement(LocaleProvider, null,
      React.createElement(NavigationProvider, { adapter: hostNavigation, children: React.createElement(ManageApp, {
        installId, viewer: { operator, readOnly: !operator }, automaticUpdatesEnabled: false, appAgentSession: session,
      }) })));
  });
}
const change = (el: HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement | null | undefined, value: string) => React.act(async () => {
  assert(el, "The input is available");
  Object.getOwnPropertyDescriptor(Object.getPrototypeOf(el), "value")!.set!.call(el, value);
  el.dispatchEvent(new Event(el.tagName === "SELECT" ? "change" : "input", { bubbles: true }));
});
const click = (button: HTMLButtonElement | null | undefined) => React.act(async () => {
  assert(button && !button.disabled, "The action is available");
  button.click();
});
const updateButton = (label: string) => [...host.querySelectorAll("button")].find(button => button.textContent?.trim() === label);
async function renderConsultation(session: string | null) {
  await React.act(async () => {
    root.render(React.createElement(LocaleProvider, null, session
      ? React.createElement(AppConsultation, { key: session, session, onCompose: (text: string) => { composed = text; } })
      : null));
  });
}
let composed = "";

async function main() {
  try {
    await render("owner-a");
    assert(selector()?.textContent?.includes("Current agent: Bookkeeper"), "Screen-only Accounting exposes the current role in Manage");
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
    owner = "owner-a";
    owners.set(owner, { revision: 4, digest: install.digest, profile: keeper });
    await renderConsultation("owner-a");
    await click(updateButton("Consult app agent"));
    await change(host.querySelector("select"), installId);
    await change(host.querySelector("textarea"), "exact source");
    await React.act(async () => { pausePrivateReads(); allowPrivateReads(); });
    const { promise, resolve: settleAgentRead } = Promise.withResolvers<Response>();
    heldAgentRead = promise;
    await click(updateButton("Consult app agent"));
    assert(updateButton("Send to app agent")?.disabled, "An interrupted draft waits for a fresh recipient");
    assert(host.querySelector<HTMLTextAreaElement>("textarea")?.value === "exact source", "Verification interruption retains editable text");
    await React.act(async () => settleAgentRead(json({}, 503)));
    heldAgentRead = null;
    assert(updateButton("Send to app agent")?.disabled && consultationPosts.length === 0, "A failed recipient read cannot send");
    await click(updateButton("Close"));
    await click(updateButton("Consult app agent"));
    await change(host.querySelector("select"), installId);
    await change(host.querySelector("textarea"), "exact source");
    acknowledgeConsultation = false;
    await click(updateButton("Send to app agent"));
    const first = consultationPosts[0];
    assert(first && Number(consultationPosts.length) === 1, "The first POST was sent with a lost acknowledgement");
    await renderConsultation(null);
    await renderConsultation("owner-a");
    await click(updateButton("Review consultation"));
    assert(!updateButton("Retry same message"), "Retry is not available before checking the original operation");
    consultationRead = "missing";
    await click(updateButton("Check reply"));
    assert(consultationReads[0] === first.id && Number(consultationPosts.length) === 1, "Recovery reads the original ID without posting");
    acknowledgeConsultation = true;
    await click(updateButton("Retry same message"));
    assert(JSON.stringify(consultationPosts[1]) === JSON.stringify(first), "The explicit retry keeps the original ID, binding and text");
    consultationRead = "failure";
    await click(updateButton("Check reply"));
    assert(host.textContent?.includes("Could not check for the reply"), "A failed read preserves admission");
    await React.act(async () => { pausePrivateReads(); allowPrivateReads(); });
    await click(updateButton("Review consultation"));
    consultationRead = "missing";
    await click(updateButton("Check reply"));
    assert(!updateButton("Retry same message") && Number(consultationPosts.length) === 2, "A 404 after admission never offers a retry");
    await click(updateButton("Start another"));
    await change(host.querySelector("select"), installId);
    await change(host.querySelector("textarea"), "another source");
    await click(updateButton("Send to app agent"));
    consultationRead = "unconfirmed";
    await click(updateButton("Check reply"));
    await click(updateButton("Check reply"));
    assert(!updateButton("Retry same message"), "An admitted consultation is never retried on an unconfirmed reply");
    await click(updateButton("Dismiss review"));
    assert(updateButton("Consult app agent") && Number(consultationPosts.length) === 3, "Dismissal clears local review without sending");
    await click(updateButton("Consult app agent"));
    await change(host.querySelector("select"), installId);
    await change(host.querySelector("textarea"), "third source");
    await click(updateButton("Send to app agent"));
    consultationRead = "answer";
    await click(updateButton("Check reply"));
    const replyRegion = () => host.querySelector<HTMLElement>('.wa-field > pre[role="region"][aria-labelledby]');
    const replyText = () => replyRegion()?.textContent;
    assert(replyText() === "  \n ", "Whitespace-only reply is literal");
    assert(replyRegion()?.tabIndex === 0 && !host.querySelector("details"), "Readable reply is focusable and omits escaped toggle without CR");
    await click(updateButton("Review in Master composer"));
    assert(composed.includes(JSON.stringify("  \n ")), "Master evidence preserves the reply");

    await click(updateButton("Consult app agent"));
    await change(host.querySelector("select"), installId);
    await change(host.querySelector("textarea"), "fourth source");
    await click(updateButton("Send to app agent"));
    consultationReply = "A\r\nB\rC";
    await click(updateButton("Check reply"));
    assert(replyText() === "A\nB\nC", "Readable reply normalizes CR and CRLF to line breaks");
    const replyLabelId = replyRegion()?.getAttribute("aria-labelledby");
    assert(replyLabelId && document.getElementById(replyLabelId)?.textContent === "Exact agent reply", "Readable reply region is labeled");
    const escaped = host.querySelector<HTMLElement>("details pre");
    assert(escaped?.textContent === JSON.stringify(consultationReply) && JSON.parse(escaped.textContent) === consultationReply, "Original line endings are selectable as escaped text");
    assert(escaped.getAttribute("role") === "region" && escaped.tabIndex === 0, "Escaped reply region is keyboard-focusable");
    const escapedLabelId = escaped.getAttribute("aria-labelledby");
    assert(escapedLabelId && document.getElementById(escapedLabelId)?.textContent === "Show exact line endings", "Escaped reply region is labeled by its summary");
    await click(updateButton("Review in Master composer"));
    const composedEvidence = JSON.parse(composed.slice(composed.indexOf("{"))) as { reply: string };
    assert(composedEvidence.reply === consultationReply, "Master evidence preserves untouched CR and CRLF");

    await click(updateButton("Consult app agent"));
    await change(host.querySelector("select"), installId);
    await change(host.querySelector("textarea"), "fifth source");
    await click(updateButton("Send to app agent"));
    await click(updateButton("Check reply"));
    assert(replyText() === "A\nB\nC", "Retained reply is present before session reset");
    await React.act(async () => resetPrivateSession());
    assert(!updateButton("Review consultation") && replyText() === undefined, "Session reset erases a retained private reply");
  } finally {
    await React.act(async () => { root.unmount(); });
    await win.happyDOM.close();
  }
  console.log("Private app agent Manage checks passed");
}
void main();
