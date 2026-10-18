import type * as HappyDom from "happy-dom";
import type * as Directory from "../src/features/explorer/publicDirectory";
import type * as ReleasePanel from "../src/features/explorer/releasePanel";
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
  const panel = require<typeof ReleasePanel>("../src/features/explorer/releasePanel");
  const app = { id: "example-notes", app: "example-notes", title: "Example Notes", summary: "Synthetic notes", version: "1.2.3", listing: {}, digest: null, installable: false };
  const apps = directory.parsePublicList({ ok: true, data: { apps: [app] } });
  assert(apps.length === 1 && apps[0].id === app.id && apps[0].installable === false, "browse-only releases remain visible rather than disappearing from discovery");
  assert(directory.parsePublicList({ ok: true, data: { apps: [] } }).length === 0, "an explicit empty catalogue remains an empty answer");
  for (const invalid of [{}, { ok: true, data: {} }, { ok: true, data: { apps: [app, app] } }, { ok: true, data: { apps: [{ ...app, installable: true }] } }]) {
    let refused = false;
    try { directory.parsePublicList(invalid); } catch { refused = true; }
    assert(refused, "malformed, duplicate or falsely installable data must not become an empty/successful catalogue");
  }

  // The versions document is raw data (app_id/app/recommended_version/
  // releases) — never the ok/data envelope the list and detail use.
  const digest = `sha256:${"ab".repeat(32)}`;
  const release = { version: "1.2.3", digest, installable: true, unavailableReason: null, release_notes: null };
  const versions = directory.parsePublicVersions(
    { app_id: app.id, app: app.app, recommended_version: "1.2.3", releases: [release] },
    app.id,
    app.app,
  );
  assert(versions.releases.length === 1 && versions.releases[0].version === release.version, "a well-formed versions document parses to its releases");
  for (const invalid of [
    { ok: true, data: { app_id: app.id, app: app.app, recommended_version: null, releases: [] } },
    { app_id: "another-app", app: app.app, recommended_version: null, releases: [] },
    { app_id: app.id, app: app.app, recommended_version: null, releases: [{ ...release, installable: false }] },
    { app_id: app.id, app: app.app, recommended_version: null, releases: [{ ...release, digest: "sha256:nope" }] },
  ]) {
    let refused = false;
    try { directory.parsePublicVersions(invalid, app.id, app.app); } catch { refused = true; }
    assert(refused, "envelope-wrapped, foreign-identity, self-contradicting or bad-digest versions are refused");
  }

  // The release fetch pins the exact selected release and fails closed
  // when the staged bytes do not match it.
  const installableApp = { ...app, digest, installable: true };
  const selectedRelease = { version: "1.2.3", digest, installable: true, unavailableReason: null, release_notes: null };
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
    throw new Error(`unexpected fetch ${url}`);
  }) as typeof fetch;
  const files = await panel.loadReleaseFiles(app.id, selectedRelease, new AbortController().signal);
  assert(files["app.md"] === "guide", "the staged release reads as a file bundle");
  for (const body of [{ files: {}, expected_digest: digest }, { files: { "app.md": "guide" }, expected_digest: `sha256:${"cd".repeat(32)}` }, { files: { "app.md": 7 }, expected_digest: digest }]) {
    globalThis.fetch = (async () => new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } })) as typeof fetch;
    let refused = false;
    try { await panel.loadReleaseFiles(app.id, selectedRelease, new AbortController().signal); } catch { refused = true; }
    assert(refused, "an empty bundle, a digest mismatch or non-text files never reach the check");
  }

  // A checked install applies once, pins the locator, and only succeeds
  // when the committed row and a readback both name the release.
  const receipt: ReleasePanel.Receipt = {
    kind: "install",
    release: selectedRelease,
    files: { "app.md": "guide" },
    install: null,
    expectedDigest: null,
    expectedGeneration: null,
    proposal: {} as ReleasePanel.Receipt["proposal"],
    compatible: true,
    secrets: [],
  };
  const installedFields = {
    title: app.title, summary: app.summary, project_link: null,
    catalog_generation: "synthetic-generation-1", storage_kind: "workspace",
    approval: { state: "unapproved" }, approved: false, executable: false,
  };
  seen.length = 0;
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    seen.push(`${url} ${init?.method ?? "GET"}`);
    if (url.endsWith("/api/app-installations/upload")) {
      const body = JSON.parse(String(init?.body)) as { expected_digest?: string; catalogue?: { id: string; version: string; digest: string } };
      assert(body.expected_digest === digest, "the upload pins the release digest");
      assert(body.catalogue?.id === installableApp.id && body.catalogue.version === "1.2.3" && body.catalogue.digest === digest, "the upload names the catalogue release");
      return new Response(JSON.stringify({
        ...installedFields,
        install_id: "install-1", name: app.app, version: "1.2.3", digest,
        catalogue: { id: app.id, version: "1.2.3", digest },
      }), { status: 200, headers: { "Content-Type": "application/json" } });
    }
    if (url.endsWith("/api/app-installations/install-1")) {
      return new Response(JSON.stringify({
        ...installedFields,
        install_id: "install-1", name: app.app, version: "1.2.3", digest,
        catalogue: { id: app.id, version: "1.2.3", digest },
      }), { status: 200, headers: { "Content-Type": "application/json" } });
    }
    throw new Error(`unexpected fetch ${url}`);
  }) as typeof fetch;
  const row = await panel.applyCheckedRelease(installableApp, selectedRelease, receipt);
  assert(row.install_id === "install-1", "a pinned catalogue apply returns the committed row");
  assert(seen.filter((line) => line.includes("/upload")).length === 1, "a successful catalogue apply uploads once");

  // A committed row that does not name the catalogue release is refused
  // and the apply is never retried.
  let posted = 0;
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    if (init?.method === "POST") posted += 1;
    if (url.endsWith("/api/app-installations/upload")) {
      return new Response(JSON.stringify({ ...installedFields, install_id: "install-2", name: app.app, version: "1.2.3", digest }), { status: 200, headers: { "Content-Type": "application/json" } });
    }
    if (url.endsWith("/api/app-installations/install-2")) {
      return new Response(JSON.stringify({ ...installedFields, install_id: "install-2", name: app.app, version: "1.2.3", digest }), { status: 200, headers: { "Content-Type": "application/json" } });
    }
    throw new Error(`unexpected fetch ${url}`);
  }) as typeof fetch;
  let refused = false;
  try { await panel.applyCheckedRelease(installableApp, selectedRelease, receipt); } catch { refused = true; }
  assert(refused, "a row without the catalogue locator is not treated as applied");
  assert(posted === 1, "a mismatched apply is never retried");

  // A readback that disagrees with the committed row is refused the same way.
  posted = 0;
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    if (init?.method === "POST") posted += 1;
    if (url.endsWith("/api/app-installations/upload")) {
      return new Response(JSON.stringify({
        ...installedFields,
        install_id: "install-3", name: app.app, version: "1.2.3", digest,
        catalogue: { id: app.id, version: "1.2.3", digest },
      }), { status: 200, headers: { "Content-Type": "application/json" } });
    }
    if (url.endsWith("/api/app-installations/install-3")) {
      return new Response(JSON.stringify({ ...installedFields, install_id: "install-3", name: app.app, version: "1.2.3", digest }), { status: 200, headers: { "Content-Type": "application/json" } });
    }
    throw new Error(`unexpected fetch ${url}`);
  }) as typeof fetch;
  refused = false;
  try { await panel.applyCheckedRelease(installableApp, selectedRelease, receipt); } catch { refused = true; }
  assert(refused, "a readback without the catalogue locator is not treated as applied");
  assert(posted === 1, "a mismatched readback is never retried");

  globalThis.fetch = (async () => new Response(JSON.stringify({ ok: true, data: { app: { ...app, id: "another-app" } } }), { status: 200 })) as typeof fetch;
  refused = false;
  try { await directory.loadPublicApp(app.id, new AbortController().signal); } catch { refused = true; }
  assert(refused, "a detail response for another identity is not the requested app");
  console.log("public directory boundary checks passed");
}
void main().catch((error) => { console.error(error); throw error; });
