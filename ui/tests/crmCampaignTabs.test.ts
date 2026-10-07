export {};
/**
 * CAD-1055 campaign detail tabs (view only): the saved campaign is
 * Overview / Email / Audience / Activity — no Content tab, no single
 * scroll. The ready-to-send checklist is the Final send gate's own
 * list; the Email tab keeps the host-rendered preview, the pending
 * assistant proposal strip (saved/draft toggle, pinned Apply, plain
 * Discard) and every existing safety rule.
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
  readinessRules();
  await mounted(false);
  await mounted(true);
  console.log("crm campaign tabs checks passed");
}

/** The checklist and the Prepare gate read one list. */
function readinessRules() {
  const { sendReadiness, missingReasons } = require("../src/features/app-shell/campaign/readiness") as typeof import(
    "../src/features/app-shell/campaign/readiness"
  );
  const doc: any = {
    campaignId: "c", revision: 2, subject: "S", preheader: "", blocks: [], contentDigest: "d2",
    approval: { revision: 2, digest: "d2", valid: true, scope: "content-only" },
  };
  const binding: any = { state: "live", digest: "link1", sender: { name: "N", address: "a@b.c" } };
  const test: any = { contentDigest: "d2", linkDigest: "link1" };
  const all = sendReadiness({ doc, freezeId: "f", freeze: { valid: true }, binding, testEvidence: test });
  assert(all.every((item) => item.done), "every prerequisite met");
  equal(missingReasons(all), [], "nothing missing");
  // Order and wording of the gate are the long-standing ones.
  const none = sendReadiness({
    doc: { ...doc, approval: { revision: null, digest: null, valid: false, scope: "content-only" } },
    freezeId: "f", freeze: null, binding: undefined, testEvidence: null,
  });
  equal(missingReasons(none), [
    "content approved at the current revision",
    "the audience frozen and rechecked (its validity is unverified)",
    "the sender binding read (still loading)",
    "an accepted test send of this content and binding",
  ], "unmet reasons keep their order and text");
  // A stale approval (older revision) is unmet; a drifted test send is named.
  const stale = sendReadiness({
    doc: { ...doc, revision: 3 }, freezeId: "f", freeze: { valid: false }, binding: null,
    testEvidence: test,
  });
  equal(missingReasons(stale), [
    "content approved at the current revision",
    "the audience frozen and reporting valid",
    "a live SMTP sender binding",
  ], "stale approval, invalid freeze and no sender are unmet");
  const drifted = sendReadiness({ doc, freezeId: "f", freeze: { valid: true }, binding, testEvidence: { ...test, contentDigest: "old" } });
  equal(missingReasons(drifted), ["a test send accepted against this exact content revision and binding"], "a test of other bytes is not evidence");
  // The Fix targets point at the tab or panel that resolves each item.
  const fixes = Object.fromEntries(none.map((item) => [item.key, item.fix]));
  equal(fixes.freeze, { kind: "tab", tab: "audience", label: "Fix" }, "freeze fixes on the Audience tab");
  equal(fixes.draft, { kind: "tab", tab: "email", label: "Review" }, "the draft is reviewed on the Email tab");
  equal(sendReadiness({ doc: null, freezeId: "f", freeze: null, binding: null, testEvidence: null })[1].fix, null, "no approve link without a draft");
}

