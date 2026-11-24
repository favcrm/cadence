/**
 * CAD-2748: Screen mount resilience on /apps/:id.
 *
 * 1. InstalledScreenOutlet renders ScreenFailurePane for mount/runtime failures,
 *    while an app declaring no screen keeps the ScreenUnavailable fallback.
 * 2. When /mount returns a newer digest than projection.digest, ScreenOutlet
 *    asks the shell to refresh scope and mounts cleanly on the refreshed digest.
 * 3. ScreenOutlet does not fire handshake_timeout while the document is hidden
 *    and auto-retries a hidden transient failure once on returning to visible.
 */
import type * as HappyDom from "happy-dom";
import type * as ReactNs from "react";
import type * as ReactDomClient from "react-dom/client";
import type * as LocaleModule from "../src/lib/locale";
import type * as PrivateSessionModule from "../src/lib/privateSession";
import type { InstalledScreenOutlet as InstalledScreenOutletType } from "../src/features/workspace-apps/screen/ScreenHost";
import type ScreenOutletType from "../src/features/workspace-apps/screen/ScreenOutlet";
import type ScreenFailurePaneType from "../src/features/workspace-apps/screen/ScreenFailurePane";
import type ScreenUnavailableType from "../src/features/workspace-apps/screen/ScreenUnavailable";
import type { ScreenPush } from "../src/features/workspace-apps/screen/screenProtocol";
import type { AppContext, Installation } from "../src/features/workspace-apps/workspaceApps";

declare function require(name: string): unknown;
declare const process: { exit(code: number): never };
export {};

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

type ModuleLoader = {
  prototype: { require: (this: unknown, id: string) => unknown };
};

