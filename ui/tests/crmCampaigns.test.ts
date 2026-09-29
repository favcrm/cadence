export {};
/**
 * CAD-784 CRM Campaigns: content wire grammar plus mounted
 * list/new/detail flow through the real AppShell — no inline builder
 * on the list, host previews, distinct test-send, attributed
 * Apply/Discard, and locked preview-only sender material.
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
  await contentGrammar();
  await mountedFlow();
}

async function contentGrammar() {
  const { contentClient } = require("../src/features/app-shell/contentClient") as typeof import(
    "../src/features/app-shell/contentClient"
  );
  const { audienceClient, audiencePaths } = require("../src/features/app-shell/audienceClient") as typeof import(
    "../src/features/app-shell/audienceClient"
  );
  const { ApiError } = require("../src/lib/api") as typeof import("../src/lib/api");
  const scope = { installId: "install-a", contextId: "ctx-b" };
  equal(
    audiencePaths.exclusionSavePath(scope),
    "/api/app-installations/install-a/contexts/ctx-b/exclusions",
    "exclusion save binds the URL scope",
  );
  equal(
    audiencePaths.suppressionAddPath(scope),
    "/api/app-installations/install-a/contexts/ctx-b/suppressions",
    "suppression add binds the URL scope",
  );
  equal(
    audiencePaths.preparePath(scope),
    "/api/app-installations/install-a/contexts/ctx-b/audience/prepares",
    "freeze prepare binds the URL scope",
  );
  // Receipt-shaped fields refuse on every content/audience write: no
  // assistant seam exists yet, so any present receipt is forged.
  const realFetch = (globalThis as any).fetch;
  (globalThis as any).fetch = () => {
    throw new Error("network must not be reached by a receipt-bearing body");
  };
  try {
    for (const [call, why] of [
      [
        () =>
          (contentClient as any).propose(scope, {
            campaignId: "launch-1",
            proposalId: "prop-1",
            subject: "S",
            blocks: [],
            assistant_receipt: { turn_id: "t" },
          }),
        "proposed receipt",
      ],
      [
        () =>
          (contentClient as any).save(scope, {
            campaignId: "launch-1",
            subject: "S",
            blocks: [],
            turn_id: "t",
          }),
        "save turn claim",
      ],
      [
        () =>
          (audienceClient as any).segmentSave(scope, {
            segmentId: "seg-1",
            name: "N",
            predicates: [],
            nonce: "n",
          }),
        "segment nonce",
      ],
    ] as [() => Promise<unknown>, string][]) {
      let refused = false;
      try {
        await call();
      } catch (error) {
        assert(error instanceof ApiError, `${why} refuses typed`);
        refused = true;
      }
      assert(refused, `${why} reached the network`);
    }
  } finally {
    (globalThis as any).fetch = realFetch;
  }

  // The constrained editor grammar mirrors the host bounds.
  const grammar = require("../src/features/app-shell/campaignGrammar") as typeof import(
    "../src/features/app-shell/campaignGrammar"
  );
  grammar.checkContent("Hello {{first_name|Friend}}", "", [{ type: "paragraph", text: "Hi" }]);
  grammar.checkContent("S", "", [{ type: "button", label: "Shop", url: "https://example.com/offer" }]);
  for (const [subject, preheader, blocks, why] of [
    ["", "", [{ type: "paragraph", text: "Hi" }], "blank subject"],
    ["S", "", [], "no blocks"],
    ["S", "", [{ type: "heading", text: "a<b" }], "markup paste"],
    ["S {{other|x}}", "", [{ type: "paragraph", text: "Hi" }], "unknown token"],
    ["S {{first_name|}}", "", [{ type: "paragraph", text: "Hi" }], "empty fallback"],
    ["S", "", [{ type: "button", label: "Go", url: "http://example.com" }], "plain http"],
    ["S", "", [{ type: "button", label: "Go", url: "javascript:alert(1)" }], "script url"],
    ["S", "", [{ type: "video", text: "x" }], "unknown block"],
  ] as [string, string, any[], string][]) {
    let refused = false;
    try {
      grammar.checkContent(subject, preheader, blocks);
    } catch (error) {
      assert(error instanceof ApiError, `${why} refuses typed`);
      refused = true;
    }
    assert(refused, `${why} passed the client grammar`);
  }
  try {
    grammar.checkCampaignId("bad id");
    throw new Error("bad campaign id passed");
  } catch (error) {
    assert(error instanceof ApiError, "bad campaign id refuses typed");
  }
}

async function mountedFlow() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/app-installations/install-crm?ctx=ctx-a" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLSelectElement", "HTMLIFrameElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
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
  const contents: Record<string, any> = {
    "launch-1": {
      campaign_id: "launch-1", install_id: "install-crm", context_id: "ctx-a", revision: 1,
      subject: "Hello", preheader: "", blocks: [{ type: "paragraph", text: "Hi there" }],
      content_digest: "content-digest-1",
      approval: { revision: null, digest: null, valid: false, scope: "content-only" },
    },
  };
  const proposals: Record<string, any> = {};
  let propSeq = 0;
  const suppressions: { kind: string; key: string; reason: string }[] = [];
  const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
  const refused = (message: string, status = 409) => json({ error: message }, status);
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const path = String(input);
    const url = new URL(path, "http://localhost");
    const method = init?.method ?? "GET";
    const parts = url.pathname.split("/");
    // Content saves ride the collection path with CAS.
    if (method === "POST" && url.pathname.endsWith("/content/campaigns")) {
      const body = JSON.parse(String(init!.body));
      const current = contents[body.campaign_id]?.revision as number | undefined;
      if (current !== undefined && body.expected_revision === undefined) {
        return refused("email content already exists; name the observed revision");
      }
      if (current !== undefined && body.expected_revision !== current) {
        return refused("email content revision is stale");
      }
      const revision = current === undefined ? 1 : current + 1;
      contents[body.campaign_id] = {
        campaign_id: body.campaign_id, install_id: "install-crm", context_id: "ctx-a", revision,
        subject: body.subject, preheader: body.preheader, blocks: body.blocks,
        content_digest: `content-digest-${revision}`,
        approval: { revision: null, digest: null, valid: false, scope: "content-only" },
      };
      return json({ content: contents[body.campaign_id] });
    }
    if (method === "POST" && parts.includes("campaigns") && parts.at(-1) === "render") {
      const id = parts.at(-2)!;
      const doc = contents[id];
      if (!doc) return refused("email content is unavailable for this installation and context", 404);
      return json({ render: {
        campaign_id: id, install_id: "install-crm", context_id: "ctx-a", revision: doc.revision,
        content_digest: doc.content_digest, sample_first_name: null,
        binding: { binding_id: "preview", revision: 1, digest: "binding-digest", preview_only: true },
        preview_only: true, send_ready: false,
        sender: { name: "Cadence CRM", address: "noreply@cadence.invalid" },
        unsubscribe_url: "https://cadence.invalid/unsubscribe/preview",
        html: "<h1>HTML form</h1>", text: "TEXT form", render_digest: "render-digest-1",
      } });
    }
    if (method === "POST" && parts.includes("campaigns") && parts.at(-1) === "approve") {
      const id = parts.at(-2)!;
      const doc = contents[id];
      const body = JSON.parse(String(init!.body));
      if (!doc) return refused("email content is unavailable for this installation and context", 404);
      if (body.expected_revision !== doc.revision) return refused("email content revision is stale");
      doc.approval = { revision: doc.revision, digest: doc.content_digest, valid: true, scope: "content-only" };
      return json({ content: doc });
    }
    if (method === "POST" && parts.includes("campaigns") && parts.at(-1) === "test-prepare") {
      const id = parts.at(-2)!;
      const doc = contents[id];
      const body = JSON.parse(String(init!.body));
      if (!doc) return refused("email content is unavailable for this installation and context", 404);
      if (typeof body.to_email !== "string" || !body.to_email.includes("@")) {
        return refused("email test recipient exceeds its supported shape or bounds");
      }
      return json({ test_send: {
        kind: "test", campaign_id: id, install_id: "install-crm", context_id: "ctx-a",
        content_revision: doc.revision, content_digest: doc.content_digest,
        sender_binding: { binding_id: "preview", preview_only: true },
        preview_only: true, send_ready: false, to_email: body.to_email,
        payload_digest: "payload-digest-1",
      } });
    }
    if (method === "POST" && url.pathname.endsWith("/content/proposals")) {
      const body = JSON.parse(String(init!.body));
      propSeq += 1;
      const id = body.proposal_id as string;
      proposals[id] = {
        proposal_id: id, install_id: "install-crm", context_id: "ctx-a",
        campaign_id: body.campaign_id, source_revision: contents[body.campaign_id]?.revision ?? 0,
        subject: body.subject, preheader: body.preheader, blocks: body.blocks,
        content_digest: `proposal-digest-${propSeq}`, actor: "operator", origin: "operator-direct",
        assistant_receipt: null, state: "pending", created: 1759286400, decided: null,
      };
      return json({ proposal: proposals[id] });
    }
    if (method === "POST" && parts.includes("proposals") && parts.at(-1) === "apply") {
      const id = parts.at(-2)!;
      const row = proposals[id];
      if (!row || row.state !== "pending") return refused("email proposal is already decided");
      const body = String(init!.body ?? "");
      const expected = body === "" ? undefined : (JSON.parse(body).expected_revision as number | undefined);
      const doc = contents[row.campaign_id];
      if (expected !== undefined && doc && expected !== doc.revision) {
        return refused("email content revision is stale");
      }
      if (doc && row.source_revision !== doc.revision) return refused("email proposal source revision is stale");
      const revision = (doc?.revision ?? 0) + 1;
      contents[row.campaign_id] = {
        ...doc, revision, subject: row.subject, preheader: row.preheader, blocks: row.blocks,
        content_digest: `content-digest-${revision}`,
        approval: { revision: null, digest: null, valid: false, scope: "content-only" },
      };
      row.state = "applied";
      return json({ content: contents[row.campaign_id] });
    }
    if (method === "POST" && parts.includes("proposals") && parts.at(-1) === "discard") {
      const id = parts.at(-2)!;
      const row = proposals[id];
      if (!row || row.state !== "pending") return refused("email proposal is already decided");
      row.state = "discarded";
      return json({ proposal: row });
    }
    if (method === "POST" && url.pathname.endsWith("/audience/preview")) {
      return json({
        base: {}, base_count: 5, exclusion_list_id: null, exclusion_count: 0,
        final_excluded: { invalid_email: 0, no_consent: 0, unsubscribed: 0, suppressed: suppressions.length },
        final_count: 5,
        sample: [{ id: "customer-s1", display_name: "Seed Alpha" }],
        digest: "audience-digest-1",
      });
    }
    if (method === "POST" && url.pathname.endsWith("/audience/prepares")) {
      const body = JSON.parse(String(init!.body));
      return json({ freeze: {
        freeze_id: body.freeze_id, install_id: "install-crm", context_id: "ctx-a",
        base: body.base, exclusion_list_id: body.exclusion_list_id ?? null,
        final_count: 5, max_recipients: body.max_recipients,
        digest: "audience-digest-1", sample: [{ id: "customer-s1", display_name: "Seed Alpha" }],
        replayed: false,
      } });
    }
    if (method === "POST" && url.pathname.endsWith("/suppressions")) {
      const body = JSON.parse(String(init!.body));
      suppressions.push({ kind: "email", key: body.email as string, reason: body.reason as string });
      return json({ suppression: suppressions.at(-1) });
    }
    if (path === "/api/app-installations/install-crm") return json(install);
    if (path === "/api/app-installations/install-crm/contexts") return json({ contexts: [
      { id: "ctx-a", install_id: "install-crm", revision: 1, state: "active", digest: "ca", config: { schema: 1, label: "Acme", input_defaults: {} } },
    ] });
    if (path.startsWith("/api/threads/master")) return json({ entries: [], more_before: false });
    if (url.pathname.endsWith("/segments/list")) return json({ segments: [] });
    if (url.pathname.endsWith("/exclusions/list")) return json({ exclusions: [] });
    if (url.pathname.endsWith("/suppressions/list")) return json({ suppressions });
    if (url.pathname.endsWith("/content/campaigns/list")) return json({ contents: Object.values(contents) });
    if (url.pathname.endsWith("/content/proposals/list")) return json({ proposals: Object.values(proposals) });
    if (url.pathname.includes("/content/campaigns/")) {
      const id = parts.at(-1)!;
      if (contents[id]) return json({ content: contents[id] });
      return refused("email content is unavailable for this installation and context", 404);
    }
    if (url.pathname.includes("/audience/prepares/")) {
      return json({ freeze: {
        freeze_id: parts.at(-1), install_id: "install-crm", context_id: "ctx-a",
        base: { mode: "all" }, exclusion_list_id: null, final_count: 5, max_recipients: 500,
        digest: "audience-digest-1", sample: [{ id: "customer-s1", display_name: "Seed Alpha" }],
      }, current_digest: "audience-digest-1", current_final_count: 5, valid: true, drift: null });
    }
    throw new Error(`Unexpected ${method} ${path}`);
  }) as typeof fetch;

  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
  const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
  const text = () => host.textContent ?? "";
  async function settle(check: () => void) {
    for (let i = 0; i < 80; i++) {
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
  const byText = (tag: string, label: string) =>
    Array.from(host.querySelectorAll(tag)).find((el) => (el.textContent ?? "").trim() === label) ?? null;
  async function fillInput(selector: string, value: string) {
    const element = host.querySelector(selector) as HTMLInputElement;
    assert(element, `field exists: ${selector}`);
    await React.act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(element, value);
      element.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await flush();
  }
  async function fillArea(selector: string, value: string) {
    const element = host.querySelector(selector) as HTMLTextAreaElement;
    assert(element, `area exists: ${selector}`);
    await React.act(async () => {
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(element, value);
      element.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await flush();
  }

  win.sessionStorage.clear();
  history.pushState(null, "", "/app-installations/install-crm?ctx=ctx-a&crm=campaigns");
  await React.act(async () => {
    root.render(React.createElement(AppShell, { installId: "install-crm", viewer: { operator: true, readOnly: false } }));
  });
  // The list shows status per saved campaign and contains no builder.
  await settle(() => assert(text().includes("Hello"), "campaigns list paints server rows"));
  assert(text().includes("Draft"), "list shows the content-only approval state");
  const listSection = host.querySelector('section[aria-label="Campaigns list"]');
  assert(listSection, "campaigns list section renders");
  assert(!listSection!.querySelector("input, textarea"), "the list contains no inline builder fields");
  assert(!text().includes("Create campaign"), "the list contains no inline builder submit");

  // Nested tabs move the `crm` route; Customers drops it entirely.
  await click(byText("button", "Segments"));
  await settle(() => assert(location.search.includes("crm=segments"), "segments tab routes"));
  await click(byText("button", "Customers"));
  await settle(() => assert(!location.search.includes("crm="), "customers is the default route"));
  await click(byText("button", "Campaigns"));
  await settle(() => assert(location.search.includes("crm=campaigns"), "campaigns tab routes back"));
  // Switching sections clears the record view: no drawer follows.
  assert(!host.querySelector("[data-drawer]"), "no drawer follows a section switch");

  // New campaign: audience preview counts render live without a save.
  await click(byText("button", "New campaign"));
  await settle(() => assert(host.querySelector("#cmp-subject"), "new campaign page opens"));
  await React.act(async () => { await sleep(700); });
  await settle(() => assert(text().includes("Final recipients"), "audience preview counts render pre-save"));
  // Suppression add refreshes the suppression counts.
  await fillInput("#aud-suppress-email", "gone@example.com");
  await fillInput("#aud-suppress-reason", "opted out by phone");
  await click(byText("button", "Add suppression"));
  await settle(() => assert(text().includes("gone@example.com"), "added suppression lists"));
  // Freeze the picked audience under an operator-named ID.
  await click(byText("button", "Freeze audience"));
  await settle(() => assert(text().includes("Frozen") && text().includes("5 recipients"), "freeze receipt reports"));

  // Content save unlocks preview, test-send and proposals in place.
  await fillInput("#cmp-subject", "Launch");
  await fillArea('form[aria-label="Email content"] textarea', "Hi there {{first_name|Friend}}");
  await click(byText("button", "Create campaign (revision 1)"));
  await settle(() => assert(text().includes("Created revision 1"), "first save reports its revision"));
  const createdId = new URLSearchParams(location.search).get("record");
  void createdId;

  // Host-rendered preview: visual frame plus HTML/Text tabs from one revision.
  await click(byText("button", "Render preview"));
  await settle(() => {
    const frame = host.querySelector('iframe[data-preview="visual"]') as HTMLIFrameElement;
    assert(frame && (frame.getAttribute("srcdoc") ?? "").includes("HTML form"), "visual frame carries the host HTML");
  });
  await click(byText("button", "Text"));
  await flush();
  assert((host.querySelector('pre[data-preview="text"]')?.textContent ?? "").includes("TEXT form"), "text tab shows the host text form");
  assert(text().includes("noreply@cadence.invalid"), "preview-only sender material renders");
  assert(text().includes("locked until CAD-785/786"), "final send stays visibly locked");

  // Approval is content-only; proposals Apply (new revision, approval
  // invalidated) or Discard (non-mutating) with honest attribution.
  await click(byText("button", "Approve r1 (content-only)"));
  await settle(() => assert(text().includes("Approved r1"), "approval lands content-only"));
  await click(byText("button", "Submit editor as proposal"));
  await settle(() => assert(text().includes("operator-direct"), "proposal shows honest attribution"));
  await click(byText("button", "Apply (new revision)"));
  await settle(() => assert(text().includes("approval invalidated"), "apply bumps the revision and invalidates approval"));
  assert(!text().includes("Approved r2"), "no approval survives a content change");
  await click(byText("button", "Submit editor as proposal"));
  await settle(() => assert(byText("button", "Discard"), "second proposal pends"));
  await click(byText("button", "Discard"));
  await settle(() => assert(text().includes("draft unchanged at r2"), "discard is non-mutating"));
  // A proposal whose source drifted behind the draft refuses as stale:
  // submit at r2, save r3, then Apply must not silently merge.
  await click(byText("button", "Submit editor as proposal"));
  await settle(() => assert(byText("button", "Discard"), "drifted proposal pends"));
  await fillInput("#cmp-subject", "Launch v3");
  await click(byText("button", "Save as r3"));
  await settle(() => assert(text().includes("Saved revision 3"), "draft moves to r3"));
  await click(byText("button", "Apply (new revision)"));
  await settle(() => assert(text().includes("stale"), "stale apply refuses instead of merging"));

  // Test-send is a distinct prepared-only affordance.
  await fillInput("#cmp-test-email", "qa@example.com");
  await click(byText("button", "Prepare test send"));
  await settle(() => assert(text().includes("qa@example.com") && text().includes("no SMTP"), "test receipt reports prepared-only"));

  // Open the full detail directly: frozen audience validity rechecks.
  await click(byText("button", "Open campaign detail →"));
  await settle(() => assert(host.querySelector('section[aria-label="Frozen audience"]'), "detail names its freeze panel"));
  await click(byText("button", "Recheck freeze"));
  await settle(() => assert(text().includes("Valid"), "freeze validity rechecks live"));

  // Direct HTTP failures surface: an unknown campaign ID refuses.
  await React.act(async () => { root.unmount(); });
  history.pushState(null, "", "/app-installations/install-crm?ctx=ctx-a&crm=campaigns&record=campaign-missing");
  await React.act(async () => {
    const root2 = createRoot(host);
    root2.render(React.createElement(AppShell, { installId: "install-crm", viewer: { operator: true, readOnly: false } }));
    (globalThis as any).__cmpRoot2 = root2;
  });
  await settle(() => assert(text().includes("unavailable for this installation and context"), "unknown campaign refuses through the route"));
  await React.act(async () => { (globalThis as any).__cmpRoot2.unmount(); });

  // An unknown section never renders: the route strips back to Customers.
  history.pushState(null, "", "/app-installations/install-crm?ctx=ctx-a&crm=bogus");
  await React.act(async () => {
    const root3 = createRoot(host);
    root3.render(React.createElement(AppShell, { installId: "install-crm", viewer: { operator: true, readOnly: false } }));
    (globalThis as any).__cmpRoot3 = root3;
  });
  await settle(() => assert(!location.search.includes("crm="), "unknown section strips to the default route"));
  await settle(() => assert(host.querySelector('section[aria-label="Customers list"]'), "unknown section falls back to Customers"));
  await React.act(async () => { (globalThis as any).__cmpRoot3.unmount(); });

  console.log("crm campaigns checks passed");
}

void main();
