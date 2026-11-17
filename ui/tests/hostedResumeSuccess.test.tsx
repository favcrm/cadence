/**
 * A successful resume reopens the workspace without a platform round trip.
 */
import type * as HappyDom from "happy-dom";
import type * as ReactNs from "react";
import type * as ReactDomClient from "react-dom/client";
import type * as LocaleModule from "../src/lib/locale";
import type AuthBoundaryType from "../src/features/auth/AuthBoundary";

declare function require(name: string): unknown;
declare const process: { exit(code: number): never };
export {};

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
  const { Window } = require("happy-dom") as typeof HappyDom;
  const win = new Window({ url: "http://localhost/apps/issues" });
  for (const name of [
    "window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement",
    "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator",
    "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent",
    "location", "history", "sessionStorage", "localStorage",
  ] as const)
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"] as const)
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });

  // This browser has signed in before; the board now answers as signed out.
  win.localStorage.setItem("cadence.hostedPresence", "1");

  const loader = require("module") as { prototype: { require: (this: unknown, id: string) => unknown } };
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
  const { default: AuthBoundary } = require("../src/features/auth/AuthBoundary") as {
    default: typeof AuthBoundaryType;
  };

  const REFUSAL = { ok: false, error: { code: "board_session_required", message: "a board session is required" } };
  const SESSION_ID = "bsi_11111111-1111-4111-8111-111111111111";
  const readyMeta = {
    read_only: false,
    hosted: true,
    signed_in: true,
    session: {
      id: SESSION_ID,
      binding: SESSION_ID,
      origin: "public",
      user: { sub: "usr_test", email: "test@example.test", name: "Test", role: "member", handle: "test" },
      created: 1_790_000_000,
      last_used: 1_790_000_000,
      idle_expires_at: 4_102_444_800,
      expires_at: 4_102_444_800,
      user_agent: "test",
    },
    operator: false,
    actor: "users/test",
    version: "hosted-board-edge.v1",
  };
  let metaCalls = 0;
  let resumeCalls = 0;
  let resumed = false;
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.href : new Request(input).url;
    if (url.endsWith("/__platform/session/resume")) {
      resumeCalls += 1;
      resumed = true;
      return new Response(JSON.stringify({ ok: true }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }
    metaCalls += 1;
    return new Response(JSON.stringify(resumed ? readyMeta : REFUSAL), {
      status: resumed ? 200 : 401,
      headers: { "Content-Type": "application/json" },
    });
  }) as typeof fetch;

  const delay = (ms: number): Promise<void> => new Promise<void>((resolve) => { setTimeout(resolve, ms); });
  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);
  const flush = () => React.act(async () => { await delay(0); });
  await React.act(async () => {
    root.render(React.createElement(LocaleProvider, null,
      React.createElement(AuthBoundary, { children: () => React.createElement("main", null, "PRIVATE WORKSPACE") })));
  });
  for (let i = 0; i < 60 && !host.textContent?.includes("PRIVATE WORKSPACE"); i++) {
    await flush();
    await delay(10);
  }
  await flush();

  const text = host.textContent ?? "";
  assert(text.includes("PRIVATE WORKSPACE"), "a successful resume reopens the workspace");
  assert(host.querySelector("[data-auth-state='ready']"), "the boundary reports a ready session");
  assert(resumeCalls === 1, `exactly one resume attempt, saw ${resumeCalls}`);
  assert(metaCalls >= 2, `the session was re-verified after resume, saw ${metaCalls}`);
  await React.act(async () => { root.unmount(); });
  console.log("hosted resume success checks passed");
}

void main().catch((e: unknown) => { console.error(e); process.exit(1); });
