export {};
/**
 * CAD-784 CRM Segments: audience wire grammar plus mounted
 * list/new/detail flow through the real AppShell.
 */
declare function require(name: string): any;

function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
  await wireGrammar();
  await mountedFlow();
}

async function wireGrammar() {
  const { audienceClient, audiencePaths, FORBIDDEN_AUDIENCE_KEYS } = require(
    "../src/features/app-shell/audienceClient",
  ) as typeof import("../src/features/app-shell/audienceClient");
  const { ApiError } = require("../src/lib/api") as typeof import("../src/lib/api");
  const scope = { installId: "install-a", contextId: "ctx-b" };
  equal(
    audiencePaths.segmentSavePath(scope),
    "/api/app-installations/install-a/contexts/ctx-b/segments",
    "segment save binds the URL scope",
  );
  equal(
    audiencePaths.segmentListPath(scope),
    "/api/app-installations/install-a/contexts/ctx-b/segments/list",
    "segment list binds the URL scope",
  );
  equal(
    audiencePaths.segmentPath(scope, "seg-vip"),
    "/api/app-installations/install-a/contexts/ctx-b/segments/seg-vip",
    "segment show binds the segment",
  );
  equal(
    audiencePaths.previewPath(scope),
    "/api/app-installations/install-a/contexts/ctx-b/audience/preview",
    "preview binds the URL scope",
  );
  equal(
    audiencePaths.freezePath(scope, "freeze-1"),
    "/api/app-installations/install-a/contexts/ctx-b/audience/prepares/freeze-1",
    "freeze show binds the freeze",
  );
  // Identity and receipt-shaped keys never travel from the browser.
  for (const key of [
    "by",
    "actor",
    "install_id",
    "context_id",
    "assistant_receipt",
    "turn_id",
    "nonce",
  ]) {
    assert(
      (FORBIDDEN_AUDIENCE_KEYS as readonly string[]).includes(key),
      `audience client forgot forbidden key ${key}`,
    );
  }
  // Malformed scope and forged bodies refuse client-side.
  const realFetch = (globalThis as any).fetch;
  (globalThis as any).fetch = () => {
    throw new Error("network must not be reached by a refused body");
  };
  try {
    for (const call of [
      () =>
        (audienceClient as any).segmentSave({ installId: "bad id", contextId: "ctx-b" }, {
          segmentId: "seg-1",
          name: "N",
          predicates: [],
        }),
      () =>
        (audienceClient as any).segmentSave(scope, {
          segmentId: "seg-1",
          name: "N",
          predicates: [],
          by: "operator",
        }),
      () =>
        (audienceClient as any).segmentSave(scope, {
          segmentId: "seg-1",
          name: "N",
          predicates: [],
          assistant_receipt: { turn_id: "t" },
        }),
      () => (audienceClient as any).preview(scope, { mode: "segment", segmentId: "bad id" }),
      () =>
        (audienceClient as any).preview(scope, { mode: "all" }, "bad id"),
      () =>
        (audienceClient as any).prepare(scope, {
          freezeId: "freeze-1",
          base: { mode: "custom", customerIds: [] },
          maxRecipients: 10,
        }),
    ]) {
      let refused = false;
      try {
        await call();
      } catch (error) {
        assert(error instanceof ApiError, "audience refusal is a typed ApiError");
        refused = true;
      }
      assert(refused, "forged audience body reached the network");
    }
  } finally {
    (globalThis as any).fetch = realFetch;
  }
  // Writes carry the board write guard; reads stay GET.
  const seen: { path: string; init: RequestInit }[] = [];
  (globalThis as any).fetch = (input: unknown, init?: RequestInit) => {
    seen.push({ path: String(input), init: init ?? {} });
    return Promise.resolve({ ok: true, json: () => Promise.resolve({ segments: [] }) });
  };
  try {
    await audienceClient.segmentList(scope);
    await audienceClient.segmentSave(scope, {
      segmentId: "seg-1",
      name: "VIP",
      predicates: [{ field: "tag", op: "eq", value: "vip" }],
    });
    assert(seen.length === 2, "list then save reach the wire");
    assert(
      (seen[0].init.method ?? "GET") === "GET" && seen[0].init.body === undefined,
      "segment list stays a GET with no body",
    );
    equal(
      (seen[1].init.headers as Record<string, string>)["X-Cadence-Board"],
      "1",
      "segment save carries the board write guard",
    );
  } finally {
    (globalThis as any).fetch = realFetch;
  }

  // The approved rule grammar mirrors the daemon's allowlist.
  const grammar = require("../src/features/app-shell/segmentGrammar") as typeof import(
    "../src/features/app-shell/segmentGrammar"
  );
  grammar.checkPredicate("tag", "eq", "vip");
  grammar.checkPredicate("consent_email", "ne", "granted");
  grammar.checkPredicate("email_domain", "eq", "Example.COM");
  for (const [field, op, value, why] of [
    ["tag", "eq", "has space", "bad tag"],
    ["email", "eq", "vip", "unknown field"],
    ["tag", "contains", "vip", "unknown operator"],
    ["tag", "eq", "a<b", "markup paste"],
    ["tag", "eq", "x'; DROP TABLE", "query fragment"],
    ["consent_email", "eq", "maybe", "unknown consent"],
    ["email_domain", "eq", "no-dot", "bare domain"],
  ] as [string, string, string, string][]) {
    let refused = false;
    try {
      grammar.checkPredicate(field, op, value);
    } catch (error) {
      assert(error instanceof ApiError, `${why} refuses typed`);
      refused = true;
    }
    assert(refused, `${why} passed the client grammar`);
  }
}

