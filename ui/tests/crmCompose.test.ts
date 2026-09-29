import {
  FORBIDDEN_CONTENT_KEYS,
  contentClient,
  contentPaths,
} from "../src/features/app-shell/contentClient";
import { ApiError } from "../src/lib/api";

declare function require(name: string): any;

export {};
/** CAD-782: content paths bind the URL scope and bodies stay grammatical. */

function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
  const scope = { installId: "install-a", contextId: "ctx-b" };
  equal(
    contentPaths.savePath(scope),
    "/api/app-installations/install-a/contexts/ctx-b/content/campaigns",
    "save path binds the URL scope",
  );
  equal(
    contentPaths.showPath(scope, "launch-1"),
    "/api/app-installations/install-a/contexts/ctx-b/content/campaigns/launch-1",
    "show path binds install, context and campaign",
  );
  equal(
    contentPaths.renderPath(scope, "launch-1"),
    "/api/app-installations/install-a/contexts/ctx-b/content/campaigns/launch-1/render",
    "render path binds the campaign",
  );
  equal(
    contentPaths.approvePath(scope, "launch-1"),
    "/api/app-installations/install-a/contexts/ctx-b/content/campaigns/launch-1/approve",
    "approve path binds the campaign",
  );
  equal(
    contentPaths.testPreparePath(scope, "launch-1"),
    "/api/app-installations/install-a/contexts/ctx-b/content/campaigns/launch-1/test-prepare",
    "test-prepare path binds the campaign",
  );
  equal(
    contentPaths.sendPreparePath(scope, "launch-1"),
    "/api/app-installations/install-a/contexts/ctx-b/content/campaigns/launch-1/send-prepare",
    "send-prepare path binds the campaign",
  );
  equal(
    contentPaths.proposePath(scope),
    "/api/app-installations/install-a/contexts/ctx-b/content/proposals",
    "propose path binds the URL scope",
  );
  equal(
    contentPaths.proposalApplyPath(scope, "prop-1"),
    "/api/app-installations/install-a/contexts/ctx-b/content/proposals/prop-1/apply",
    "apply path binds the proposal",
  );
  equal(
    contentPaths.proposalDiscardPath(scope, "prop-1"),
    "/api/app-installations/install-a/contexts/ctx-b/content/proposals/prop-1/discard",
    "discard path binds the proposal",
  );
  equal(
    contentPaths.bindingSavePath(scope),
    "/api/app-installations/install-a/contexts/ctx-b/content/sender-bindings",
    "binding save path binds the URL scope",
  );
  equal(
    contentPaths.bindingListPath(scope),
    "/api/app-installations/install-a/contexts/ctx-b/content/sender-bindings/list",
    "binding list path binds the URL scope",
  );
  equal(
    contentPaths.bindingPath(scope, "bind-1"),
    "/api/app-installations/install-a/contexts/ctx-b/content/sender-bindings/bind-1",
    "binding path binds the binding",
  );
  // No identity, routing or discovery-link key may travel from the browser.
  for (const key of [
    "by",
    "actor",
    "workspace",
    "project",
    "project_link",
    "install_id",
    "context_id",
  ]) {
    assert(
      (FORBIDDEN_CONTENT_KEYS as readonly string[]).includes(key),
      `content client forgot forbidden key ${key}`,
    );
  }
  // The client mirrors the peer grammar: save/propose/apply bodies
  // refuse unknown fields before a byte is sent.
  let refused = 0;
  const realFetch = (globalThis as any).fetch;
  (globalThis as any).fetch = () =>
    Promise.resolve({ ok: true, json: () => Promise.resolve({}) });
  try {
    // Monkey-patch the body guard through a forged call shape: the
    // client's assertClean throws ApiError 400 client-side.
    for (const call of [
      () =>
        (contentClient as any).save(scope, {
          campaignId: "launch-1",
          subject: "S",
          blocks: [],
          by: "operator",
        }),
    ]) {
      try {
        await call();
      } catch (error) {
        assert(
          error instanceof ApiError && (error as ApiError).status === 400,
          "forged content body did not refuse client-side",
        );
        refused += 1;
      }
    }
    assert(refused === 1, "forged content body reached the network");
  } finally {
    (globalThis as any).fetch = realFetch;
  }
  // Every POST carries the board write guard header alongside
  // Content-Type, and the tab session header still passes through.
  const seen: { path: string; init: RequestInit }[] = [];
  (globalThis as any).fetch = (input: unknown, init?: RequestInit) => {
    seen.push({ path: String(input), init: init ?? {} });
    return Promise.resolve({
      ok: true,
      json: () =>
        Promise.resolve({
          render: { html: "<h1>H</h1>", text: "H", revision: 1, preview_only: true },
        }),
    });
  };
  try {
    await contentClient.render(scope, "launch-1", { sampleFirstName: "Ada" });
    await contentClient.proposalDiscard(scope, "prop-1");
    assert(seen.length === 2, "two guarded posts reached the wire");
    for (const call of seen) {
      const headers = call.init.headers as Record<string, string>;
      equal(headers["Content-Type"], "application/json", "post keeps its content type");
      equal(headers["X-Cadence-Board"], "1", "post carries the board write guard");
    }
    // A signed-in tab keeps its session header next to the guard.
    (globalThis as any).sessionStorage = {
      getItem: () => "tab-key",
      setItem: () => {},
      removeItem: () => {},
    };
    await contentClient.render(scope, "launch-1", {});
    const guarded = seen[2].init.headers as Record<string, string>;
    equal(guarded["X-Cadence-Board"], "1", "guard survives alongside a session");
    equal(guarded["X-Cadence-Session"], "tab-key", "session header still passes through");
    delete (globalThis as any).sessionStorage;
  } finally {
    (globalThis as any).fetch = realFetch;
  }
  // Preview stores both render forms: switching the HTML/Text tab
  // after Preview re-selects at display instead of showing stale
  // bytes from the tab that was active during the fetch.
  await composeTabSwitch();
}

