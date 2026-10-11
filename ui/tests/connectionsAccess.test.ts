/**
 * CAD-1319 independent acceptance check (bad case): an UNKNOWN operator
 * proof — `viewer.operator === null` — grants nothing on ANY of the four
 * real ConnectionPage variants (index, add, addProvider, detail). The
 * default export routes every unknown-access page through the index's
 * checking/unavailable state with canWrite=false, so no private resource
 * read, credential form, or mutation action can mount.
 *
 * Asserts, against the real exported Connections component mounted in
 * HappyDOM (same rig as connectionsSmtp.test.ts):
 *  - zero GET /api/connections or /api/connection-providers requests, and
 *    zero mutation requests, for every unknown variant;
 *  - no password/secret input, consent box, or mutation action renders;
 *  - no false "not found" and no "Operator access required" sign-in
 *    misclaim while the proof is still unknown;
 *  - "checking" vs "unavailable" copy is distinct and honest, and the
 *    Retry button calls only the provided onRetryAccess callback — it
 *    grants nothing itself;
 *  - positive/negative proof: operator:true renders the normal index,
 *    operator:false gets the operator-required state, and a read-only
 *    operator still gets the Read-only board state on the add route.
 */
declare function require(name: string): any;
import type { act as reactAct } from "react";

function assert(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(`assert: ${msg}`);
}
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
let act: typeof reactAct;
async function settle(fn: () => void, ms = 15000) {
  const end = Date.now() + ms;
  for (;;) {
    try {
      fn();
      return;
    } catch (e) {
      if (Date.now() > end) throw e;
      await act(async () => sleep(25));
    }
  }
}

type Viewer = {
  operator: boolean | null;
  readOnly: boolean;
  access?: "checking" | "unavailable" | null;
  onRetryAccess?: () => void;
};
type Page = { kind: "add" } | { kind: "addProvider"; provider: string } | { kind: "detail"; id: string };

const PAGES: { name: string; page?: Page }[] = [
  { name: "index", page: undefined },
  { name: "add", page: { kind: "add" } },
  { name: "addProvider", page: { kind: "addProvider", provider: "smtp" } },
  { name: "detail", page: { kind: "detail", id: "c-unknown" } },
];

const PRIVATE_READ = /\/api\/(connections|connection-providers)\b/;
const MUTATION = /\/api\/(connections|connection-providers)/;

const FORBIDDEN_TEXT = [
  "Operator access required",
  "not found",
  "not on this board",
  "Add connection",
  "Save connection",
  "Update credentials",
  "Disconnect",
  "Replace credential",
  "Check configuration",
  "Set up",
];