async function main() {
  const { Window } = require("happy-dom") as typeof HappyDom;
  const win = new Window({ url: "https://fav-limited.cadencecloud.app/apps/waba" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLIFrameElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MessageEvent", "location", "history"] as const) {
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  }
  for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"] as const) {
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });
  }
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true, configurable: true });

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
  const { allowPrivateReads, resetPrivateSession } = require("../src/lib/privateSession") as typeof PrivateSessionModule;
  const { InstalledScreenOutlet } = require("../src/features/workspace-apps/screen/ScreenHost") as {
    InstalledScreenOutlet: typeof InstalledScreenOutletType;
  };
  const ScreenOutlet = (require("../src/features/workspace-apps/screen/ScreenOutlet") as {
    default: typeof ScreenOutletType;
  }).default;
  const ScreenFailurePane = (require("../src/features/workspace-apps/screen/ScreenFailurePane") as {
    default: typeof ScreenFailurePaneType;
  }).default;
  const ScreenUnavailable = (require("../src/features/workspace-apps/screen/ScreenUnavailable") as {
    default: typeof ScreenUnavailableType;
  }).default;

  resetPrivateSession();
  allowPrivateReads();

  const delay = (ms: number) => new Promise<void>((resolve) => { setTimeout(resolve, ms); });
  const flush = () => React.act(async () => { await delay(0); });
  const json = (body: unknown, status = 200) =>
    new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });

  const digestV1 = `sha256:${"a".repeat(64)}`;
  const digestV2 = `sha256:${"b".repeat(64)}`;
  const nonce = "c".repeat(64);
  const makeInstall = (files: string[], digest = digestV1): Installation => ({
    install_id: "inst-waba",
    name: "waba",
    title: "WhatsApp",
    version: "1.0.0",
    digest,
    summary: "WhatsApp inbox",
    executable: true,
    approval: { state: "approved" },
    files,
    removed: null,
  } as unknown as Installation);
  const contexts: AppContext[] = [{
    id: "ctx-1",
    install_id: "inst-waba",
    revision: 1,
    state: "active",
    digest: "ctx-d",
    config: { schema: 1, label: "General", input_defaults: {} },
  } as unknown as AppContext];
  const makeProjection = (digest: string): ScreenPush => ({
    v: 2,
    op: "screen",
    install_id: "inst-waba",
    digest,
    tag: "main",
    context_id: "ctx-1",
    installation: {
      install_id: "inst-waba",
      name: "waba",
      title: "WhatsApp",
      version: "1.0.0",
      digest,
      summary: "WhatsApp inbox",
    },
    contexts: [{ id: "ctx-1", label: "General" }],
    runs: [],
    outbox: [],
    publish_intents: { status: "ok", withheld: 0, rows: [] },
    readiness: { ok: true, blockers: [] },
    actions: [],
    now: 1700000000,
  });

  const setVisibility = (state: "visible" | "hidden") => {
    Object.defineProperty(document, "visibilityState", { value: state, configurable: true });
    document.dispatchEvent(new win.Event("visibilitychange") as unknown as Event);
  };
  setVisibility("visible");

  const host = document.createElement("div");
  document.body.appendChild(host);
  const previousFetch = globalThis.fetch;

  try {
    // 1. InstalledScreenOutlet mount failure renders ScreenFailurePane, while screenless app keeps ScreenUnavailable.
    globalThis.fetch = (async () => json({ error: { code: "app_bundle_missing", message: "missing" } }, 503)) as typeof fetch;
    const root1 = createRoot(host);
    const unavailableFallback = React.createElement(ScreenUnavailable, {
      installation: makeInstall([]),
      contexts,
      contextId: "ctx-1",
      admitted: true,
      operator: true,
    });
    await React.act(async () => {
      root1.render(React.createElement(LocaleProvider, null, React.createElement(InstalledScreenOutlet, {
        installation: makeInstall([]),
        contexts,
        contextId: "ctx-1",
        admitted: true,
        fallback: unavailableFallback,
        renderFailure: (failure, retry) => React.createElement(ScreenFailurePane, { failure, appName: "WhatsApp", onRetry: retry }),
      })));
    });
    await flush();
    assert(host.textContent?.includes("This app's screen is unavailable."), "screenless app preserves ScreenUnavailable fallback");

    await React.act(async () => {
      root1.render(React.createElement(LocaleProvider, null, React.createElement(InstalledScreenOutlet, {
        installation: makeInstall(["screens/main/screens.json"]),
        contexts,
        contextId: "ctx-1",
        admitted: true,
        fallback: unavailableFallback,
        renderFailure: (failure, retry) => React.createElement(ScreenFailurePane, { failure, appName: "WhatsApp", onRetry: retry }),
      })));
    });
    for (let i = 0; i < 10 && !host.querySelector("[data-screen-pane]"); i += 1) await flush();
    assert(host.querySelector('[data-screen-pane="refused"]'), "mount failure renders ScreenFailurePane instead of dead-end fallback");
    assert(!host.textContent?.includes("This app's screen is unavailable."), "screen mount failure hides ScreenUnavailable");
    assert(Array.from(host.querySelectorAll("button")).some((b) => b.textContent === "Retry"), "ScreenFailurePane includes Retry action");
    await React.act(async () => { root1.unmount(); });

    // 2. Stale digest on /mount triggers onRefreshScope and remounts once digest catches up.
    let refreshCalls = 0;
    globalThis.fetch = (async (input) => {
      const url = String(input);
      if (url.endsWith("/mount")) {
        return json({
          mount: `/api/app-screen/${"e".repeat(64)}`,
          bridge_nonce: nonce,
          generation: 2,
          tag: "main",
          host_contract: "screen-records.v1",
          install_id: "inst-waba",
          context_id: "ctx-1",
          digest: digestV2,
          display_session_key: "f".repeat(64),
          display_scope_key: "0".repeat(64),
          action_token: "1".repeat(64),
          tools: [],
          navigation: null,
          remote_images: [],
          operations: ["records.list"],
        });
      }
      if (url.includes("/revoke")) return json({ ok: true });
      throw new Error(`Unexpected fetch: ${url}`);
    }) as typeof fetch;

    const root2 = createRoot(host);
    await React.act(async () => {
      root2.render(React.createElement(LocaleProvider, null, React.createElement(ScreenOutlet, {
        projection: makeProjection(digestV1),
        fallback: React.createElement("div", null, "fallback"),
        renderFailure: (failure, retry) => React.createElement(ScreenFailurePane, { failure, appName: "WhatsApp", onRetry: retry }),
        onRefreshScope: () => {
          refreshCalls += 1;
          return true;
        },
      })));
    });
    for (let i = 0; i < 10 && refreshCalls === 0; i += 1) await flush();
    assert(refreshCalls === 1, "mismatched receipt digest invokes onRefreshScope once");
    assert(!host.querySelector('[data-screen-pane="invalid_receipt"]'), "digest refresh suppresses premature invalid_receipt failure pane");

    await React.act(async () => {
      root2.render(React.createElement(LocaleProvider, null, React.createElement(ScreenOutlet, {
        projection: makeProjection(digestV2),
        fallback: React.createElement("div", null, "fallback"),
        renderFailure: (failure, retry) => React.createElement(ScreenFailurePane, { failure, appName: "WhatsApp", onRetry: retry }),
        onRefreshScope: () => {
          refreshCalls += 1;
          return true;
        },
      })));
    });
    for (let i = 0; i < 10 && !host.querySelector("iframe"); i += 1) await flush();
    assert(Boolean(host.querySelector("iframe")), "updated projection digest mounts iframe cleanly");
    assert(refreshCalls === 1, "matched digest does not trigger extra scope refreshes");
    await React.act(async () => { root2.unmount(); });

    // 3. Handshake timeout does not fire while hidden, and arms when returning to visible.
    const realSetTimeout = globalThis.setTimeout;
    const realClearTimeout = globalThis.clearTimeout;
    const capturedHandshakeTimers: Array<{ id: number; fn: () => void; cleared: boolean }> = [];
    let nextTimerId = 90000;
    globalThis.setTimeout = ((fn: TimerHandler, ms?: number, ...args: unknown[]) => {
      if (ms === 10000 && typeof fn === "function") {
        const entry = { id: ++nextTimerId, fn: () => fn(...args), cleared: false };
        capturedHandshakeTimers.push(entry);
        return entry.id as unknown as ReturnType<typeof setTimeout>;
      }
      return realSetTimeout(fn, ms, ...(args as []));
    }) as typeof setTimeout;
    globalThis.clearTimeout = ((id?: ReturnType<typeof setTimeout>) => {
      const entry = capturedHandshakeTimers.find((t) => t.id === (id as unknown as number));
      if (entry) { entry.cleared = true; return; }
      realClearTimeout(id);
    }) as typeof clearTimeout;

    try {
      setVisibility("hidden");
      globalThis.fetch = (async (input) => {
        const url = String(input);
        if (url.endsWith("/mount")) {
          return json({
            mount: `/api/app-screen/${"e".repeat(64)}`,
            bridge_nonce: nonce,
            generation: 2,
            tag: "main",
          });
        }
        return json({ ok: true });
      }) as typeof fetch;

      const root3 = createRoot(host);
      await React.act(async () => {
        root3.render(React.createElement(LocaleProvider, null, React.createElement(ScreenOutlet, {
          projection: makeProjection(digestV1),
          fallback: React.createElement("div", null, "fallback"),
          renderFailure: (failure, retry) => React.createElement(ScreenFailurePane, { failure, appName: "WhatsApp", onRetry: retry }),
        })));
      });
      for (let i = 0; i < 10 && !host.querySelector("iframe"); i += 1) await flush();
      assert(capturedHandshakeTimers.filter((t) => !t.cleared).length === 0, "no 10s handshake timer arms while document is hidden");
      assert(!host.querySelector("[data-screen-pane]"), "no handshake_timeout occurs while document stays hidden");

      await React.act(async () => { setVisibility("visible"); });
      const activeTimers = capturedHandshakeTimers.filter((t) => !t.cleared);
      assert(activeTimers.length === 1, "10s handshake timer arms once document becomes visible");
      await React.act(async () => { activeTimers[0].fn(); });
      assert(Boolean(host.querySelector('[data-screen-pane="handshake_timeout"]')), "armed visible handshake timer triggers handshake_timeout");
      await React.act(async () => { root3.unmount(); });
    } finally {
      globalThis.setTimeout = realSetTimeout;
      globalThis.clearTimeout = realClearTimeout;
    }

    // 4. Hidden transient mount failure auto-retries once when document becomes visible again.
    const hiddenMounts = { attempts: 0 };
    globalThis.fetch = (async () => {
      hiddenMounts.attempts += 1;
      return json({ error: { code: "app_bundle_missing", message: "transient" } }, 503);
    }) as typeof fetch;
    setVisibility("hidden");
    const root4 = createRoot(host);
    await React.act(async () => {
      root4.render(React.createElement(LocaleProvider, null, React.createElement(ScreenOutlet, {
        projection: makeProjection(digestV1),
        fallback: React.createElement("div", null, "fallback"),
        renderFailure: (failure, retry) => React.createElement(ScreenFailurePane, { failure, appName: "WhatsApp", onRetry: retry }),
      })));
    });
    const attempts = () => hiddenMounts.attempts;
    for (let i = 0; i < 10 && !host.querySelector('[data-screen-pane="refused"]'); i += 1) await flush();
    assert(attempts() === 1, "initial hidden mount runs once");

    await React.act(async () => { setVisibility("visible"); });
    for (let i = 0; i < 10 && attempts() < 2; i += 1) await flush();
    for (let i = 0; i < 10 && !host.querySelector('[data-screen-pane="refused"]'); i += 1) await flush();
    assert(attempts() === 2, "returning to visible auto-retries hidden mount failure once");

    await React.act(async () => { setVisibility("hidden"); });
    await React.act(async () => { setVisibility("visible"); });
    await flush();
    assert(attempts() === 2, "hidden auto-retry does not repeat without manual retry");
    await React.act(async () => { root4.unmount(); });
  } finally {
    setVisibility("visible");
    globalThis.fetch = previousFetch;
    host.remove();
  }

  console.log("screen mount resilience checks passed");
}

void main().catch((error: unknown) => {
  console.error(error);
  process.exit(1);
});
