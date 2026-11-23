export {};
/**
 * CAD-2726: Day-0 starter prompts appear on empty workspaces and revert to
 * operational prompts once projects or agents exist.
 */
import { streamFixture } from "./streamFixture";
import type * as HappyDOM from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDOM from "react-dom/client";
import type * as HomeModule from "../src/features/home/Home";
import type * as Locale from "../src/lib/locale";
import type * as Stores from "../src/lib/resources";

declare function require<T>(name: string): T;

const { Window } = require<typeof HappyDOM>("happy-dom");
const win = new Window({ url: "https://acme.cadencecloud.app/" });
for (const key of [
  "window", "document", "navigator", "Node", "Element", "HTMLElement",
  "HTMLInputElement", "HTMLTextAreaElement", "HTMLButtonElement", "HTMLFormElement",
  "MutationObserver", "ResizeObserver", "getComputedStyle", "localStorage", "sessionStorage",
] as const) {
  Object.defineProperty(globalThis, key, {
    value: key === "window" ? win : (win as unknown as Record<string, unknown>)[key],
    configurable: true,
    writable: true,
  });
}
for (const key of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"] as const) {
  Object.defineProperty(globalThis, key, {
    value: (win as unknown as Record<string, (...a: unknown[]) => unknown>)[key].bind(win),
    configurable: true,
    writable: true,
  });
}
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

const loader = require<{ prototype: { require: (this: unknown, id: string) => unknown } }>("module");
const originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react" || id === "remark-gfm") return { HugeiconsIcon: () => null };
  if (id === "react-markdown") return { __esModule: true, default: () => null };
  return originalRequire.call(this, id);
};

const React = require<typeof ReactModule>("react");
const { createRoot } = require<typeof ReactDOM>("react-dom/client");
const Home = require<typeof HomeModule>("../src/features/home/Home").default;
const { LocaleProvider } = require<typeof Locale>("../src/lib/locale");
const { resources } = require<typeof Stores>("../src/lib/resources");
const { setImmediate: nextTurn } = require<{ setImmediate: () => Promise<void> }>("node:timers/promises");

const identity = { name: null, tone: null, look: "robot", revision: "fixture" };
const masterPayload = {
  alias: "master",
  identity,
  provider: "fixture",
  endpoint_kind: "fixture",
  live: true,
  turn: null,
  queue: [],
};
const json = (value: unknown) =>
  new Response(JSON.stringify(value), { headers: { "Content-Type": "application/json" } });

globalThis.fetch = async (input, init) => {
  const url = new URL(String(input), "https://acme.cadencecloud.app");
  if (url.pathname.endsWith("/stream")) return streamFixture(url.href, masterPayload as never, init?.signal);
  if (url.pathname === "/api/threads/master") {
    return json({ thread: { id: "home-thread", alias: "master" }, entries: [], more_before: false });
  }
  if (url.pathname === "/api/master/state") return json(masterPayload);
  if (url.pathname === "/api/agents") {
    return json({ daemon: "reachable", agents: [{ alias: "master", state: "running" }], profiles: [], totals: null });
  }
  if (url.pathname === "/api/meta") return json({ operator: true, read_only: false });
  return json({});
};

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(`homeDay0Prompts failed: ${why}`);
}

const settle = async () => {
  for (let i = 0; i < 8; i++) {
    await React.act(async () => {
      await nextTurn();
    });
  }
};

async function main(): Promise<void> {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);

  await React.act(async () => {
    resources.agents.write(() => ({
      daemon: "reachable",
      agents: [{ alias: "master", state: "running" }],
      profiles: [],
      totals: null,
    }) as never);
  });

  // 1. workspaceEmpty = true -> Day-0 prompts
  await React.act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(Home, {
          readOnly: false,
          hosted: true,
          overview: resources.overview.get(),
          onOpenIssue: () => undefined,
          onOpenDecision: () => undefined,
          permissionsHref: "/settings/permissions",
          resolveAppPath: () => null,
          workspaceEmpty: true,
        }),
      ),
    );
  });
  await settle();

  const day0Text = host.querySelector('[data-empty="thread"]')?.textContent ?? "";
  assert(
    day0Text.includes("Help me set up my first project and break down the initial milestones."),
    "renders Day-0 setup prompt when workspace is empty",
  );
  assert(
    !day0Text.includes("What is every agent working on right now, and is anything stuck?"),
    "omits active-team prompt when workspace is empty",
  );

  // 2. workspaceEmpty = false -> standard operational prompts
  await React.act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(Home, {
          readOnly: false,
          hosted: true,
          overview: resources.overview.get(),
          onOpenIssue: () => undefined,
          onOpenDecision: () => undefined,
          permissionsHref: "/settings/permissions",
          resolveAppPath: () => null,
          workspaceEmpty: false,
        }),
      ),
    );
  });
  await settle();

  const normalText = host.querySelector('[data-empty="thread"]')?.textContent ?? "";
  assert(
    normalText.includes("What is every agent working on right now, and is anything stuck?"),
    "renders normal operational prompts once workspace has content",
  );

  await React.act(async () => root.unmount());
  host.remove();
  console.log("homeDay0Prompts checks passed");
}

declare const process: { exit: (code?: number) => void };
main().then(
  () => process.exit(0),
  (e) => {
    console.error(e);
    process.exit(1);
  },
);