async function mountedFlow() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/app-installations/install-crm?ctx=ctx-a" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLSelectElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener"])
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  Object.defineProperty(globalThis, "EventSource", {
    configurable: true,
    value: class {
      onopen: null = null;
      onerror: null = null;
      constructor() {}
      addEventListener() {}
      removeEventListener() {}
      close() {}
    },
  });
  const loader = require("module"), originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, arguments);
  };
  const React = require("react") as typeof import("react");
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const AppShell = (require("../src/features/app-shell/AppShell") as typeof import("../src/features/app-shell/AppShell")).default;

  const install = {
    install_id: "install-crm", title: "CRM", name: "crm", version: "0.1.0",
    digest: "install-digest", catalog_generation: "gen-1", approved: true,
    storage_kind: "workspace", files: [], capabilities: null, connection_slots: [],
  };
  const segments: Record<string, any> = {
    "seg-vip": {
      id: "seg-vip", install_id: "install-crm", context_id: "ctx-a", revision: 1,
      name: "VIP", predicates: [{ field: "tag", op: "eq", value: "vip" }], digest: "seg-digest-1",
    },
    "seg-none": {
      id: "seg-none", install_id: "install-crm", context_id: "ctx-a", revision: 1,
      name: "No consent", predicates: [{ field: "tag", op: "eq", value: "cold" }, { field: "source", op: "ne", value: "web" }], digest: "seg-digest-n",
    },
  };
  let posts = 0;
  const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
  const refused = (message: string, status = 409) => json({ error: message }, status);
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const path = String(input);
    const url = new URL(path, "http://localhost");
    const method = init?.method ?? "GET";
    if (method === "POST" && url.pathname.endsWith("/segments")) {
      posts += 1;
      const body = JSON.parse(String(init!.body));
      const id = body.segment_id as string;
      const current = segments[id]?.revision as number | undefined;
      if (current !== undefined && body.expected_revision === undefined) {
        return refused("audience segment already exists; name the observed revision");
      }
      if (current !== undefined && body.expected_revision !== current) {
        return refused("audience segment revision is stale");
      }
      // A name of "Stale Race" simulates another operator winning the
      // race: the server's CAS refuses even a freshly observed revision.
      if (body.name === "Stale Race") {
        return refused("audience segment revision is stale");
      }
      const revision = current === undefined ? 1 : current + 1;
      segments[id] = {
        id, install_id: "install-crm", context_id: "ctx-a", revision,
        name: body.name, predicates: body.predicates, digest: `seg-digest-${revision}`,
      };
      return json({ segment: segments[id] });
    }
    if (method === "POST" && url.pathname.endsWith("/audience/preview")) {
      const pbody = JSON.parse(String(init!.body));
      if (pbody.base?.segment_id === "seg-none") {
        return json({
          base: {}, base_count: 5, exclusion_list_id: null, exclusion_count: 0,
          final_excluded: { invalid_email: 0, no_consent: 5, unsubscribed: 0, suppressed: 0 },
          final_count: 0, sample: [], digest: "audience-digest-0",
        });
      }
      return json({
        base: {}, base_count: 42, exclusion_list_id: null, exclusion_count: 0,
        final_excluded: { invalid_email: 1, no_consent: 1, unsubscribed: 0, suppressed: 0 },
        final_count: 40,
        sample: [{ id: "customer-s1", display_name: "Seed Alpha" }],
        digest: "audience-digest-1",
      });
    }
    if (path === "/api/app-installations/install-crm") return json(install);
    if (path === "/api/app-installations/install-crm/contexts") return json({ contexts: [
      { id: "ctx-a", install_id: "install-crm", revision: 1, state: "active", digest: "ca", config: { schema: 1, label: "Acme", input_defaults: {} } },
    ] });
    if (path.startsWith("/api/threads/master")) return json({ entries: [], more_before: false });
    if (url.pathname.endsWith("/records")) return json({ records: [], truncated: false, next_cursor: null });
    if (url.pathname.endsWith("/segments/list")) return json({ segments: Object.values(segments) });
    const segId = url.pathname.split("/").pop()!;
    if (url.pathname.includes("/segments/")) {
      if (segments[segId]) return json({ segment: segments[segId] });
      return refused("audience segment is unavailable for this installation and context", 404);
    }
    throw new Error(`Unexpected read ${path}`);
  }) as typeof fetch;

  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
  const text = () => host.textContent ?? "";
  async function settle(check: () => void) {
    for (let i = 0; i < 60; i++) {
      await flush();
      try { check(); return; } catch { /* keep polling */ }
    }
    await flush();
    check();
  }
  async function click(element: Element | undefined | null) {
    assert(element, "click target exists");
    await React.act(async () => { element.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
    await flush();
  }
  async function fill(selector: string, value: string) {
    const element = host.querySelector(selector) as HTMLInputElement;
    assert(element, `field exists: ${selector}`);
    await React.act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(element, value);
      element.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await flush();
  }

  win.sessionStorage.clear();
  history.pushState(null, "", "/app-installations/install-crm?ctx=ctx-a&crm=segments");
  await React.act(async () => {
    root.render(React.createElement(AppShell, { installId: "install-crm", viewer: { operator: true, readOnly: false } }));
  });
  // Direct section route: the Segments list paints server rows, with
  // no customer content and no campaign builder in sight.
  await settle(() => assert(text().includes("VIP"), "segments list paints server rows"));
  assert(host.querySelector("[data-outlet-heading]")?.textContent?.trim() === "Segments", "segments list retains its section heading");
  assert(host.querySelectorAll('a[href="/apps"]').length === 1, "only the shared shell Apps link renders");
  assert(!host.querySelector(".crm-crumb"), "segments do not duplicate the shell breadcrumb");
  assert(!host.querySelector('[data-section="campaigns"]'), "campaigns stay unmounted on the segments route");

  // New segment: malformed rules refuse before the wire.
  await click(Array.from(host.querySelectorAll("button")).find((el) => el.textContent === "New segment"));
  await settle(() => assert(host.querySelector("#seg-name"), "new segment page opens"));
  assert(location.search.includes("appview=new") && location.search.includes("crm=segments"), "new route keeps its section");
  const postsBefore = posts;
  await fill("#seg-name", "Bad rules");
  await fill("#seg-rule-value-0", "a<b");
  await click(host.querySelector('section[aria-label="New segment"] button[type="submit"]'));
  await flush();
  assert(text().includes("grammar"), "markup paste refuses with the grammar message");
  assert(posts === postsBefore, "refused rules never reach the wire");

  // Valid rules create and land atomically on the detail drawer with
  // the host's exact preview counts.
  await fill("#seg-name", "Newsletter");
  await fill("#seg-rule-value-0", "vip");
  await click(host.querySelector('section[aria-label="New segment"] button[type="submit"]'));
  await settle(() => assert(host.querySelector('[data-drawer="segment"]'), "created segment opens its drawer"));
  assert(!location.search.includes("appview="), "no stale appview=new after create");
  await settle(() => assert(text().includes("40 can be emailed") && text().includes("42 match"), "header pills show exact host counts"));
  assert(text().includes("Seed Alpha") && text().includes("Can email"), "overview previews members with a consent pill");
  assert(host.querySelector('[data-testid="rule-sentence"]')?.textContent?.includes("Customers where"), "rule renders as a sentence");
  const steps = Array.from(host.querySelectorAll('[data-testid="funnel"] li')).map((li) => li.textContent);
  assert(steps.length === 4 && steps[0]!.includes("42") && steps[1]!.includes("41") && steps[2]!.includes("40") && steps[3]!.includes("40"), "funnel steps come from preview counts: " + JSON.stringify(steps));
  assert(host.querySelector('[data-testid="funnel-reason"]')?.textContent?.includes("1 customer"), "funnel names a plain-language reason");
  assert(!text().includes("Suppressed"), "technical breakdown leaves the main path");
  const tabBtn = (name: string) => Array.from(host.querySelectorAll('[role="tab"]')).find((el) => el.textContent === name);
  await click(tabBtn("Details"));
  await settle(() => assert(text().includes("Suppressed") && text().includes("revision r1"), "Details holds the breakdown and revision"));
  await click(tabBtn("Overview"));
  await click(Array.from(host.querySelectorAll("button")).find((el) => el.textContent === "View all"));
  await settle(() => assert(host.querySelector('[aria-label="All members"]'), "View all opens the Members tab"));

  // Footer menu: Duplicate works; Export and Delete are disabled with a reason.
  await click(host.querySelector('[aria-label="More actions"]'));
  const item = (label: string) => Array.from(host.querySelectorAll('[role="menuitem"]')).find((el) => el.textContent === label) as HTMLElement;
  assert(item("Export members").getAttribute("aria-disabled") === "true" && item("Export members").title !== "", "export is disabled with a tooltip");
  assert(item("Delete segment").getAttribute("aria-disabled") === "true" && item("Delete segment").className.includes("danger"), "delete is disabled and red");
  const postsBeforeDelete = posts;
  await click(item("Delete segment"));
  assert(posts === postsBeforeDelete, "a disabled item sends nothing");
  await click(item("Duplicate"));
  await settle(() => assert(Object.values(segments).some((x: any) => x.name === "Newsletter (copy)"), "duplicate saves a copy"));
  await settle(() => assert(host.querySelector('[data-drawer="segment"] h3')?.textContent === "Newsletter (copy)", "duplicate opens the copy"));

  // Nobody can be emailed: warn pill and a blocking reason.
  await click(host.querySelector('[data-drawer="segment"] button[aria-label="Close segment details"]'));
  await settle(() => assert(host.querySelector('tr[data-record-id="seg-none"] .chip[data-tone="warn"]'), "list row shows the warn pill for zero eligible"));
  await click(host.querySelector('tr[data-record-id="seg-none"] td'));
  await settle(() => assert(host.querySelector('[data-drawer="segment"]'), "a click anywhere on the row opens the drawer"));
  await click(host.querySelector('[data-drawer="segment"] button[aria-label="Close segment details"]'));
  await click(host.querySelector('tr[data-record-id="seg-none"] button'));
  await settle(() => assert(text().includes("5 match"), "zero-eligible segment drawer renders"));
  assert(host.querySelector('[data-drawer="segment"] .chip[data-tone="warn"]')?.textContent?.includes("0 can be emailed"), "zero-eligible pill is the warn tone");
  assert(host.querySelector('[data-testid="funnel-reason"]')?.textContent?.includes("Nobody can be emailed: 5 have not agreed"), "reason names the blocker");
  assert((host.querySelector('[data-testid="rule-sentence"]')?.textContent ?? "").includes("and"), "rules join with and");
  await click(Array.from(host.querySelectorAll("button")).find((el) => el.textContent === "Use in campaign"));
  assert(location.search.includes("crm=campaigns") && location.search.includes("appview=new") && location.search.includes("segment=seg-none"), "Use in campaign opens new campaign with the segment: " + location.search);
  await settle(() => assert(host.querySelector('[role="dialog"] [aria-label="Audience"]'), "the New campaign dialog opens"));
  await settle(() => assert((host.querySelector('[role="dialog"]')?.textContent ?? "").includes("No consent"), "the dialog's audience preselects the segment"));
  // Back to the segment list and re-open the Newsletter drawer for the stale-write check.
  await React.act(async () => { history.pushState(null, "", "/app-installations/install-crm?ctx=ctx-a&crm=segments"); window.dispatchEvent(new Event("popstate")); });
  await settle(() => assert(host.querySelector('tr[data-record-id]'), "segments list again"));
  await click(Array.from(host.querySelectorAll("tr")).find((tr) => tr.textContent?.includes("Newsletter") && !tr.textContent?.includes("(copy)"))?.querySelector("button"));
  await settle(() => assert(host.querySelector('[data-drawer="segment"]'), "back on a segment drawer"));

  // Stale writes refuse through the host route: renaming against a
  // revision another operator already advanced explains itself.
  await click(Array.from(host.querySelectorAll("button")).find((el) => el.textContent === "Edit rules"));
  await settle(() => assert(host.querySelector("#seg-edit-name"), "edit form opens"));
  await fill("#seg-edit-name", "Stale Race");
  await click(host.querySelector('[data-drawer="segment"] button[type="submit"]'));
  await settle(() => assert(text().includes("stale"), "stale segment write explains itself"));
  assert(host.querySelector('[data-drawer="segment"]'), "the drawer survives a stale write");

  // Reload coherence: the landed URL restores list + drawer directly.
  const landed = location.href;
  await React.act(async () => { root.unmount(); });
  await React.act(async () => {
    history.pushState(null, "", landed.replace("http://localhost", ""));
    const root2 = createRoot(host);
    root2.render(React.createElement(AppShell, { installId: "install-crm", viewer: { operator: true, readOnly: false } }));
    (globalThis as any).__segRoot2 = root2;
  });
  await settle(() => assert(
    host.querySelector('[data-drawer="segment"]') && text().includes("Who can be emailed"),
    "reload of the landed URL restores list + drawer",
  ));
  await React.act(async () => { (globalThis as any).__segRoot2.unmount(); });

  console.log("crm segments checks passed");
}

void main();