async function main() {
  // HappyDOM + loader stubs, identical to the other mounted suites.
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/settings/connections" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLSelectElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener"])
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
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
  const { default: Connections } = require("../src/features/settings/Connections") as typeof import("../src/features/settings/Connections");

  // Record every request; a private read or any mutation while the
  // operator proof is unknown fails the check.
  const calls: { url: string; method: string }[] = [];
  const innerFetch = globalThis.fetch;
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = String(input);
    calls.push({ url, method: init?.method ?? "GET" });
    if (url.endsWith("/api/connection-providers")) {
      return new Response(JSON.stringify({ providers: [] }), { status: 200, headers: { "Content-Type": "application/json" } });
    }
    if (url.endsWith("/api/connections")) {
      return new Response(JSON.stringify({ connections: [] }), { status: 200, headers: { "Content-Type": "application/json" } });
    }
    return new Response(JSON.stringify({ error: "not stubbed" }), { status: 404, headers: { "Content-Type": "application/json" } });
  }) as typeof fetch;

  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);
  const text = () => host.textContent ?? "";

  const noPrivateCalls = (since: number, what: string) => {
    const made = calls.slice(since);
    assert(
      !made.some((c) => PRIVATE_READ.test(c.url)),
      `${what}: a private connections read was issued: ${JSON.stringify(made)}`,
    );
    assert(
      !made.some((c) => c.method !== "GET" && MUTATION.test(c.url)),
      `${what}: a mutation request was issued: ${JSON.stringify(made)}`,
    );
  };
  const noForbiddenUi = (what: string) => {
    assert(host.querySelectorAll('input[type="password"]').length === 0, `${what}: a secret input rendered`);
    assert(host.querySelectorAll("form").length === 0, `${what}: a form rendered`);
    assert(!host.querySelector("[data-risk-needed]"), `${what}: the storage-consent control rendered`);
    for (const bad of FORBIDDEN_TEXT) {
      assert(!text().includes(bad), `${what}: forbidden text "${bad}" rendered`);
    }
  };

  // ---- Bad case: operator === null, access "checking" ----
  for (const { name, page } of PAGES) {
    const since = calls.length;
    await act(async () => {
      root.render(React.createElement(Connections, {
        viewer: { operator: null, readOnly: false, access: "checking" } satisfies Viewer,
        page,
      }));
    });
    await settle(() => {
      assert(text().includes("Checking access"), `${name}/checking: the checking state renders`);
      assert(text().includes("still proving this session"), `${name}/checking: honest in-flight copy`);
    });
    noPrivateCalls(since, `${name}/checking`);
    noForbiddenUi(`${name}/checking`);
    assert(!text().includes("Access could not be confirmed"), `${name}/checking: not mislabelled unavailable`);
    assert(!host.querySelector("button"), `${name}/checking: no retry or action button without an unavailable proof`);
  }

  // ---- Bad case: operator === null, access "unavailable" ----
  let retries = 0;
  const onRetryAccess = () => {
    retries += 1;
  };
  for (const { name, page } of PAGES) {
    const since = calls.length;
    await act(async () => {
      root.render(React.createElement(Connections, {
        viewer: { operator: null, readOnly: false, access: "unavailable", onRetryAccess } satisfies Viewer,
        page,
      }));
    });
    await settle(() => {
      assert(text().includes("Access could not be confirmed"), `${name}/unavailable: the unavailable state renders`);
      assert(text().includes("did not answer"), `${name}/unavailable: honest no-answer copy`);
      assert(text().includes("nothing is shown"), `${name}/unavailable: refuses to show data on an unanswered check`);
    });
    noPrivateCalls(since, `${name}/unavailable`);
    noForbiddenUi(`${name}/unavailable`);
    assert(!text().includes("Checking access"), `${name}/unavailable: not mislabelled checking`);

    // The retry control invokes only the provided callback — it grants
    // nothing itself: after the click no private read fires and no
    // privileged UI appears.
    const retryBtn = Array.from(host.querySelectorAll("button")).find((b) =>
      (b.textContent ?? "").includes("Retry access check"),
    ) as HTMLButtonElement | undefined;
    assert(retryBtn, `${name}/unavailable: the provided retry action renders`);
    const before = retries;
    const sinceRetry = calls.length;
    await act(async () => retryBtn!.click());
    assert(retries === before + 1, `${name}/unavailable: retry calls the provided onRetryAccess exactly once`);
    await act(async () => sleep(50));
    noPrivateCalls(sinceRetry, `${name}/unavailable post-retry`);
    noForbiddenUi(`${name}/unavailable post-retry`);
    assert(text().includes("Access could not be confirmed"), `${name}/unavailable: retry alone never grants`);
  }

  // Without a provided onRetryAccess the unavailable state offers no
  // fake retry control.
  await act(async () => {
    root.render(React.createElement(Connections, {
      viewer: { operator: null, readOnly: true, access: "unavailable" } satisfies Viewer,
      page: { kind: "detail", id: "c-unknown" },
    }));
  });
  await settle(() => assert(text().includes("Access could not be confirmed"), "unavailable without callback renders"));
  assert(
    !Array.from(host.querySelectorAll("button")).some((b) => (b.textContent ?? "").includes("Retry access check")),
    "no retry button is invented when onRetryAccess is absent",
  );

  // ---- Negative proof: operator === false gets the operator-required
  // state (not the unknown checking/unavailable state). ----
  {
    const since = calls.length;
    await act(async () => {
      root.render(React.createElement(Connections, {
        viewer: { operator: false, readOnly: false } satisfies Viewer,
        page: { kind: "detail", id: "c-unknown" },
      }));
    });
    await settle(() => assert(text().includes("Operator access required"), "operator:false gets the refused state"));
    noPrivateCalls(since, "operator:false");
    assert(!text().includes("Checking access"), "operator:false is not the checking state");
  }

  // ---- Positive proof: a proven operator renders the normal index. ----
  {
    await act(async () => {
      root.render(React.createElement(Connections, {
        viewer: { operator: true, readOnly: false } satisfies Viewer,
      }));
    });
    await settle(() => {
      assert(text().includes("Connections"), "operator:true renders the index");
      assert(text().includes("Add connection"), "operator:true writable shows the add action");
      assert(!text().includes("Checking access"), "operator:true is not gated as unknown");
    });
    assert(calls.some((c) => c.url.endsWith("/api/connections")), "operator:true issues the connections read");
  }

  // ---- Read-only preserved: a proven operator on a read-only board
  // still gets the Read-only state and no form on the add route. ----
  {
    await act(async () => {
      root.render(React.createElement(Connections, {
        viewer: { operator: true, readOnly: true } satisfies Viewer,
        page: { kind: "add" },
      }));
    });
    await settle(() => {
      assert(text().includes("Read-only board"), "operator+readOnly keeps the read-only guard");
      assert(host.querySelectorAll('input[type="password"]').length === 0, "read-only mounts no secret input");
    });
  }

  globalThis.fetch = innerFetch;
  await act(async () => root.unmount());
  console.log("connections access guard checks passed");
}

void main();
