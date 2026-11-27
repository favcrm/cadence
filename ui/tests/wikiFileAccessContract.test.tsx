/** CAD-2876: consume the shared hosted managed-file privacy wire fixture. */
import { decideAccess, FileAccessError, listAccessRequests, requestAccess, shareFile } from "../src/features/wiki/fileAccess";
import type * as ReactModule from "react";
import type * as ReactDomModule from "react-dom/client";
import type * as ViewsModule from "../src/features/wiki/FileAccessViews";

declare function require(name: string): unknown;
declare const process: { cwd(): string; exitCode?: number };
const fixture = JSON.parse((require("fs") as { readFileSync(path: string, encoding: string): string }).readFileSync(`${process.cwd()}/../tests/fixtures/agenticos/wiki-file-access.contract.json`, "utf8"));

function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}
async function rejects(fn: () => Promise<unknown>, message: string) {
  try { await fn(); } catch { return; }
  throw new Error(message);
}

async function run() {
  const originalFetch = globalThis.fetch;
  Object.defineProperty(globalThis, "localStorage", { configurable: true, value: { getItem: () => null, setItem: () => {} } });
  let response: unknown = fixture.requests;
  let httpStatus = 200;
  const calls: { path: string; method: string; body: unknown }[] = [];
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    calls.push({ path: String(input), method: init?.method ?? "GET", body: init?.body ? JSON.parse(String(init.body)) : undefined });
    return Response.json(response, { status: httpStatus });
  }) as typeof fetch;
  try {
    const rows = await listAccessRequests();
    assert(calls[0].path === "/api/wiki/file-access/requests" && calls[0].method === "GET", "list uses the hosted endpoint");
    assert(rows.length === 2 && rows[0].id === "art-example" && rows[0].title === "Project brief" && rows[1].title === undefined, "list preserves request identity and optional title");
    response = fixture.empty_requests;
    assert((await listAccessRequests()).length === 0, "an empty request list is supported");
    for (const status of ["pending", "granted", "declined", "revoked", "available"]) {
      response = fixture[`request_${status}`];
      assert(await requestAccess("file /1", 2) === status, `request accepts ${status}`);
      const sent = calls[calls.length - 1];
      assert(sent.path === "/api/wiki/file-access/file%20%2F1/2/request" && sent.method === "POST" && sent.body === undefined, "request targets the exact version without a body");
    }
    response = fixture.share_preview;
    const preview = await shareFile("file /1", 2, "ALPHA-1");
    assert(preview.status === "preview" && preview.ticket === "ALPHA-1" && preview.audience.visibility === "private" && preview.audience.revision === "audience-1", "preview shows the destination and audience");
    let sent = calls[calls.length - 1];
    assert(sent.path === "/api/wiki/file-access/file%20%2F1/2/share" && sent.method === "POST" && JSON.stringify(sent.body) === JSON.stringify({ ticket: "ALPHA-1" }), "preview never sends consent");
    response = fixture.share_confirmed;
    const shared = await shareFile("file /1", 2, "ALPHA-1", preview.audience);
    assert(shared.status === "shared", "confirmation accepts a shared response");
    sent = calls[calls.length - 1];
    assert(JSON.stringify(sent.body) === JSON.stringify({ ticket: "ALPHA-1", audience: preview.audience }), "confirmation binds the previewed audience");
    response = fixture.share_workspace_preview;
    const workspace = await shareFile("file /1", 2, "BETA-1");
    assert(workspace.audience.visibility === "workspace" && workspace.audience.revision === null, "workspace audience can have a null revision");
    for (const [decision, status] of [["grant", "granted"], ["decline", "declined"], ["revoke", "revoked"]] as const) {
      response = fixture[`decision_${status}`];
      await decideAccess(rows[0], decision);
      sent = calls[calls.length - 1];
      assert(sent.path === "/api/wiki/file-access/art-example/2/decision" && sent.method === "POST" && JSON.stringify(sent.body) === JSON.stringify({ requester: "member-example", decision }), "decision is bound to the request and requester");
    }
    response = { status: "preview", ticket: "OTHER-1", audience: fixture.share_preview.audience };
    await rejects(() => shareFile("file /1", 2, "ALPHA-1"), "a preview for a different ticket must be refused");
    response = { ...fixture.share_confirmed, audience: { ...fixture.share_confirmed.audience, revision: "changed" } };
    await rejects(() => shareFile("file /1", 2, "ALPHA-1", preview.audience), "a confirmation for a different audience must be refused");
    response = { status: "pending" };
    await rejects(() => decideAccess(rows[0], "grant"), "a decision without the matching status must be refused");
    httpStatus = 403;
    response = fixture.refused;
    let denied = false;
    try { await requestAccess("file /1", 2); } catch (error) { denied = error instanceof FileAccessError && error.status === 403; }
    assert(denied, "HTTP refusal remains a refusal, not a successful request");
    httpStatus = 503;
    await rejects(() => listAccessRequests(), "unavailable policy must not enable actions");
    httpStatus = 200;

    // The capability is the successful, validated list response, not a truthy object.
    const { Window } = require("happy-dom") as { Window: new (options: { url: string }) => Record<string, unknown> };
    const win = new Window({ url: "http://localhost/" });
    for (const name of ["window", "document", "Node", "Element", "HTMLElement", "navigator", "MutationObserver", "Event"])
      Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
    Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true, configurable: true });
    const loader = require("module") as { prototype: { require: (id: string) => unknown } }, originalRequire = loader.prototype.require;
    loader.prototype.require = function (this: unknown, id: string) {
      if (id.endsWith(".css")) return {};
      if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
      if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
      return originalRequire.call(this, id);
    };
    const React = require("react") as typeof ReactModule;
    const { createRoot } = require("react-dom/client") as typeof ReactDomModule;
    const { useFileAccessSupport } = require("../src/features/wiki/FileAccessViews") as typeof ViewsModule;
    function Capability() { return React.createElement("span", null, useFileAccessSupport() ? "enabled" : "disabled"); }
    const host = document.createElement("div");
    document.body.append(host);
    // The test target is ES2023, before Promise.withResolvers.
    const flush = () => React.act(async () => { await new Promise<void>((resolve) => setTimeout(resolve, 0)); });
    for (const malformed of [{ requests: "yes" }, { requests: [{ ...fixture.requests.requests[0], version: "2" }] }, { requests: [{ ...fixture.requests.requests[0], status: "yes" }] }]) {
      response = malformed;
      const root = createRoot(host);
      await React.act(async () => { root.render(React.createElement(Capability)); });
      await flush();
      assert(host.textContent === "disabled", "malformed capability response cannot enable actions");
      await React.act(async () => { root.unmount(); });
    }
    response = fixture.empty_requests;
    const root = createRoot(host);
    await React.act(async () => { root.render(React.createElement(Capability)); });
    await flush();
    assert(host.textContent === "enabled", "a valid empty capability response enables the feature");
    await React.act(async () => { root.unmount(); });
    host.remove();
    loader.prototype.require = originalRequire;
  } finally {
    globalThis.fetch = originalFetch;
    Reflect.deleteProperty(globalThis, "localStorage");
  }
}
void run().catch((error) => { console.error(error); process.exitCode = 1; });
