import type * as HappyDom from "happy-dom";
import type * as Directory from "../src/features/explorer/publicDirectory";
declare function require<T = unknown>(name: string): T;
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
async function main() {
  const { Window } = require<typeof HappyDom>("happy-dom");
  const win = new Window({ url: "http://localhost/apps/explore" });
  for (const name of ["window", "document", "navigator", "location", "history", "sessionStorage"])
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : Reflect.get(win, name), configurable: true });
  const loader = require<{ prototype: { require: (this: unknown, id: string) => unknown } }>("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.call(this, id);
  };
  const directory = require<typeof Directory>("../src/features/explorer/publicDirectory");
  const app = { id: "example-notes", app: "example-notes", title: "Example Notes", summary: "Synthetic notes", version: "1.2.3", listing: {}, digest: null, installable: false };
  const apps = directory.parsePublicList({ ok: true, data: { apps: [app] } });
  assert(apps.length === 1 && apps[0].id === app.id && apps[0].installable === false, "browse-only releases remain visible rather than disappearing from discovery");
  assert(directory.parsePublicList({ ok: true, data: { apps: [] } }).length === 0, "an explicit empty catalogue remains an empty answer");
  for (const invalid of [{}, { ok: true, data: {} }, { ok: true, data: { apps: [app, app] } }, { ok: true, data: { apps: [{ ...app, installable: true }] } }]) {
    let refused = false;
    try { directory.parsePublicList(invalid); } catch { refused = true; }
    assert(refused, "malformed, duplicate or falsely installable data must not become an empty/successful catalogue");
  }
  let calls = 0;
  globalThis.fetch = (async () => { calls++; throw new Error("browse-only app must not fetch a release"); }) as typeof fetch;
  const outcome = await directory.installPublicApp(app, new AbortController().signal);
  assert(outcome.state === "refused" && calls === 0, "a browse-only release cannot reach the install transport");
  const digest = `sha256:${"ab".repeat(32)}`;
  const installable = { ...app, version: "1.2.3", digest, installable: true };
  const seen: string[] = [];
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    seen.push(`${url} ${init?.method ?? "GET"}`);
    if (url.includes("/release?")) {
      const query = new URL(url, "http://localhost").searchParams;
      assert(query.get("version") === "1.2.3" && query.get("digest") === digest, "the release fetch pins both version and digest");
      assert(query.getAll("version").length === 1 && query.getAll("digest").length === 1, "the release query has no duplicate pins");
      return new Response(JSON.stringify({ files: { "app.md": "guide" }, expected_digest: digest }), { status: 200, headers: { "Content-Type": "application/json" } });
    }
    if (url.endsWith("/api/app-installations/upload")) {
      const body = JSON.parse(String(init?.body)) as { expected_digest?: string; catalogue?: { id: string; version: string; digest: string } };
      assert(body.expected_digest === digest, "the upload pins the release digest");
      assert(body.catalogue?.id === installable.id && body.catalogue.version === "1.2.3" && body.catalogue.digest === digest, "the upload names the catalogue release");
      return new Response(JSON.stringify({ install_id: "install-1" }), { status: 200, headers: { "Content-Type": "application/json" } });
    }
    if (url.endsWith("/api/app-installations/install-1")) {
      return new Response(JSON.stringify({
        install_id: "install-1", version: "1.2.3", digest, approval: "unapproved",
        catalogue: { id: installable.id, version: "1.2.3", digest },
      }), { status: 200, headers: { "Content-Type": "application/json" } });
    }
    throw new Error(`unexpected fetch ${url}`);
  }) as typeof fetch;
  const installed = await directory.installPublicApp(installable, new AbortController().signal);
  assert(installed.state === "installed" && installed.installId === "install-1", "a pinned catalogue install reads the recorded origin back");
  assert(seen.filter((line) => line.includes("/upload")).length === 1, "a successful catalogue install uploads once");
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    if (url.includes("/release?")) {
      return new Response(JSON.stringify({ files: { "app.md": "guide" }, expected_digest: digest }), { status: 200, headers: { "Content-Type": "application/json" } });
    }
    if (url.endsWith("/api/app-installations/upload")) {
      return new Response(JSON.stringify({ install_id: "install-2" }), { status: 200, headers: { "Content-Type": "application/json" } });
    }
    if (url.endsWith("/api/app-installations/install-2")) {
      return new Response(JSON.stringify({ install_id: "install-2", version: "1.2.3", digest, approval: "unapproved" }), { status: 200, headers: { "Content-Type": "application/json" } });
    }
    if (init?.method === "POST") throw new Error("a mismatched readback must not be retried");
    throw new Error(`unexpected fetch ${url}`);
  }) as typeof fetch;
  const mismatched = await directory.installPublicApp(installable, new AbortController().signal);
  assert(mismatched.state === "unknown", "readback without the catalogue locator is not treated as installed");
  globalThis.fetch = (async () => new Response(JSON.stringify({ ok: true, data: { app: { ...app, id: "another-app" } } }), { status: 200 })) as typeof fetch;
  let refused = false;
  try { await directory.loadPublicApp(app.id, new AbortController().signal); } catch { refused = true; }
  assert(refused, "a detail response for another identity is not the requested app");
  console.log("public directory boundary checks passed");
}
void main().catch((error) => { console.error(error); throw error; });
