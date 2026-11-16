/** Project grants must not be lost or retargeted while roster and policy writes overlap. */
import type * as HappyDom from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDomClient from "react-dom/client";
import type * as Locale from "../src/lib/locale";
import type * as PrivateSession from "../src/lib/privateSession";
import type * as ProjectSettingsModule from "../src/features/projects/ProjectSettings";

declare function require(name: string): unknown;

function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}
let act: typeof ReactModule.act;
async function settle(check: () => void) {
  for (let n = 0; n < 100; n++) {
    try { check(); return; } catch (error) {
      if (n === 99) throw error;
      await act(async () => { await Promise.resolve(); });
    }
  }
}

async function main() {
  const { Window } = require("happy-dom") as typeof HappyDom;
  const win = new Window({ url: "http://localhost/projects/sample/settings" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage", "localStorage"] as const)
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener"] as const)
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  Object.defineProperty(globalThis, "requestAnimationFrame", { value: (fn: FrameRequestCallback) => { fn(0); return 0; }, configurable: true });
  const loader = require("module") as { prototype: { require: (this: unknown, id: string) => unknown } };
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
  const ProjectSettings = (require("../src/features/projects/ProjectSettings") as typeof ProjectSettingsModule).default;
  allowPrivateReads();

  const roster = { members: [
    { userId: "usr_a", name: "Alex", role: "member", joinedAt: "2026-01-01" },
    { userId: "usr_b", name: "Blair", role: "member", joinedAt: "2026-01-01" },
  ] };
  const policy = { state: "ok", project: "sample", visibility: "private", grants: [] as string[], revision: 1, provenance: "hosted-owner-attested" };
  const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
  let holdRoster = false;
  let rosterRequested = false;
  // The test runner uses Node 22; this project's ES2023 library omits the built-in type.
  const deferred = Promise as typeof Promise & {
    withResolvers<T>(): { promise: Promise<T>; resolve: (value: T) => void };
  };
  const rosterDeferred = deferred.withResolvers<Response>();
  const writeDeferred = deferred.withResolvers<Response>();
  let sent: { visibility: string; grants: string[]; if_rev: string } | null = null;
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = new URL(typeof input === "string" ? input : String((input as Request).url), "http://localhost");
    const method = init?.method ?? "GET";
    if (url.pathname === "/api/team/members") {
      if (!holdRoster) return json(roster);
      rosterRequested = true;
      return await rosterDeferred.promise;
    }
    if (url.pathname === "/api/projects/sample/policy" && method === "GET") return json(policy);
    if (url.pathname === "/api/projects/sample/policy" && method === "PUT") {
      sent = JSON.parse(String(init?.body));
      return await writeDeferred.promise;
    }
    throw new Error(`Unexpected ${method} ${url.pathname}`);
  }) as typeof fetch;

  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const toasts: string[] = [];
  await act(async () => root.render(React.createElement(LocaleProvider, null,
    React.createElement(ProjectSettings, { project: "sample", block: null, hosted: true, onToast: (text: string) => toasts.push(text) }))));
  const button = (text: string) => Array.from(host.querySelectorAll("button")).find((item) => item.textContent?.trim() === text) as HTMLButtonElement | undefined;
  const selector = () => host.querySelector<HTMLButtonElement>('[aria-label="Grant a workspace member"]');
  await settle(() => assert(selector() && button("Save policy"), "private settings and roster render"));

  async function choose(name: string) {
    await act(async () => selector()!.click());
    const option = Array.from(document.querySelectorAll('[role="option"]')).find((item) => item.textContent?.includes(name)) as HTMLElement | undefined;
    assert(option, `${name} appears in the member roster`);
    await act(async () => option.click());
  }
  await choose("Alex");
  await act(async () => button("Add")!.click());
  await settle(() => assert(host.textContent?.includes("usr_a"), "the first selected member is added"));
  await choose("Blair");
  holdRoster = true;
  await act(async () => button("Add")!.click());
  await settle(() => assert(rosterRequested, "the second membership check is pending"));
  assert(button("Save policy")?.disabled, "a pending add cannot be skipped by saving");
  assert(selector()?.disabled, "selection cannot change during membership verification");
  await act(async () => rosterDeferred.resolve(json(roster)));
  await settle(() => assert(host.textContent?.includes("usr_b"), "the verified second member is added"));
  assert(!button("Save policy")?.disabled, "the resolved draft can be saved");
  await act(async () => button("Save policy")!.click());
  await settle(() => assert(sent, "the policy save is pending"));
  assert(selector()?.disabled, "a pending save cannot accept another selected grant");
  assert(Array.from(host.querySelectorAll("button")).filter((item) => item.textContent?.trim() === "Remove").every((item) => item.disabled), "a pending save cannot remove a grant");
  assert(JSON.stringify(sent!.grants) === JSON.stringify(["usr_a", "usr_b"]), "the saved policy contains both verified members");
  assert(sent!.if_rev === "1", "the saved policy pins the revision that was read");
  policy.grants = [...sent!.grants];
  policy.revision += 1;
  await act(async () => writeDeferred.resolve(json(policy)));
  await settle(() => assert(toasts.length === 1, "a matching receipt completes the save"));
  await act(async () => root.unmount());
  console.log("project grant race checks passed");
}

main().catch((error) => { console.error(error); throw error; });
