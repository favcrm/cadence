export {};
/**
 * CAD-1182 campaign workspace: compact Overview with one next task,
 * separate guarded approval/test/final-send surfaces, Visual | HTML
 * editing, and proposal Review → Use in editor → explicit Save. Keep
 * revision, read-only, provenance, audience and receipt safeguards.
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

/** The compact workspace still derives its next task from the send prerequisites. */
function readinessRules() {
  const { sendReadiness, missingReasons } = require("../src/features/app-shell/campaign/readiness") as typeof import(
    "../src/features/app-shell/campaign/readiness"
  );
  const doc: any = {
    campaignId: "c", revision: 2, subject: "S", preheader: "", blocks: [], contentDigest: "d2",
    approval: { revision: 2, digest: "d2", valid: true, scope: "content-only" },
  };
  const binding: any = { state: "live", digest: "link1", sender: { name: "N", address: "a@b.c" } };
  const test: any = { contentRevision: 2, contentDigest: "d2", linkDigest: "link1" };
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
    testEvidence: { ...test, contentRevision: 3 },
  });
  equal(missingReasons(stale), [
    "content approved at the current revision",
    "the audience frozen and reporting valid",
    "a live SMTP sender binding",
  ], "stale approval, invalid freeze and no sender are unmet");
  const wrongRevisionTest = sendReadiness({
    doc: { ...doc, revision: 3, approval: { revision: 3, digest: "d2", valid: true, scope: "content-only" } },
    freezeId: "f", freeze: { valid: true }, binding, testEvidence: test,
  });
  equal(missingReasons(wrongRevisionTest), ["a test send accepted against this exact content revision and binding"], "same content digest from another revision is not accepted test evidence");
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
  const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
  const AppShellComponent = (require("../src/features/app-shell/AppShell") as typeof import("../src/features/app-shell/AppShell")).default;
  const AppShell = (props: React.ComponentProps<typeof AppShellComponent>) => React.createElement(LocaleProvider, null, React.createElement(AppShellComponent, props));

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
  const saveBodies: any[] = [];
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
    if (method === "POST" && url.pathname.endsWith("/content/campaigns")) {
      const body = init?.body ? JSON.parse(String(init.body)) : {};
      saveBodies.push(body);
      if (body.expected_revision !== doc.revision) return refused("email revision is stale");
      doc.revision += 1;
      doc.subject = body.subject;
      doc.preheader = body.preheader;
      doc.blocks = body.blocks ?? [];
      doc.mode = typeof body.html === "string" ? "html" : "blocks";
      doc.html = typeof body.html === "string" ? body.html : null;
      doc.text_override = body.text ?? null;
      doc.content_digest = `content-digest-${doc.revision}`;
      doc.approval = { revision: null, digest: null, valid: false, scope: "content-only" };
      return json({ content: doc });
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

  // ---- Overview: one useful next task, with guarded decisions kept separate.
  assert(host.querySelector("[data-campaign-overview] [data-next-task]"), "Overview presents a next task");
  assert(text().includes("Content approved"), "the unmet current-revision approval is the next task");
  const overview = host.querySelector("[data-campaign-overview]")!;
  assert(!overview.querySelector("[data-checklist], [data-prerequisites], [data-item]"), "Overview summary does not repeat the full readiness checklist");
  assert(!overview.textContent?.includes(" of 5"), "Overview summary has no checklist progress parity counter");
  const approvalGuard = host.querySelector('details[data-overview-guard="content-approval"]') as HTMLDetailsElement | null;
  const testGuard = host.querySelector('details[data-overview-guard="test-send"]') as HTMLDetailsElement | null;
  const finalGuard = host.querySelector('details[data-overview-guard="final-send"]') as HTMLDetailsElement | null;
  assert(approvalGuard && testGuard && finalGuard, "approval, test and final-send remain distinct guarded surfaces");
  assert(!approvalGuard.open && !testGuard.open && !finalGuard.open, "separate action surfaces start collapsed, not repeated as overview forms");
  if (!readOnly) {
    await click(approvalGuard.querySelector("summary"));
    await settle(() => assert(buttonIn(approvalGuard, "Approve r2 (content-only)"), "content-only approval is available in its separate surface"));
    await click(buttonIn(approvalGuard, "Approve r2 (content-only)"));
    await settle(() => assert(host.querySelector("[data-campaign-status]")?.textContent === "Approved", "approval updates the status without implying send readiness"));
    assert(host.querySelector("[data-campaign-overview]")?.textContent?.includes("content approved only"), "summary labels approval as content-only");
    await click(finalGuard.querySelector("summary"));
    assert(buttonIn(finalGuard, "Prepare send")?.disabled, "Prepare send stays disabled while sender and exact-revision test prerequisites are missing");
    await openTab("audience");
    assert(host.querySelector('section[aria-label="Frozen audience"]'), "the Audience tab mounts the audience panel");
    assert(!host.querySelector('section[aria-label="Final send"]'), "Audience mounts no send controls");
    await openTab("overview");
  } else {
    assert(!buttonIn(approvalGuard, "Approve r2 (content-only)"), "a read-only viewer cannot approve");
    assert(buttonIn(testGuard, "Send test")?.disabled, "a read-only viewer cannot send a test");
    const prepare = buttonIn(finalGuard, "Prepare send");
    assert(prepare === null || prepare.disabled, "a read-only viewer cannot prepare a send");
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
  equal(Array.from(host.querySelectorAll('[aria-label="Email editing mode"] button')).map((button) => button.textContent?.trim()), ["Visual", "HTML"], "editing offers exactly Visual and HTML modes");
  await click(byText(host, "button", "HTML"));
  if (readOnly) {
    assert((host.querySelector('pre[data-preview="html"]')?.textContent ?? "").includes("HTML form"), "read-only HTML mode displays host markup without an editor");
    assert(!host.querySelector("#cmp-html-source"), "read-only viewers cannot edit HTML source");
  } else {
    await settle(() => assert(host.querySelector("#cmp-html-source"), "HTML mode mounts its source editor"));
    assert((host.querySelector("#cmp-html-source") as HTMLTextAreaElement).value.includes("Hi there"), "HTML mode edits the current email source");
    assert(host.querySelector('iframe[data-preview="draft-html"]')?.getAttribute("sandbox") === "", "HTML draft preview remains scriptless");
  }
  assert(byText(host, "button", "HTML")!.getAttribute("aria-pressed") === "true", "the active mode is exposed");
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
  equal(saveBodies, [], "preview renders never save campaign content");
  equal(doc.revision, 2, "preview renders leave the saved revision unchanged");

  // ---- Assistant proposals: receipt provenance, Review/Use separation and explicit Save.
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
  assert(host.querySelector('[data-preview="draft-note"]'), "the stage says nothing is saved until explicit Save");
  assert(Array.from(host.querySelectorAll('[aria-label="Email editing mode"] button')).length === 2, "proposal review also has only Visual and HTML modes");
  equal(applyBodies.length, 0, "viewing a draft never applies it");
  equal(doc.revision, 2, "viewing a draft never saves a revision");
  await click(strip1()!.querySelector('[data-version="saved"]'));
  await settle(() => assert(!host.querySelector("[data-proposal-body]") && (host.querySelector('iframe[data-preview="visual"]')?.getAttribute("srcdoc") ?? "").includes("HTML form"), "Saved returns to the saved render"));
  if (readOnly) {
    assert(!buttonIn(strip1(), "Use in editor") && !buttonIn(strip1(), "Discard"), "a read-only viewer sees no Use or Discard controls");
    assert(!host.querySelector("#cmp-subject"), "a read-only viewer cannot edit the subject");
  } else {
    // Discarding one proposal is non-mutating to saved content.
    await click(buttonIn(strip2(), "Discard"));
    await settle(() => assert(!strip2(), "Discard removes only the proposal strip"));
    equal(discards, ["prop-2"], "Discard hit the discard route for that proposal only");
    equal(doc.revision, 2, "Discard never saves a revision");

    // Dirty local work must be protected before Use replaces the editor draft.
    const setInput = async (selector: string, value: string) => {
      const input = host.querySelector(selector) as HTMLInputElement | HTMLTextAreaElement;
      assert(input, `${selector} exists`);
      await React.act(async () => {
        Object.getOwnPropertyDescriptor(Object.getPrototypeOf(input), "value")!.set!.call(input, value);
        input.dispatchEvent(new Event("input", { bubbles: true }));
        input.dispatchEvent(new Event("change", { bubbles: true }));
      });
      await flush();
    };
    await setInput("#cmp-subject", "Locally edited subject");
    await setInput("#cmp-preheader", "Local preheader");
    await click(byText(host, "button", "HTML"));
    const localHtml = '<!doctype html><html><head><title>Local source</title></head><body><p>LOCAL SOURCE</p></body></html>';
    await setInput("#cmp-html-source", localHtml);
    const writesBeforeReview = posts.length;
    await click(strip1()!.querySelector('[data-proposal-preview="prop-1"]'));
    await settle(() => assert(host.querySelector('[data-proposal-body="prop-1"]') && byText(strip1()!, "button", "Use in editor"), "Review selects the proposal without saving"));
    equal(posts.length, writesBeforeReview, "Review makes no API writes");
    equal(doc.revision, 2, "Review leaves the saved revision unchanged");

    await click(buttonIn(strip1(), "Use in editor"));
    const replacementDialog = () => host.querySelector('[role="dialog"][aria-label="Replace unsaved email changes?"]');
    await settle(() => assert(replacementDialog(), "dirty Use requires an explicit replacement confirmation"));
    assert((replacementDialog()!.textContent ?? "").includes("Saved content stays unchanged until you choose Save"), "confirmation explains that saved content remains unchanged until Save");
    await click(buttonIn(replacementDialog(), "Cancel"));
    await settle(() => assert(!replacementDialog() && strip1()!.querySelector('[data-version="draft"]')?.getAttribute("aria-pressed") === "true", "cancel closes only the dialog and leaves proposal review selected"));
    equal(posts.length, writesBeforeReview, "cancelled Use makes no API writes");
    equal(doc.revision, 2, "cancelled Use leaves saved content unchanged");
    await click(strip1()!.querySelector('[data-version="saved"]'));
    await settle(() => assert((host.querySelector("#cmp-subject") as HTMLInputElement).value === "Locally edited subject", "cancel preserves the local subject"));
    assert((host.querySelector("#cmp-preheader") as HTMLInputElement).value === "Local preheader", "cancel preserves the local preheader");
    assert((host.querySelector("#cmp-html-source") as HTMLTextAreaElement).value === localHtml, "cancel preserves full-document HTML source");
    assert(host.querySelector("[data-unsaved-bar]"), "cancel preserves dirty local draft state");

    await click(strip1()!.querySelector('[data-proposal-preview="prop-1"]'));
    await settle(() => assert(host.querySelector('[data-proposal-body="prop-1"]'), "proposal can be reviewed again after cancellation"));
    await click(buttonIn(strip1(), "Use in editor"));
    await settle(() => assert(replacementDialog(), "replacement requires confirmation again after re-review"));
    await click(buttonIn(replacementDialog(), "Replace local draft"));
    await settle(() => assert(!host.querySelector("[data-proposal-body]") && (host.querySelector("#cmp-subject") as HTMLInputElement | null)?.value === "Draft subject one", "confirmed Use replaces the local draft with the reviewed suggestion"));
    assert(host.querySelector("[data-unsaved-bar]"), "Use changes only the unsaved editor draft");
    equal(doc.revision, 2, "Use does not save a revision");
    equal(saveBodies, [], "Review and Use never call the save API");
    equal(applyBodies, [], "Review and Use never invoke direct Apply");
    assert(proposals["prop-1"].state === "pending", "using a suggestion does not decide or consume its proposal");

    // Explicit Save is the sole write; it carries the observed revision and invalidates approval.
    await click(byText(host, "button", "Save as v3"));
    await settle(() => assert(doc.revision === 3 && !doc.approval.valid, "explicit Save creates r3 and resets content approval"));
    equal(saveBodies, [{ campaign_id: "launch-1", subject: "Draft subject one", preheader: "", blocks: [{ type: "paragraph", text: "Draft copy one" }], expected_revision: 2 }], "Save uses the native contentClient payload pinned to the source revision");
    assert(posts.some((post) => post === "POST /api/app-installations/install-crm/contexts/ctx-a/content/campaigns"), "Save uses the existing campaign-content HTTP route");
    await openTab("overview");
    assert(host.querySelector("[data-campaign-status]")?.textContent === "Draft", "saved revision returns to unapproved Draft status");
    await openTab("email");
    await settle(() => assert(host.querySelector("#cmp-subject"), "inline subject editing remains available in Email"));
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
  const sendsHistory = host.querySelector('section[aria-label="Campaign sends"]');
  assert(sendsHistory, "Activity carries the sends history");
  assert((sendsHistory.textContent ?? "").includes("No sends") || (sendsHistory.textContent ?? "").includes("No campaign sends"), "empty history is reported honestly rather than as a receipt");
  assert(!(sendsHistory.textContent ?? "").includes("delivered"), "Activity does not imply inbox delivery without delivery evidence");
  const events = Array.from(host.querySelectorAll("[data-timeline] li")).map((li) => li.getAttribute("data-event"));
  if (readOnly) {
    equal(events, ["proposal-pending", "proposal-pending", "approval", "revision"], "pending drafts lead the timeline");
  } else {
    equal(events, ["proposal-pending", "approval", "revision", "proposal-discarded"], "activity reports the pending proposal, saved revision and actual discard without inventing an apply");
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
}

void main();
