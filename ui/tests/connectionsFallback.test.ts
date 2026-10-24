/**
 * CAD-1797 tenant fallback: when `/api/connection-providers` is not
 * carried (501), Connections still offers the workspace SMTP account.
 * Mounts the real Connections add page against a stubbed transport, not
 * a source regex.
 */
declare function require(name: string): any;
import type { act as reactAct } from "react";

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual), e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
let act: typeof reactAct;
async function settle(fn: () => void, ms = 8000) {
  const end = Date.now() + ms;
  for (;;) {
    try { fn(); return; } catch (e) {
      if (Date.now() > end) throw e;
      await act(async () => sleep(20));
    }
  }
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/settings/connections/new" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLSelectElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener"])
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  Object.defineProperty(globalThis, "requestAnimationFrame", { value: (fn: () => void) => setTimeout(fn, 0), configurable: true });
  const loader = require("module"), originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, arguments);
  };

  const React = require("react") as typeof import("react");
  act = React.act;
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
  const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof import("../screen-ui/src/navigation");
  const { hostNavigation } = require("../src/ui/hostNavigation") as typeof import("../src/ui/hostNavigation");
  const Connections = (require("../src/features/settings/Connections") as typeof import("../src/features/settings/Connections")).default;

  const posts: { path: string; body: any }[] = [];
  const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = new URL(String(input), "http://localhost");
    const method = init?.method ?? "GET";
    const body = init?.body ? JSON.parse(String(init.body)) : undefined;
    if (url.pathname === "/api/connection-providers") return json({ error: "not carried on this board" }, 501);
    if (url.pathname === "/api/connections" && method === "GET") return json({ connections: [] });
    if (url.pathname === "/api/connections" && method === "POST") {
      posts.push({ path: url.pathname, body });
      return json({ connection: { id: "c-new" } });
    }
    throw new Error(`Unexpected ${method} ${url.pathname}`);
  }) as typeof fetch;

  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  act(() => {
    root.render(
      React.createElement(NavigationProvider, {
        adapter: hostNavigation,
        children: React.createElement(LocaleProvider, null,
          React.createElement(Connections, { viewer: { operator: true, readOnly: false }, page: { kind: "add" } }),
        ),
      }),
    );
  });

  const text = () => host.textContent ?? "";
  await settle(() => assert(text().includes("Your email address"), "fallback SMTP form renders"));
  assert(text().includes("The service catalogue is not available on this board"), "catalogue fallback is explained");
  assert(!text().includes("could not load the registered services"), "the 501 is not a dead-end error gate");
  assert(text().includes("Connect email"), "add action is offered");

  const field = (label: string) =>
    Array.from(host.querySelectorAll("label")).find((l) => (l.textContent ?? "").includes(label));
  const fill = (label: string, value: string) => {
    const lab = field(label);
    assert(lab, `field labelled ${label}`);
    const input = lab!.parentElement!.querySelector("input") as HTMLInputElement;
    act(() => {
      const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, "value")!.set!;
      setter.call(input, value);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
  };
  fill("Your email address", "owner@essentialfoods.test");
  fill("App password", "stub-app-password");
  const add = Array.from(host.querySelectorAll("button")).find((b) => (b.textContent ?? "").trim() === "Connect email") as HTMLButtonElement;
  act(() => add.click());
  await settle(() => assert(posts.length === 1, "one connection create posted"));
  equal(posts[0].body.provider, "smtp", "SMTP provider");
  equal(posts[0].body.account, "email", "workspace account");
  equal(posts[0].body.shape, "smtp", "SMTP shape");
  equal(posts[0].body.scopes, ["email:send"], "only the send scope");
  equal(posts[0].body.sender, "owner@essentialfoods.test", "sender address");
  equal(posts[0].body.username, "owner@essentialfoods.test", "SMTP login");
  assert(posts[0].body.secret === "stub-app-password", "secret crosses once in the request");
  await act(async () => root.unmount());

  // A custody-consent refusal stores nothing; the secret must not survive
  // the settled request, even though the consent retry stays available.
  const { ApiError } = require("../src/lib/apiError") as typeof import("../src/lib/apiError");
  const HostedSmtpConnect = (require("../src/features/settings/HostedSmtpConnect") as typeof import("../src/features/settings/HostedSmtpConnect")).default;
  const host2 = document.createElement("div");
  document.body.append(host2);
  const root2 = createRoot(host2);
  act(() => {
    root2.render(
      React.createElement(LocaleProvider, null,
        React.createElement(HostedSmtpConnect, {
          connected: null,
          onConnect: async () => { throw new ApiError("unprotected", 400, { code: "custody_unprotected" }); },
        }),
      ),
    );
  });
  await settle(() => assert(host2.querySelector("#hs-password"), "credential form renders"));
  const field2 = (label: string) =>
    Array.from(host2.querySelectorAll("label")).find((l) => (l.textContent ?? "").includes(label));
  const fill2 = (label: string, value: string) => {
    const lab = field2(label);
    assert(lab, `field labelled ${label}`);
    const input = lab!.parentElement!.querySelector("input") as HTMLInputElement;
    act(() => {
      const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, "value")!.set!;
      setter.call(input, value);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
  };
  fill2("Your email address", "owner@essentialfoods.test");
  fill2("App password", "stub-app-password");
  const submit2 = Array.from(host2.querySelectorAll("button")).find((b) => (b.textContent ?? "").trim() === "Connect email") as HTMLButtonElement;
  act(() => submit2.click());
  await settle(() => {
    const password = host2.querySelector("#hs-password") as HTMLInputElement;
    assert(password.value === "", "password is cleared after a custody refusal");
    assert((host2.textContent ?? "").includes("This workspace can't keep the password apart"), "consent guidance remains");
  });
  assert(!(host2.textContent ?? "").includes("stub-app-password"), "the secret never appears in text");
  await act(async () => root2.unmount());

  // A save that finishes after the form is left must not navigate the
  // operator away from the page they moved to.
  const SmtpAccountForm = (require("../src/features/settings/SmtpAccountForm") as typeof import("../src/features/settings/SmtpAccountForm")).default;
  let resolveSave!: (response: Response) => void;
  globalThis.fetch = ((input: unknown, init?: RequestInit) => {
    if (init?.method === "POST" && String(input).endsWith("/api/connections")) {
      return new Promise<Response>((resolve) => { resolveSave = resolve; });
    }
    throw new Error(`Unexpected ${String(input)}`);
  }) as typeof fetch;
  let saved: string | null = null;
  const host3 = document.createElement("div");
  document.body.append(host3);
  const root3 = createRoot(host3);
  act(() => {
    root3.render(
      React.createElement(LocaleProvider, null,
        React.createElement(SmtpAccountForm, { rows: [], onSaved: (id: string) => { saved = id; } }),
      ),
    );
  });
  await settle(() => assert(host3.querySelector("#hs-password"), "late-save form renders"));
  const field3 = (label: string) =>
    Array.from(host3.querySelectorAll("label")).find((l) => (l.textContent ?? "").includes(label));
  const fill3 = (label: string, value: string) => {
    const lab = field3(label);
    assert(lab, `field labelled ${label}`);
    const input = lab!.parentElement!.querySelector("input") as HTMLInputElement;
    act(() => {
      const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, "value")!.set!;
      setter.call(input, value);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
  };
  fill3("Your email address", "owner@essentialfoods.test");
  fill3("App password", "stub-app-password");
  const submit3 = Array.from(host3.querySelectorAll("button")).find((b) => (b.textContent ?? "").trim() === "Connect email") as HTMLButtonElement;
  act(() => submit3.click());
  await settle(() => assert(typeof resolveSave === "function", "save request started"));
  await act(async () => root3.unmount());
  await act(async () => {
    resolveSave(new Response(JSON.stringify({ connection: { id: "late-save" } }), {
      status: 200, headers: { "Content-Type": "application/json" },
    }));
    await sleep(0);
  });
  assert(saved === null, "a save completing after unmount does not navigate");
  console.log("connections fallback checks passed");
}
void main();
