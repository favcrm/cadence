/**
 * The idle gate must not read as a first visit, and a browser that has signed
 * in before should try its paired credential once before asking the person to
 * sign in again.
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
  let metaCalls = 0;
  let resumeCalls = 0;
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.href : new Request(input).url;
    if (url.endsWith("/__platform/session/resume")) {
      resumeCalls += 1;
      return new Response(JSON.stringify({ ok: false }), {
        status: 401,
        headers: { "Content-Type": "application/json" },
      });
    }
    metaCalls += 1;
    return new Response(JSON.stringify(REFUSAL), {
      status: 401,
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
  for (let i = 0; i < 40 && !host.querySelector("[data-auth-state='expired'] h1"); i++) {
    await flush();
    await delay(10);
  }
  await flush();

  const text = host.textContent ?? "";
  assert(host.querySelector("[data-auth-state='expired'] h1"), "a returning browser sees the ended-session gate, not a welcome");
  assert(text.includes("Session ended"), "the gate says the session ended");
  assert(text.includes("Continue with AgenticOS"), "the expired gate still offers the platform sign-in");
  assert(resumeCalls === 1, `exactly one resume attempt, saw ${resumeCalls}`);
  assert(metaCalls >= 1, "the boundary checked the session");
  assert(!text.includes("PRIVATE WORKSPACE"), "the expired gate renders no private workspace");
  await React.act(async () => { root.unmount(); });
  console.log("hosted resume checks passed");
}

void main().catch((e: unknown) => { console.error(e); process.exit(1); });
