import type * as HappyDom from "happy-dom";
import type * as Directory from "../src/features/explorer/publicDirectory";
import type * as ReleasePanel from "../src/features/explorer/releasePanel";
import type * as PrivateSession from "../src/lib/privateSession";
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
  const session = require<typeof PrivateSession>("../src/lib/privateSession");
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
  const hostBlocked = { ...release, version: "1.3.0", installable: false, unavailableReason: "host_incompatible" };
  const withHostBlocked = directory.parsePublicVersions(
    { app_id: app.id, app: app.app, recommended_version: "1.2.3", releases: [hostBlocked, release] },
    app.id,
    app.app,
  );
  assert(withHostBlocked.releases[0].unavailableReason === "host_incompatible", "a host-incompatible release remains a readable, non-installable row");
  assert(withHostBlocked.recommended_version === "1.2.3", "the older compatible release stays recommended");
  for (const invalid of [
    { ok: true, data: { app_id: app.id, app: app.app, recommended_version: null, releases: [] } },
    { app_id: "another-app", app: app.app, recommended_version: null, releases: [] },
    { app_id: app.id, app: app.app, recommended_version: null, releases: [{ ...release, installable: false }] },
    { app_id: app.id, app: app.app, recommended_version: null, releases: [{ ...release, digest: "sha256:nope" }] },
    { app_id: app.id, app: app.app, recommended_version: null, releases: [{ ...release, installable: false, unavailableReason: "other" }] },
  ]) {
    let refused = false;
    try { directory.parsePublicVersions(invalid, app.id, app.app); } catch { refused = true; }
    assert(refused, "envelope-wrapped, foreign-identity, self-contradicting or bad-digest versions are refused");
  }

  // CAD-1942: catalogue check and apply name the release; the board fetches
  // and verifies its bytes server-side, so the browser never downloads the
  // release (the digest-mismatch refusal moved to the board, src/ui/apps.rs).
  const installableApp = { ...app, digest, installable: true };
  const selectedRelease = { version: "1.2.3", digest, installable: true, unavailableReason: null, release_notes: null };
  const seen: string[] = [];
  assert(!("loadReleaseFiles" in panel), "the browser no longer reads release files");

  // A checked install applies once, pins the locator, and only succeeds
  // when the committed row and a readback both name the release.
  const receipt: ReleasePanel.Receipt = {
    kind: "install",
    sessionGeneration: session.privateSessionGeneration(),
    release: selectedRelease,
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
      assert(!("files" in body), "a catalogue apply sends the locator, never the package files");
      assert(body.expected_digest === digest, "the upload pins the release digest");
      assert(body.catalogue?.id === installableApp.id && body.catalogue.version === "1.2.3" && body.catalogue.digest === digest, "the upload names the catalogue release");
      return new Response(JSON.stringify({
        ...installedFields,
        committed: true,
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
  assert(!seen.some((line) => line.includes("/release?")), "the apply never downloads the release");

  // A catalogue update apply carries the locator and pins only.
  let upgradeBody: Record<string, unknown> | null = null;
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    if (url.endsWith("/api/app-installations/install-1/upgrade")) {
      upgradeBody = JSON.parse(String(init?.body)) as Record<string, unknown>;
      return new Response(JSON.stringify({ error: "synthetic stop" }), { status: 500, headers: { "Content-Type": "application/json" } });
    }
    throw new Error(`unexpected fetch ${url}`);
  }) as typeof fetch;
  const updateReceipt: ReleasePanel.Receipt = {
    ...receipt,
    kind: "update",
    install: { install_id: "install-1" } as ReleasePanel.Receipt["install"],
    expectedDigest: `sha256:${"cd".repeat(32)}`,
    expectedGeneration: "synthetic-generation-1",
  };
  try { await panel.applyCheckedRelease(installableApp, selectedRelease, updateReceipt); } catch { /* the synthetic 500 */ }
  const sentUpgrade = upgradeBody as Record<string, unknown> | null;
  assert(sentUpgrade !== null && !("files" in sentUpgrade), "a catalogue update sends no package files");
  assert(sentUpgrade.expected_new_digest === digest && (sentUpgrade.catalogue as { digest?: string })?.digest === digest, "a catalogue update pins the locator and the new digest");

  // Identity readback cannot replace the mutation's own commit acknowledgement.
  for (const committed of [false, undefined]) {
    let attempts = 0;
    globalThis.fetch = (async (_input: RequestInfo | URL, init?: RequestInit) => {
      if (init?.method === "POST") attempts += 1;
      return new Response(JSON.stringify({
        ...installedFields, committed,
        install_id: "unconfirmed", name: app.app, version: "1.2.3", digest,
        catalogue: { id: app.id, version: "1.2.3", digest },
      }), { status: 200, headers: { "Content-Type": "application/json" } });
    }) as typeof fetch;
    let rejected = false;
    try { await panel.applyCheckedRelease(installableApp, selectedRelease, receipt); } catch { rejected = true; }
    assert(rejected, "missing or negative commit acknowledgement is not an installation success");
    assert(attempts === 1, "an unconfirmed commit is never retried");
  }

  // A committed row that does not name the catalogue release is refused
  // and the apply is never retried.
  let posted = 0;
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    if (init?.method === "POST") posted += 1;
    if (url.endsWith("/api/app-installations/upload")) {
      return new Response(JSON.stringify({ ...installedFields, committed: true, install_id: "install-2", name: app.app, version: "1.2.3", digest }), { status: 200, headers: { "Content-Type": "application/json" } });
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
        committed: true,
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
  // A completed review belongs to the private session that performed it.
  session.resetPrivateSession();
  session.allowPrivateReads();
  let staleWrites = 0;
  globalThis.fetch = (async (_input: RequestInfo | URL, init?: RequestInit) => {
    if (init?.method === "POST") staleWrites += 1;
    return new Response(JSON.stringify({
      ...installedFields, committed: true,
      install_id: "stale-session", name: app.app, version: "1.2.3", digest,
      catalogue: { id: app.id, version: "1.2.3", digest },
    }), { status: 200, headers: { "Content-Type": "application/json" } });
  }) as typeof fetch;
  refused = false;
  try { await panel.applyCheckedRelease(installableApp, selectedRelease, receipt); } catch { refused = true; }
  assert(refused && staleWrites === 0, "a previous session's review never authorizes a new session's write");


  globalThis.fetch = (async () => new Response(JSON.stringify({ ok: true, data: { app: { ...app, id: "another-app" } } }), { status: 200 })) as typeof fetch;
  refused = false;
  try { await directory.loadPublicApp(app.id, new AbortController().signal); } catch { refused = true; }
  assert(refused, "a detail response for another identity is not the requested app");
  console.log("public directory boundary checks passed");
}
void main().catch((error) => { console.error(error); throw error; });