async function mounted(readOnly: boolean) {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/app-installations/install-crm?ctx=ctx-a" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLSelectElement", "HTMLIFrameElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener"])
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  Object.defineProperty(globalThis, "requestAnimationFrame", { value: (fn: () => void) => setTimeout(fn, 0), configurable: true });
  Object.defineProperty(globalThis, "EventSource", {
    configurable: true,
    value: class { onopen: null = null; onerror: null = null; addEventListener() {} removeEventListener() {} close() {} },
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
  const doc: any = {
    campaign_id: "launch-1", install_id: "install-crm", context_id: "ctx-a", revision: 2,
    subject: "Welcome aboard", preheader: "We are glad you are here",
    blocks: [{ type: "paragraph", text: "Hi there" }],
    content_digest: "content-digest-2",
    approval: { revision: null, digest: null, valid: false, scope: "content-only" },
  };
  const receipt = {
    message_id: "m1", agent: "crm-writer", request_id: "req-1", install_id: "install-crm",
    context_id: "ctx-a", campaign_id: "launch-1", source_revision: 2,
  };
  const proposals: Record<string, any> = {
    "prop-1": {
      proposal_id: "prop-1", install_id: "install-crm", context_id: "ctx-a", campaign_id: "launch-1",
      source_revision: 2, subject: "Draft subject one", preheader: "", blocks: [{ type: "paragraph", text: "Draft copy one" }],
      content_digest: "pd1", actor: "assistant", origin: "assistant-receipt", assistant_receipt: receipt, state: "pending",
    },
    // Forged provenance: actor says assistant but there is no host receipt.
    "prop-2": {
      proposal_id: "prop-2", install_id: "install-crm", context_id: "ctx-a", campaign_id: "launch-1",
      source_revision: 2, subject: "Draft subject two", preheader: "", blocks: [{ type: "paragraph", text: "Draft copy two" }],
      content_digest: "pd2", actor: "assistant", origin: "assistant-receipt", assistant_receipt: null, state: "pending",
    },
  };
  const applyBodies: { id: string; body: any }[] = [];
  const discards: string[] = [];
  const renderBodies: any[] = [];
  const posts: string[] = [];
  const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
  const refused = (message: string, status = 409) => json({ error: message }, status);
  const realFetch = globalThis.fetch;
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const path = String(input);
    const url = new URL(path, "http://localhost");
    const method = init?.method ?? "GET";
    const parts = url.pathname.split("/");
    if (method !== "GET") posts.push(`${method} ${url.pathname}`);
    if (method === "POST" && url.pathname === "/api/crm-smtp/show") return refused("CRM SMTP sender refused or unavailable", 409);
    if (url.pathname === "/api/crm-send/list") return json({ sends: [] });
    if (url.pathname === "/api/crm-send/origin") return json({ unsubscribe_origin: null, stored: false });
    if (path === "/api/connections") return json({ connections: [] });
    if (method === "POST" && url.pathname.endsWith("/audience/preview")) {
      return json({
        base: {}, base_count: 4, exclusion_list_id: null, exclusion_count: 0,
        final_excluded: { invalid_email: 0, no_consent: 4, unsubscribed: 0, suppressed: 0 },
        final_count: 0, sample: [], digest: "aud-digest-1",
      });
    }
    if (url.pathname.includes("/audience/prepares/")) {
      return json({ freeze: {
        freeze_id: "launch-1-freeze-1", install_id: "install-crm", context_id: "ctx-a",
        base: { mode: "all" }, exclusion_list_id: null, final_count: 0, max_recipients: 50,
        digest: "audience-digest-frozen-0001", sample: [],
      }, current_digest: "audience-digest-frozen-0001", current_final_count: 0, valid: true, drift: null });
    }
    if (path.startsWith("/api/threads/master")) return json({ entries: [], more_before: false });
    if (url.pathname.endsWith("/segments/list")) return json({ segments: [] });
    if (url.pathname.endsWith("/exclusions/list")) return json({ exclusions: [] });
    if (url.pathname.endsWith("/suppressions/list")) return json({ suppressions: [] });
    if (url.pathname.endsWith("/content/campaigns/list")) return json({ contents: [doc] });
    if (url.pathname.endsWith("/content/proposals/list")) return json({ proposals: Object.values(proposals) });
    if (method === "GET" && url.pathname.includes("/content/proposals/") && url.pathname.endsWith("/render")) {
      const row = proposals[parts.at(-2)!];
      if (!row || row.state !== "pending") return refused("email proposal is already decided", 409);
      return json({ render: {
        proposal_id: row.proposal_id, campaign_id: row.campaign_id, install_id: "install-crm", context_id: "ctx-a",
        state: "pending", source_revision: row.source_revision, content_digest: row.content_digest,
        binding: { binding_id: "preview", revision: 0, digest: "b", preview_only: true },
        preview_only: true, send_ready: false, sender: { name: "CRM News", address: "news@example.com" },
        unsubscribe_url: "https://unsub.example.invalid",
        html: `<h1>${row.subject}</h1><p>${row.blocks[0].text}</p>`, text: `${row.subject} TEXT draft`,
        render_digest: "sha256:r-" + row.proposal_id,
      } });
    }
    if (method === "POST" && url.pathname.includes("/content/proposals/") && url.pathname.endsWith("/apply")) {
      const id = parts.at(-2)!;
      const body = init?.body ? JSON.parse(String(init.body)) : {};
      applyBodies.push({ id, body });
      const row = proposals[id];
      if (body.expected_revision !== doc.revision) return refused("email revision is stale");
      row.state = "applied";
      doc.revision += 1; doc.subject = row.subject; doc.preheader = row.preheader; doc.blocks = row.blocks;
      doc.content_digest = `content-digest-${doc.revision}`;
      doc.approval = { revision: null, digest: null, valid: false, scope: "content-only" };
      return json({ content: doc });
    }
    if (method === "POST" && url.pathname.includes("/content/proposals/") && url.pathname.endsWith("/discard")) {
      const id = parts.at(-2)!;
      discards.push(id);
      proposals[id].state = "discarded";
      return json({ proposal: proposals[id] });
    }
    if (method === "POST" && url.pathname.endsWith("/approve")) {
      const body = init?.body ? JSON.parse(String(init.body)) : {};
      if (body.expected_revision !== doc.revision) return refused("email revision is stale");
      doc.approval = { revision: doc.revision, digest: doc.content_digest, valid: true, scope: "content-only" };
      return json({ content: doc });
    }
    if (method === "POST" && url.pathname.endsWith("/render")) {
      const body = init?.body ? JSON.parse(String(init.body)) : {};
      renderBodies.push(body);
      return json({ render: {
        campaign_id: doc.campaign_id, install_id: "install-crm", context_id: "ctx-a",
        revision: doc.revision, content_digest: doc.content_digest, sample_first_name: body.sample_first_name ?? null,
        binding: { binding_id: "preview", revision: 1, digest: "binding-digest", preview_only: true },
        preview_only: true, send_ready: false, sender: { name: "CRM News", address: "news@example.com" },
        unsubscribe_url: "https://unsub.example.invalid/preview",
        html: `<h1>${doc.subject}</h1>HTML form ${body.sample_first_name ?? ""}`, text: `${doc.subject} TEXT form`,
        render_digest: "sha256:render",
      } });
    }
    if (url.pathname.includes("/content/campaigns/")) {
      if (parts.at(-1) === doc.campaign_id) return json({ content: doc });
      return refused("email content is unavailable for this installation and context", 404);
    }
    if (path === "/api/app-installations/install-crm") return json(install);
    if (path === "/api/app-installations/install-crm/contexts") return json({ contexts: [
      { id: "ctx-a", install_id: "install-crm", revision: 1, state: "active", digest: "ca", config: { schema: 1, label: "Acme", input_defaults: {} } },
    ] });
    throw new Error(`Unexpected ${method} ${path}`);
  }) as typeof fetch;

  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
  const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
  const text = () => host.textContent ?? "";
  async function settle(check: () => void, budgetMs = 8000) {
    const deadline = Date.now() + budgetMs;
    let last: unknown;
    for (;;) {
      await flush();
      try { check(); return; } catch (e) { last = e; }
      if (Date.now() >= deadline) break;
      await sleep(120);
    }
    await flush();
    try { check(); } catch { throw last; }
  }
  async function click(element: Element | undefined | null) {
    assert(element, "click target exists");
    await React.act(async () => { element.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
    await flush();
  }
  const byText = (scope: ParentNode, tag: string, label: string) =>
    Array.from(scope.querySelectorAll(tag)).find((el) => (el.textContent ?? "").trim() === label) ?? null;
  const tabEl = (name: string) => host.querySelector(`[data-tab="${name}"]`);
  const openTab = async (name: string) => { await click(tabEl(name)); await settle(() => assert(tabEl(name)?.getAttribute("aria-selected") === "true", `${name} tab selected`)); };
  const items = () => Array.from(host.querySelectorAll("[data-checklist] li[data-item]"));
  const item = (key: string) => host.querySelector(`[data-checklist] li[data-item="${key}"]`);
  const buttonIn = (scope: ParentNode | null, label: string) => (scope ? (byText(scope, "button", label) as HTMLButtonElement | null) : null);

  win.sessionStorage.clear();
  history.pushState(null, "", "/app-installations/install-crm?ctx=ctx-a&crm=campaigns&record=launch-1");
  await React.act(async () => {
    root.render(React.createElement(AppShell, { installId: "install-crm", viewer: { operator: true, readOnly } }));
  });
  await settle(() => assert(host.querySelector('section[aria-label="Campaign details"]') && tabEl("overview"), "the tabbed detail renders"));

  // ---- Structure: four tabs, Overview first, one panel mounted at a time.
  equal(Array.from(host.querySelectorAll('[role="tab"]')).map((el) => el.getAttribute("data-tab")), ["overview", "email", "audience", "activity"], "four tabs and no Content tab");
  assert(!byText(host, '[role="tab"]', "Content"), "no Content tab label");
  assert(tabEl("overview")!.getAttribute("aria-selected") === "true", "a campaign opens on Overview");
  equal(host.querySelectorAll('[role="tabpanel"]').length, 1, "exactly one tab panel is mounted");
  assert(!host.querySelector('[aria-label="Email preview"]') && !host.querySelector('[aria-label="Frozen audience"]'), "Overview mounts neither the preview nor the audience");
  assert(host.querySelector("[data-campaign-status]")?.textContent === "Draft", "the header says Draft while unapproved");
  assert(!text().includes("Record diagnostics") || host.querySelector("details.crm-diag"), "ids stay behind a disclosure");
  // Keyboard: ArrowRight moves selection to the next tab.
  await React.act(async () => { tabEl("overview")!.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true })); });
  await settle(() => assert(tabEl("email")!.getAttribute("aria-selected") === "true", "ArrowRight selects the next tab"));
  equal(tabEl("email")!.getAttribute("tabindex"), "0", "roving tabindex follows selection");
  equal(tabEl("overview")!.getAttribute("tabindex"), "-1", "other tabs leave the tab order");
  await openTab("overview");

  // ---- Overview: the checklist is driven by the same readiness list.
  await settle(() => assert(items().length === 5, "five checklist rows"));
  assert(item("draft")?.hasAttribute("data-done"), "a saved draft is done");
  assert(!item("approval")?.hasAttribute("data-done"), "approval is not done");
  const unmet = items().filter((li) => !li.hasAttribute("data-done")).length;
  await settle(() => assert(host.querySelector('[data-prerequisites="missing"]'), "Final send names what is missing"));
  equal(host.querySelectorAll('[data-prerequisites="missing"] li').length, unmet, "the checklist and the Prepare gate list the same unmet items");
  const done = items().filter((li) => li.hasAttribute("data-done")).length;
  assert(text().includes(`${done} of 5`), "progress counts done rows");
  if (!readOnly) {
    assert((byText(host, "button", "Prepare send") as HTMLButtonElement).disabled, "Prepare send stays disabled while items are unmet");
    // Fix links go to the owning tab or panel.
    await click(buttonIn(item("approval"), "Approve"));
    assert((document.activeElement as HTMLElement | null)?.id === "cmp-approval-section", "Approve focuses the approval panel");
    assert(item("freeze")?.hasAttribute("data-done"), "the freeze item is satisfied by the automatic check");
    await openTab("audience");
    assert(host.querySelector('section[aria-label="Frozen audience"]'), "the Audience tab mounts the audience panel");
    assert(!host.querySelector('section[aria-label="Final send"]'), "Audience mounts no send controls");
    await openTab("overview");
    // Approval is content-only and relocated, not removed.
    await click(byText(host, "button", "Approve r2 (content-only)"));
    await settle(() => {
      const done = items().filter((li) => li.hasAttribute("data-done")).length;
      assert(
        item("approval")?.hasAttribute("data-done") && text().includes(`${done} of 5`),
        "approving ticks the checklist",
      );
    });
    assert(host.querySelector("[data-campaign-status]")?.textContent === "Approved", "the header says Approved");
  } else {
    assert(!byText(host, "button", "Prepare send"), "a read-only viewer gets no send controls");
    assert(!byText(host, "button", "Approve r2 (content-only)"), "a read-only viewer cannot approve");
  }

  // ---- Email: toolbar, envelope, host-rendered preview.
  await openTab("email");
  await settle(() => assert((host.querySelector('iframe[data-preview="visual"]') as HTMLIFrameElement | null)?.getAttribute("srcdoc")?.includes("HTML form"), "the saved revision renders in the Visual iframe"));
  const envelope = host.querySelector('[aria-label="Email envelope"]');
  assert(envelope, "the envelope header renders");
  const envSubject = readOnly ? (envelope!.textContent ?? "") : (host.querySelector("#cmp-subject") as HTMLInputElement).value;
  const envPre = readOnly ? (envelope!.textContent ?? "") : (host.querySelector("#cmp-preheader") as HTMLInputElement).value;
  assert(envSubject.includes("Welcome aboard") && envPre.includes("We are glad you are here"), "envelope shows subject and preheader");
  assert((envelope!.textContent ?? "").includes("CRM News <news@example.com>") && (envelope!.textContent ?? "").includes("preview-only"), "envelope shows the host-locked preview sender");
  assert(!!host.querySelector("#cmp-subject") === !readOnly, "the envelope is editable only for an operator who can write");
  const inbox = () => host.querySelector(".crm-inbox") as HTMLElement;
  equal(inbox().getAttribute("data-device"), "desktop", "desktop width by default");
  await click(byText(host, "button", "Mobile"));
  equal(inbox().getAttribute("data-device"), "mobile", "Mobile narrows the stage");
  await click(byText(host, "button", "Desktop"));
  equal(inbox().getAttribute("data-device"), "desktop", "Desktop restores it");
  await click(byText(host, "button", "HTML"));
  assert((host.querySelector('pre[data-preview="html"]')?.textContent ?? "").includes("HTML form"), "HTML shows host markup");
  await click(byText(host, "button", "Text"));
  assert((host.querySelector('pre[data-preview="text"]')?.textContent ?? "").includes("TEXT form"), "Text shows the host text form");
  assert(byText(host, "button", "Text")!.getAttribute("aria-pressed") === "true", "the active format is exposed");
  await click(byText(host, "button", "Visual"));
  assert(host.querySelector('iframe[data-preview="visual"]'), "Visual returns to the iframe");
  assert(host.querySelector('iframe[data-preview="visual"]')!.getAttribute("sandbox") === "", "the preview stays scriptless");
  // Sample recipient flows into a fresh host render.
  const rendersBefore = renderBodies.length;
  await React.act(async () => {
    const input = host.querySelector("#cmp-sample") as HTMLInputElement;
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, "Zed");
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await click(byText(host, "button", "Refresh preview"));
  await settle(() => assert(renderBodies.length > rendersBefore && renderBodies.at(-1).sample_first_name === "Zed", "the sample name is sent to the host render"));
  assert((envelope!.textContent ?? "").length > 0 && (host.querySelector('[aria-label="Email envelope"]')!.textContent ?? "").includes("Zed"), "the envelope names the sample recipient");
  assert(!posts.some((p) => p.includes("/content/campaigns") && p.endsWith("/save")), "previews never save");

  // ---- Assistant proposal strips: provenance, draft toggle, pinned apply, discard.
  const strip1 = () => host.querySelector('[data-proposal="prop-1"]');
  const strip2 = () => host.querySelector('[data-proposal="prop-2"]');
  await settle(() => assert(strip1() && strip2(), "both pending proposals render as strips"));
  assert(strip1()!.querySelector('[data-badge="verified-assistant"]') && (strip1()!.textContent ?? "").includes("crm-writer"), "a host-receipted draft is badged verified");
  assert(strip2()!.querySelector('[data-badge="operator-submitted"]') && !strip2()!.querySelector('[data-badge="verified-assistant"]'), "actor=assistant without a receipt is never verified");
  assert(tabEl("email")!.textContent!.includes("2 draft"), "the Email tab badge counts pending drafts");
  // The draft toggle previews the proposal's own render; nothing is saved.
  await click(strip1()!.querySelector('[data-version="draft"]'));
  await settle(() => assert(host.querySelector('[data-proposal-body="prop-1"] iframe[data-preview="visual"]')?.getAttribute("srcdoc")?.includes("Draft subject one"), "Draft shows the proposal's host render"));
  assert((host.querySelector('[aria-label="Email envelope"]')!.textContent ?? "").includes("Draft subject one"), "the envelope follows the draft");
  assert(host.querySelector('[data-preview="draft-note"]'), "the stage says nothing is saved until Apply");
  await click(byText(host, "button", "Text"));
  await settle(() => assert((host.querySelector('[data-proposal-body="prop-1"] pre[data-preview="text"]')?.textContent ?? "").includes("TEXT draft"), "the toolbar drives the draft stage"));
  await click(byText(host, "button", "Visual"));
  equal(applyBodies.length, 0, "viewing a draft never applies it");
  equal(doc.revision, 2, "viewing a draft never saves a revision");
  await click(strip1()!.querySelector('[data-version="saved"]'));
  await settle(() => assert(!host.querySelector("[data-proposal-body]") && (host.querySelector('iframe[data-preview="visual"]')?.getAttribute("srcdoc") ?? "").includes("HTML form"), "Saved returns to the saved render"));
  if (readOnly) {
    assert(!buttonIn(strip1(), "Apply as r3") && !buttonIn(strip1(), "Discard"), "a read-only viewer sees no Apply or Discard");
    assert(!byText(host, "button", "Edit subject / text"), "a read-only viewer cannot edit");
  } else {
    // Discard is non-mutating.
    await click(buttonIn(strip2(), "Discard"));
    await settle(() => assert(!strip2() && text().includes("draft unchanged at r2"), "Discard drops the strip and says the draft is unchanged"));
    equal(discards, ["prop-2"], "Discard hit the discard route for that proposal only");
    equal(doc.revision, 2, "Discard never saves");
    // Apply is pinned to the saved revision and resets approval.
    await click(buttonIn(strip1(), "Apply as r3"));
    await settle(() => assert(text().includes("Applied as revision 3") && text().includes("approval invalidated"), "Apply reports the new revision and the approval reset"));
    equal(applyBodies, [{ id: "prop-1", body: { expected_revision: 2 } }], "Apply is pinned to the revision the strip showed");
    await settle(() => assert(!strip1(), "the applied strip leaves"));
    await settle(() => assert(((host.querySelector("#cmp-subject") as HTMLInputElement | null)?.value ?? "").includes("Draft subject one"), "the editable envelope follows the new revision"));
    await openTab("overview");
    assert(!item("approval")?.hasAttribute("data-done"), "applying resets the approval row");
    assert(host.querySelector("[data-campaign-status]")?.textContent === "Draft", "the header returns to Draft");
    // Inline editing is reachable straight from the Email tab (CAD-1057).
    await openTab("email");
    await settle(() => assert(host.querySelector("#cmp-subject"), "inline subject editing sits in the Email tab envelope"));
    assert(!host.querySelector('[aria-label="Assistant proposals"] [role="alert"]'), "no stray proposal errors");
  }

  // ---- Audience: picker and funnel up front, the rest under Advanced.
  await openTab("audience");
  await settle(() => assert(host.querySelector("[data-funnel]"), "the eligibility funnel renders from the host counts"));
  const step = (key: string) => host.querySelector(`[data-funnel] [data-step="${key}"]`)?.textContent ?? "";
  assert(step("match").includes("4"), "funnel shows the matching customers");
  assert(step("consent").includes("−4"), "funnel shows who lacks email consent");
  assert(step("final").includes("Can be emailed") && step("final").endsWith("0"), "funnel ends on the host's final count");
  assert((host.querySelector("[data-blocked]")?.textContent ?? "").includes("All 4 lack email consent"), "a plain-language blocking reason names the biggest drop");
  assert(host.querySelector('[role="radiogroup"][aria-label="Base audience mode"]'), "the audience picker stays up front");
  const advanced = host.querySelector("details[data-advanced]") as HTMLDetailsElement | null;
  assert(advanced && !advanced.open, "Advanced is a closed disclosure");
  assert(advanced!.querySelector("#aud-exclusion") && advanced!.querySelector('section[aria-label="Suppressions"]'), "exclusions and suppressions live inside Advanced");
  assert(!Array.from(host.querySelectorAll("#aud-exclusion, section[aria-label=\"Suppressions\"]")).some((el) => !advanced!.contains(el)), "nothing from Advanced leaks onto the main path");
  if (!readOnly) {
    await settle(() => assert((host.querySelector('[aria-label="Freeze validity"]')?.textContent ?? "").includes("Valid"), "the automatic freeze check reports validity"));
    const freezeDetails = host.querySelector('section[aria-label="Frozen audience"] details') as HTMLDetailsElement;
    assert(freezeDetails && !freezeDetails.open && (freezeDetails.textContent ?? "").includes("audience-digest-frozen-0001"), "the freeze digest sits behind Technical details");
  }

  // ---- Activity: a plain timeline; ids and digests behind Technical details.
  await openTab("activity");
  assert(host.querySelector('section[aria-label="Campaign sends"]'), "Activity carries the sends history");
  const events = Array.from(host.querySelectorAll("[data-timeline] li")).map((li) => li.getAttribute("data-event"));
  if (readOnly) {
    equal(events, ["proposal-pending", "proposal-pending", "approval", "revision"], "pending drafts lead the timeline");
  } else {
    equal(events, ["approval", "revision", "proposal-applied", "proposal-discarded"], "applied and discarded drafts follow the saved version");
  }
  const timeline = host.querySelector("[data-timeline]")!.textContent ?? "";
  assert(!timeline.includes("content-digest") && !timeline.includes("prop-1") && !timeline.includes("launch-1"), "the timeline carries no ids or digests");
  const technical = host.querySelector("details[data-technical]") as HTMLDetailsElement | null;
  assert(technical && !technical.open, "Technical details is a closed disclosure");
  const tech = technical!.textContent ?? "";
  for (const needle of ["launch-1", "content-digest-", "prop-1", "Unsubscribe link", "https://unsub.example.invalid/preview", "preview-only"]) {
    assert(tech.includes(needle), `Technical details carries: ${needle}`);
  }
  assert(!Array.from(host.querySelectorAll('[data-panel] > *:not([aria-label="Campaign timeline"]):not([aria-label="Campaign sends"])')).length, "Activity holds only the timeline and the sends");

  await React.act(async () => { root.unmount(); });
  globalThis.fetch = realFetch;
  host.remove();
  void items;
}

void main();
