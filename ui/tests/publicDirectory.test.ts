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
  globalThis.fetch = (async () => new Response(JSON.stringify({ ok: true, data: { app: { ...app, id: "another-app" } } }), { status: 200 })) as typeof fetch;
  let refused = false;
  try { await directory.loadPublicApp(app.id, new AbortController().signal); } catch { refused = true; }
  assert(refused, "a detail response for another identity is not the requested app");
  console.log("public directory boundary checks passed");
}
void main().catch((error) => { console.error(error); throw error; });
