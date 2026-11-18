/**
 * CAD-1819: a hosted board must never fall back to the local-operator identity.
 *
 * The gate paints before /api/meta answers, and a failed check may never
 * answer. On a hosted origin every one of those states must show hosted copy —
 * a retry and a real sign-in — never "Private board" nor the `cadence ui login`
 * instruction, which a hosted member cannot run.
 */
import type * as HappyDom from "happy-dom";
import type * as ReactNs from "react";
import type * as ReactDomClient from "react-dom/client";
import type * as LocaleModule from "../src/lib/locale";
import type AuthBoundaryType from "../src/features/auth/AuthBoundary";
import type AccountMenuType from "../src/ui/AccountMenu";

declare function require(name: string): unknown;
declare const process: { exit(code: number): never };
export {};

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

type ModuleLoader = {
  prototype: { require: (this: unknown, id: string) => unknown };
};

function namedExport(mod: unknown, name: string): unknown {
  assert(mod && typeof mod === "object" && name in mod, `module must export ${name}`);
  return Reflect.get(mod, name);
}

async function main() {
const { Window } = require("happy-dom") as typeof HappyDom;
const win = new Window({ url: "https://fav-limited.cadencecloud.app/issues/12" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"] as const)
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"] as const)
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module") as ModuleLoader;
const originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, [id]);
};
const React = require("react") as typeof ReactNs;
const { createRoot } = require("react-dom/client") as typeof ReactDomClient;
const { LocaleProvider } = require("../src/lib/locale") as typeof LocaleModule;
const AuthBoundary = namedExport(require("../src/features/auth/AuthBoundary"), "default") as typeof AuthBoundaryType;

const delay = (ms: number): Promise<void> => new Promise<void>((resolve) => { setTimeout(resolve, ms); });

// A verification failure, after enough delay for the checking state to paint.
globalThis.fetch = (async () => {
  await delay(40);
  return new Response(JSON.stringify({ ok: false, error: { code: "capability_unavailable", message: "unavailable" } }), {
    status: 503,
    headers: { "Content-Type": "application/json" },
  });
}) as typeof fetch;

const host = document.createElement("div");
document.body.appendChild(host);
const root = createRoot(host);
const flush = () => React.act(async () => { await delay(0); });
await React.act(async () => {
  root.render(React.createElement(LocaleProvider, null,
    React.createElement(AuthBoundary, { children: () => React.createElement("main", null, "PRIVATE WORKSPACE") })));
});

// The checking state already knows this is a hosted board.
await flush();
const checking = host.textContent ?? "";
assert(checking.includes("Hosted workspace"), "checking a hosted board names it hosted");
assert(checking.includes("Verifying session & workspace access"), "checking copy is the hosted one");
assert(!checking.includes("Private board"), "checking a hosted board never says Private board");
assert(!checking.includes("cadence ui login"), "checking a hosted board never shows the CLI sign-in");

// Let the 503 settle into the unavailable state.
await React.act(async () => { await delay(120); });
await flush();
const settled = host.textContent ?? "";
assert(settled.includes("We couldn’t verify your session."), "verification failure is explained");
assert(settled.includes("Hosted workspace"), "failed verification keeps the hosted identity");
assert(settled.includes("Use another account"), "failed verification offers the hosted recovery path");
assert(settled.includes("Retry verification"), "failed verification offers a retry");
assert(!settled.includes("Private board"), "failed verification must not fall back to the private board");
assert(!settled.includes("Browser-bound Cadence session"), "no local session footer on a hosted board");
assert(!settled.includes("cadence ui login"), "no CLI instruction on a hosted board");
assert(!(host.textContent ?? "").includes("PRIVATE WORKSPACE"), "no workspace content before admission");
await React.act(async () => { root.unmount(); });

// A lost logout reply is not a sign-out or permission to reopen the board.
const AccountMenu = namedExport(require("../src/ui/AccountMenu"), "default") as typeof AccountMenuType;
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
const session = {
  id: "session-1", origin: "public", created: 1700000000, last_used: 1700000000,
  idle_expires_at: 4102444800, expires_at: 4102444800, user_agent: "test",
  user: { sub: "member", name: "Member", role: "member" },
};
let signedIn = true;
let resolveLogout!: (response: Response) => void;
let logout = new Promise<Response>((resolve) => { resolveLogout = resolve; });
globalThis.fetch = async (input) => {
  const path = new URL(String(input), location.origin).pathname;
  if (path === "/api/meta") return json({ signed_in: signedIn, hosted: true, session: signedIn ? session : null });
  if (path === "/api/session/logout") return logout;
  if (path === "/api/locale") return json({ organization_locale: "en", user_locale: "en" });
  throw new Error(`Unexpected request: ${path}`);
};
const nextRoot = createRoot(host);
await React.act(async () => {
  nextRoot.render(React.createElement(LocaleProvider, null, React.createElement(AuthBoundary, {
    children: (meta) => React.createElement("main", null, "PRIVATE WORKSPACE",
      React.createElement(AccountMenu, { meta, onChange: () => undefined, trigger: "avatar", placement: "below-end" })),
  })));
});
for (let n = 0; n < 20 && !host.querySelector("[data-account-menu]"); n += 1) await flush();
assert(host.querySelector("[data-account-menu]"), "verified hosted session opens the workspace");
await React.act(async () => { (host.querySelector("[data-account-menu] button") as HTMLButtonElement).click(); });
const button = (label: string) => Array.from(host.querySelectorAll("button")).find((item) => item.textContent === label);
await React.act(async () => { button("Sign out")?.click(); });
assert(!host.textContent?.includes("PRIVATE WORKSPACE"), "private workspace is removed while sign-out is pending");
await React.act(async () => { resolveLogout(json({ error: "unavailable" }, 503)); });
for (let n = 0; n < 20 && !button("Retry sign-out"); n += 1) await flush();
assert(host.textContent?.includes("We couldn’t confirm sign-out."), "lost sign-out confirmation is explained truthfully");
await React.act(async () => { window.dispatchEvent(new Event("focus")); window.dispatchEvent(new Event("online")); });
await flush();
assert(!host.textContent?.includes("PRIVATE WORKSPACE"), "automatic checks cannot undo an uncertain sign-out");
logout = new Promise<Response>((resolve) => { resolveLogout = resolve; });
await React.act(async () => { button("Retry sign-out")?.click(); });
signedIn = false;
document.cookie = "cadence_signed_out=1; Path=/";
await React.act(async () => { resolveLogout(json({ ok: true })); });
for (let n = 0; n < 20 && !host.textContent?.includes("You signed out of this workspace."); n += 1) await flush();
assert(host.textContent?.includes("You signed out of this workspace."), "confirmed sign-out is distinct from a first visit");
const links = Array.from(host.querySelectorAll<HTMLAnchorElement>("a"));
const continueLink = links.find((link) => link.textContent?.includes("Continue with AgenticOS"));
const switchLink = links.find((link) => link.textContent?.includes("Use another account"));
assert(continueLink && switchLink, "signed-out page offers continue and account switching");
assert(new URL(continueLink.href).searchParams.get("to") === "https://fav-limited.cadencecloud.app/issues/12", "continue preserves the board destination");
assert(new URL(switchLink.href).searchParams.get("mode") === "switch", "account switching requests a real switch");
await React.act(async () => { nextRoot.unmount(); });
const reloadRoot = createRoot(host);
await React.act(async () => {
  reloadRoot.render(React.createElement(LocaleProvider, null,
    React.createElement(AuthBoundary, { children: () => React.createElement("main", null, "PRIVATE WORKSPACE") })));
});
for (let n = 0; n < 20 && !host.textContent?.includes("You signed out of this workspace."); n += 1) await flush();
assert(host.textContent?.includes("You signed out of this workspace."), "marked anonymous reload explains explicit sign-out");
assert(!host.textContent?.includes("PRIVATE WORKSPACE"), "marked reload does not mount private content");
await React.act(async () => { reloadRoot.unmount(); });
document.cookie = "cadence_signed_out=; Max-Age=0; Path=/";

// A delayed sibling notification cannot override a newer verified sign-in.
const sibling = { receiver: null as ((event: { data: string }) => void) | null };
class TestChannel {
  set onmessage(handler: ((event: { data: string }) => void) | null) { sibling.receiver = handler; }
  postMessage() {}
  close() {}
}
Object.defineProperty(win, "BroadcastChannel", { configurable: true, value: TestChannel });
signedIn = true;
let resolveMeta!: (response: Response) => void;
let heldMeta: Promise<Response> | null = null;
globalThis.fetch = async (input) => {
  const path = new URL(String(input), location.origin).pathname;
  if (path === "/api/meta") return heldMeta ?? json({ signed_in: signedIn, hosted: true, session });
  if (path === "/api/locale") return json({ organization_locale: "en", user_locale: "en" });
  throw new Error(`Unexpected request: ${path}`);
};
const siblingRoot = createRoot(host);
await React.act(async () => {
  siblingRoot.render(React.createElement(LocaleProvider, null,
    React.createElement(AuthBoundary, { children: () => React.createElement("main", null, "PRIVATE WORKSPACE") })));
});
for (let n = 0; n < 20 && !host.textContent?.includes("PRIVATE WORKSPACE"); n += 1) await flush();
assert(host.textContent?.includes("PRIVATE WORKSPACE"), "newly verified session opens the workspace");
heldMeta = new Promise<Response>((resolve) => { resolveMeta = resolve; });
assert(sibling.receiver, "hosted tab listens for sibling session changes");
await React.act(async () => { sibling.receiver?.({ data: "signed-out" }); });
assert(!host.textContent?.includes("PRIVATE WORKSPACE"), "sibling notice hides private content during recheck");
await React.act(async () => { resolveMeta(json({ signed_in: true, hosted: true, session })); });
for (let n = 0; n < 20 && !host.textContent?.includes("PRIVATE WORKSPACE"); n += 1) await flush();
assert(host.textContent?.includes("PRIVATE WORKSPACE"), "current server proof wins over a delayed sibling notice");
await React.act(async () => { siblingRoot.unmount(); });
console.log("hosted gate failure checks passed");
}
void main().catch((e: unknown) => { console.error(e); process.exit(1); });
