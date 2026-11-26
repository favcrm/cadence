declare function require(name: string): unknown;
import type * as HappyDom from "happy-dom";
import type { act as reactAct } from "react";
import type * as ReactModule from "react";
import type * as ReactDomModule from "react-dom/client";
import type * as LocaleModule from "../src/lib/locale";
import type * as EmailSetupModule from "../src/features/settings/EmailSetup";
import type { Connection } from "../src/lib/types";

function assert(value: unknown, message: string): asserts value { if (!value) throw new Error(message); }
let act: typeof reactAct;
async function settle(check: () => void) {
  for (let attempt = 0; attempt < 50; attempt++) {
    try { check(); return; } catch (error) { if (attempt === 49) throw error; await act(async () => { await Promise.resolve(); }); }
  }
}
async function main() {
  const { Window } = require("happy-dom") as typeof HappyDom;
  const win = new Window({ url: "http://localhost/settings/connections" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"] as const)
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener"] as const)
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "requestAnimationFrame", { value: (fn: () => void) => { fn(); return 0; }, configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  const loader = require("module") as { prototype: { require: (id: string) => unknown } };
  const original = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return original.call(this, id);
  };
  const React = require("react") as typeof ReactModule;
  act = React.act;
  const { createRoot } = require("react-dom/client") as typeof ReactDomModule;
  const { LocaleProvider } = require("../src/lib/locale") as typeof LocaleModule;
  const EmailSetup = (require("../src/features/settings/EmailSetup") as typeof EmailSetupModule).default;
  const posts: { account: string; scopes: string[]; username: string }[] = [];
  const protectedPosts: unknown[] = [];
  let protectedState: unknown = { configured: false, protected: false, origin: null, board_origin: "http://localhost" };
  let refuse = false;
  const gate: { hold: boolean; release?: () => void } = { hold: false };
  const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const path = new URL(String(input), "http://localhost").pathname;
    if (path === "/api/protected-entry") return protectedState === null ? json({}, 500) : json(protectedState);
    if (path === "/api/credential-requests" && init?.method === "POST") {
      protectedPosts.push(JSON.parse(String(init.body)));
      return json({ request: { entry_url: "https://unexpected.example/connections/enroll/12345678-1234-1234-1234-1234567890ab" } });
    }
    if (path === "/api/connection-providers") return json({ providers: [{ provider: "smtp", descriptor_available: true, descriptor: { enrollment_shapes: ["smtp"], capabilities: [{ effect: "send", scopes: ["email:send"] }, { effect: "read", scopes: ["email:read"] }] } }] });
    if (path === "/api/connections" && init?.method === "POST") {
      posts.push(JSON.parse(String(init.body)));
      // The test target predates Promise.withResolvers.
      if (gate.hold) await new Promise<void>((resolve) => { gate.release = resolve; });
      return refuse ? json({ code: "smtp_not_provisioned" }, 400) : json({ connection: { id: "new" } });
    }
    throw new Error(`Unexpected ${path}`);
  }) as typeof fetch;
  const host = document.createElement("div"); document.body.append(host);
  const root = createRoot(host);
  let saved = 0;
  const render = (hostedSmtp: boolean, rows: Connection[] = []) => act(() => root.render(React.createElement(LocaleProvider, null, React.createElement(EmailSetup, { hostedSmtp, rows, onSaved: () => { saved++; }, onClose: () => {} }))));
  const text = () => document.body.textContent ?? "";
  const click = (label: string) => { const b = Array.from(document.querySelectorAll("button")).find((el) => el.textContent?.trim() === label); assert(b, `missing ${label}`); act(() => b.click()); };
  const fill = (id: string, value: string) => { const input = document.querySelector(id) as HTMLInputElement; assert(input, `missing ${id}`); act(() => { Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, "value")!.set!.call(input, value); input.dispatchEvent(new Event("input", { bubbles: true })); }); };

  protectedState = null;
  render(true);
  click("Continue");
  await settle(() => assert(document.querySelector("#hs-password"), "hosted setup does not require native metadata"));
  assert(document.querySelector('label[for="hs-password"]')?.textContent === "App password", "Gmail uses app password label");
  act(() => (document.querySelector("#hs-preset") as HTMLButtonElement).click());
  const microsoft = Array.from(document.querySelectorAll('[role="option"]')).find((el) => el.textContent?.includes("Microsoft 365 (legacy SMTP)")) as HTMLElement;
  assert(microsoft, "Microsoft legacy SMTP preset available");
  act(() => microsoft.click());
  assert(document.querySelector('label[for="hs-password"]')?.textContent === "SMTP password", "Microsoft uses SMTP password label");
  assert(text().includes("Your organization and mailbox must allow password-based SMTP AUTH. OAuth is not supported for app senders."), "Microsoft prerequisites are explicit");
  fill("#hs-address", "sender@example.com"); fill("#hs-password", "secret");
  refuse = true; click("Connect email");
  await settle(() => assert(text().includes("Email sending isn't set up for this workspace yet."), "relay refusal is shown"));
  assert((document.querySelector("#hs-password") as HTMLInputElement).value === "", "secret cleared on refusal");
  assert(posts[0].account === "email" && posts[0].scopes.join() === "email:send", "hosted create contract");
  assert(saved === 0, "refusal does not save");
  await act(async () => root.unmount());

  const host2 = document.createElement("div"); document.body.append(host2); const root2 = createRoot(host2);
  protectedState = { configured: true, protected: true, origin: "https://trusted.example", board_origin: "http://localhost" };
  act(() => root2.render(React.createElement(LocaleProvider, null, React.createElement(EmailSetup, { hostedSmtp: false, rows: [], onSaved: () => {}, onClose: () => {} }))));
  await settle(() => { const button = Array.from(document.querySelectorAll("button")).find((b) => b.textContent?.trim() === "Continue") as HTMLButtonElement; assert(button && !button.disabled, "reviewed SMTP available"); });
  click("Continue");
  await settle(() => assert(text().includes("Enter the SMTP details only on the separate trusted credential page."), "protected profile offers trusted entry"));
  assert(!document.querySelector('input[type="password"]'), "protected profile never shows password");
  click("Continue on secure entry…");
  await settle(() => assert(text().includes("The trusted credential page could not be confirmed."), "mismatched entry origin is refused"));
  assert(
    JSON.stringify(protectedPosts) === JSON.stringify([{ provider: "smtp", verb: "enroll", shape: "smtp", scopes: ["email:send"] }]),
    "protected enrollment sends only reviewed request metadata",
  );
  assert(location.origin === "http://localhost" && !document.querySelector('input[type="password"]'), "refusal stays on the board without offering password input");
  click("Back");
  assert(document.activeElement?.id === "email-method", "Back returns focus to method");
  await act(async () => root2.unmount());

  for (const state of [
    { configured: false, protected: false, origin: "https://trusted.example" },
    { protected: false, origin: null },
  ]) {
    const node = document.createElement("div"); document.body.append(node); const testRoot = createRoot(node);
    protectedState = state;
    act(() => testRoot.render(React.createElement(LocaleProvider, null, React.createElement(EmailSetup, { hostedSmtp: false, rows: [], onSaved: () => {}, onClose: () => {} }))));
    await settle(() => { const button = Array.from(document.querySelectorAll("button")).find((b) => b.textContent?.trim() === "Continue") as HTMLButtonElement; assert(button && !button.disabled, "native SMTP available"); });
    click("Continue");
    await settle(() => assert(text().includes("Credential entry could not be checked."), "ambiguous metadata fails closed"));
    assert(!text().includes("Continue on secure entry…"), "ambiguous metadata cannot start secure entry");
    assert(!document.querySelector("#hs-password"), "ambiguous metadata never shows password");
    await act(async () => testRoot.unmount());
  }

  const host3 = document.createElement("div"); document.body.append(host3); const root3 = createRoot(host3);
  protectedState = { configured: false, protected: false, origin: null, board_origin: "http://localhost" };
  refuse = false;
  act(() => root3.render(React.createElement(LocaleProvider, null, React.createElement(EmailSetup, { hostedSmtp: false, rows: [], onSaved: () => { saved++; }, onClose: () => {} }))));
  await settle(() => { const button = Array.from(document.querySelectorAll("button")).find((b) => b.textContent?.trim() === "Continue") as HTMLButtonElement; assert(button && !button.disabled, "native SMTP available"); });
  click("Continue");
  await settle(() => assert(document.querySelector("#hs-account"), "native account field available"));
  fill("#hs-account", "newsletter"); fill("#hs-address", "sender@example.com");
  fill("#hs-username", "mailer"); fill("#hs-password", "secret");
  gate.hold = true;
  click("Connect email");
  assert((Array.from(document.querySelectorAll("button")).find((b) => b.textContent?.trim() === "Back") as HTMLButtonElement).disabled, "Back disabled during verification");
  assert((Array.from(document.querySelectorAll("button")).find((b) => b.textContent?.trim() === "Cancel") as HTMLButtonElement).disabled, "Cancel disabled during verification");
  act(() => window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })));
  assert(document.querySelector("#hs-password"), "Escape cannot dismiss during verification");
  assert(!document.querySelector('[role="dialog"][data-closing="true"]'), "Escape does not start closing while verification is pending");
  assert(gate.release, "verification request is pending");
  await act(async () => { gate.release!(); });
  await settle(() => assert(Number(saved) === 1, "native enrollment completes"));
  assert(posts[1].account === "newsletter" && posts[1].username === "mailer", "native account and username used");
  assert(posts[1].scopes.join() === "email:send", "native grants reviewed send scopes only");
  await act(async () => root3.unmount());
}
void main();