// The tests directory runs each compiled file directly; mirror the
// sibling suites' shape.
void main();

async function composeTabSwitch(): Promise<void> {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/" });
  for (const name of [
    "window",
    "document",
    "Node",
    "Element",
    "HTMLElement",
    "HTMLInputElement",
    "HTMLTextAreaElement",
    "SVGElement",
    "navigator",
    "MutationObserver",
    "Event",
    "MouseEvent",
    "KeyboardEvent",
    "location",
    "history",
    "sessionStorage",
  ]) {
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : (win as any)[name],
      configurable: true,
      writable: true,
    });
  }
  Object.defineProperty(globalThis, "crypto", {
    value: require("crypto").webcrypto,
    configurable: true,
  });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  (globalThis as any).fetch = () =>
    Promise.resolve({
      ok: true,
      json: () =>
        Promise.resolve({
          render: {
            html: "<h1>HTML form</h1>",
            text: "TEXT form",
            revision: 1,
            preview_only: true,
          },
        }),
    });
  const React = require("react") as typeof import("react");
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const CrmCompose = require("../src/features/app-shell/CrmCompose").default as typeof import("../src/features/app-shell/CrmCompose").default;
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const flush = () =>
    React.act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  await React.act(async () => {
    root.render(
      React.createElement(CrmCompose, {
        scope: { installId: "install-a", contextId: "ctx-b" },
        campaignId: "launch-1",
      }),
    );
  });
  await flush();
  const button = (label: string) =>
    Array.from(host.querySelectorAll("button")).find((el) => el.textContent === label);
  const shown = () => host.querySelector("pre[data-preview]")?.textContent ?? "";
  await React.act(async () => {
    button("Preview")!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await flush();
  assert(shown().includes("HTML form"), "preview shows the active html form");
  await React.act(async () => {
    button("Text")!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await flush();
  assert(shown().includes("TEXT form"), "switching tab after preview shows the text form");
  await React.act(async () => {
    button("HTML")!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await flush();
  assert(shown().includes("HTML form"), "switching back shows the html form");
  await React.act(async () => {
    root.unmount();
  });
}
