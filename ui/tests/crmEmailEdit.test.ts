export {};
/**
 * CAD-1057: inline editing in the campaign Email tab. Mounted against
 * a stateful fake host: Visual block edits, pasted HTML and the plain
 * text override each save through `app_content_save` with exactly one
 * body kind, pinned to the revision the edits began on; a CAS
 * conflict keeps the local edits; read-only viewers get no editor;
 * the host footer is never editable.
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
  await clientXor();
  await operator("blocks");
  await operator("html");
  await readOnlyViewer();
  console.log("crm email edit checks passed");
}

/** The client refuses a body with both kinds before a byte is sent. */
async function clientXor() {
  const { contentClient } = require("../src/features/app-shell/contentClient") as typeof import("../src/features/app-shell/contentClient");
  const scope = { installId: "i", contextId: "c" };
  let sent = 0;
  const realFetch = globalThis.fetch;
  globalThis.fetch = (async () => { sent++; return new Response("{}"); }) as typeof fetch;
  const both: any = { campaignId: "c1", subject: "S", blocks: [{ type: "paragraph", text: "p" }], html: "<p>x</p>" };
  let refused = false;
  try { await contentClient.save(scope, both); } catch { refused = true; }
  const neither: any = { campaignId: "c1", subject: "S" };
  let refusedNeither = false;
  try { await contentClient.save(scope, neither); } catch { refusedNeither = true; }
  globalThis.fetch = realFetch;
  assert(refused && refusedNeither && sent === 0, "blocks XOR html is enforced client-side before any request");
}

async function mount(readOnly: boolean, htmlDoc: boolean) {
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
    blocks: htmlDoc ? [] : [{ type: "heading", text: "Hello" }, { type: "paragraph", text: "Hi there" }],
    mode: htmlDoc ? "html" : "blocks", html: htmlDoc ? "<p>Original html body</p>" : null, text_override: null,
    content_digest: "content-digest-2",
    approval: { revision: 2, digest: "content-digest-2", valid: true, scope: "content-only" },
  };
  const proposal = {
    proposal_id: "prop-1", install_id: "install-crm", context_id: "ctx-a", campaign_id: "launch-1",
    source_revision: 2, subject: "Draft subject", preheader: "", blocks: [{ type: "paragraph", text: "Draft copy" }],
    content_digest: "pd1", actor: "assistant", origin: "assistant-receipt", state: "pending",
    assistant_receipt: { message_id: "m1", agent: "crm-writer", request_id: "r1", install_id: "install-crm", context_id: "ctx-a", campaign_id: "launch-1", source_revision: 2 },
  };
  const saves: any[] = [];
  const renders: number[] = [];
  const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
  const realFetch = globalThis.fetch;
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const path = String(input);
    const url = new URL(path, "http://localhost");
    const method = init?.method ?? "GET";
    const parts = url.pathname.split("/");
    if (method === "POST" && url.pathname === "/api/crm-smtp/show") return json({ error: "CRM SMTP sender refused or unavailable" }, 409);
    if (url.pathname === "/api/crm-send/list") return json({ sends: [] });
    if (url.pathname === "/api/crm-send/origin") return json({ unsubscribe_origin: null, stored: false });
    if (path === "/api/connections") return json({ connections: [] });
    if (method === "POST" && url.pathname.endsWith("/audience/preview")) {
      return json({ base: {}, base_count: 0, exclusion_list_id: null, exclusion_count: 0, final_excluded: { invalid_email: 0, no_consent: 0, unsubscribed: 0, suppressed: 0 }, final_count: 0, sample: [], digest: "d" });
    }
    if (path.startsWith("/api/threads/master")) return json({ entries: [], more_before: false });
    if (url.pathname.endsWith("/segments/list")) return json({ segments: [] });
    if (url.pathname.endsWith("/exclusions/list")) return json({ exclusions: [] });
    if (url.pathname.endsWith("/suppressions/list")) return json({ suppressions: [] });
    if (url.pathname.endsWith("/content/campaigns/list")) return json({ contents: [doc] });
    if (url.pathname.endsWith("/content/proposals/list")) return json({ proposals: htmlDoc ? [proposal] : [] });
    if (method === "GET" && url.pathname.includes("/content/proposals/") && url.pathname.endsWith("/render")) {
      return json({ render: {
        proposal_id: "prop-1", campaign_id: "launch-1", install_id: "install-crm", context_id: "ctx-a", state: "pending",
        source_revision: 2, content_digest: "pd1", binding: { binding_id: "preview", revision: 0, digest: "b", preview_only: true },
        preview_only: true, send_ready: false, sender: { name: "CRM News", address: "news@example.com" },
        unsubscribe_url: "https://unsub.example.invalid", html: "<p>draft</p>", text: "draft", render_digest: "sha256:p",
      } });
    }
    if (method === "POST" && url.pathname.endsWith("/campaigns")) {
      const body = JSON.parse(String(init?.body));
      saves.push(body);
      if (body.expected_revision !== doc.revision) return json({ error: "email revision is stale" }, 409);
      if (("blocks" in body) === ("html" in body)) return json({ error: "content save needs exactly one of blocks or html" }, 400);
      doc.revision += 1;
      doc.subject = body.subject; doc.preheader = body.preheader;
      doc.mode = "html" in body ? "html" : "blocks";
      doc.html = body.html ?? null;
      doc.blocks = body.blocks ?? [];
      doc.text_override = body.text ?? null;
      doc.content_digest = `content-digest-${doc.revision}`;
      doc.approval = { revision: null, digest: null, valid: false, scope: "content-only" };
      return json({ content: doc });
    }
    if (method === "POST" && url.pathname.endsWith("/render")) {
      renders.push(doc.revision);
      return json({ render: {
        campaign_id: doc.campaign_id, install_id: "install-crm", context_id: "ctx-a",
        revision: doc.revision, content_digest: doc.content_digest, sample_first_name: null,
        binding: { binding_id: "preview", revision: 1, digest: "binding-digest", preview_only: true },
        preview_only: true, send_ready: false, sender: { name: "CRM News", address: "news@example.com" },
        unsubscribe_url: "https://unsub.example.invalid/preview",
        html: `<h1>HOSTRENDER v${doc.revision} ${doc.subject}</h1>`, text: doc.text_override ?? `GENERATED v${doc.revision}`,
        render_digest: "sha256:render",
      } });
    }
    if (url.pathname.includes("/content/campaigns/")) {
      if (parts.at(-1) === doc.campaign_id) return json({ content: doc });
      return json({ error: "email content is unavailable for this installation and context" }, 404);
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
  async function type(element: Element | null, value: string) {
    assert(element, "typing target exists");
    const proto = element instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    await React.act(async () => {
      Object.getOwnPropertyDescriptor(proto, "value")!.set!.call(element, value);
      element.dispatchEvent(new Event(element instanceof HTMLTextAreaElement ? "input" : "input", { bubbles: true }));
    });
    await flush();
  }
  const q = (selector: string) => host.querySelector(selector);
  const byText = (scope: ParentNode, tag: string, label: string) =>
    Array.from(scope.querySelectorAll(tag)).find((el) => (el.textContent ?? "").trim() === label) ?? null;
  const bar = () => q("[data-unsaved-bar]");
  const barButton = (label: string) => Array.from(bar()?.querySelectorAll("button") ?? []).find((b) => (b.textContent ?? "").trim() === label) ?? null;
  const text = () => host.textContent ?? "";

  win.sessionStorage.clear();
  history.pushState(null, "", "/app-installations/install-crm?ctx=ctx-a&crm=campaigns&record=launch-1");
  await React.act(async () => {
    root.render(React.createElement(AppShell, { installId: "install-crm", viewer: { operator: true, readOnly } }));
  });
  await settle(() => assert(q('[data-tab="email"]'), "the tabbed detail renders"));
  await click(q('[data-tab="email"]'));
  await settle(() => assert(q('iframe[data-preview="visual"]'), "the Email tab renders the host preview"));
  const finish = async () => {
    await React.act(async () => { root.unmount(); });
    globalThis.fetch = realFetch;
    loader.prototype.require = originalRequire;
    host.remove();
  };
  return { React, doc, saves, renders, settle, click, type, q, byText, bar, barButton, text, flush, finish, host };
}

async function operator(kind: "blocks" | "html") {
  const t = await mount(false, kind === "html");
  const { doc, saves, renders, settle, click, type, q, byText, bar, barButton, text } = t;
  const bodyKinds = () => saves.map((s) => [("blocks" in s), ("html" in s)]);

  // The host footer is locked: shown, never an input.
  const footer = q("[data-host-footer]");
  if (kind === "blocks") {
    assert(footer && footer.textContent!.includes("host footer"), "the locked host footer renders");
    assert(!footer.querySelector("input, textarea, [contenteditable]") && !footer.closest("textarea"), "the host footer is not editable");
  }

  if (kind === "blocks") {
    // Visual: edit text inline, add a block; the bar names the versions.
    assert(!bar(), "no unsaved bar before an edit");
    await type(q('textarea[aria-label="Paragraph, block 2"]'), "Hi there, edited");
    await settle(() => assert(bar(), "an inline edit raises the unsaved bar"));
    const copy = bar()!.textContent ?? "";
    assert(copy.includes("Unsaved changes to v2") && copy.includes("Saving creates v3") && copy.includes("resets approval"), "the bar names vN, vN+1 and the approval reset");
    await click(byText(q('[aria-label="Add block"]')!, "button", "Button"));
    await type(q('input[aria-label="Button label, block 3"]'), "Join");
    await type(q('input[aria-label="Button link, block 3"]'), "https://example.com/join");
    await type(q("#cmp-subject"), "New subject");
    const rendersBefore = renders.length;
    await click(barButton("Save as v3"));
    await settle(() => assert(saves.length === 1, "the save request left"));
    const body = saves[0];
    equal(body.blocks, [{ type: "heading", text: "Hello" }, { type: "paragraph", text: "Hi there, edited" }, { type: "button", label: "Join", url: "https://example.com/join" }], "a visual edit saves blocks");
    assert(!("html" in body) && !("text" in body), "a blocks save carries no html and no text override");
    equal([body.subject, body.expected_revision], ["New subject", 2], "the save is pinned to the revision the edit began on");
    await settle(() => assert(renders.length > rendersBefore && ((q('iframe[data-preview="visual"]') as HTMLIFrameElement).getAttribute("srcdoc") ?? "").includes("HOSTRENDER v3 New subject"), "the host preview re-renders the new version after save"));
    await settle(() => assert(!bar() && text().includes("Saved v3") && text().includes("approval reset"), "the approval-reset message shows after save"));
    equal(doc.approval.valid, false, "the save reset approval on the host");

    // HTML paste: html body only, blocks dropped from the body.
    await click(byText(t.host, "button", "HTML"));
    const source = q("#cmp-html-source") as HTMLTextAreaElement;
    assert(source && source.classList.contains("srcedit"), "HTML mode is a source editor");
    assert((q("[data-html-note]")?.textContent ?? "").includes("sanitises") && (q("[data-html-note]")?.textContent ?? "").includes("footer"), "HTML mode says the host sanitises and adds the footer");
    await type(source, "<p>Pasted <b>body</b></p><script>x()</script>");
    await settle(() => assert(bar(), "pasting raises the bar"));
    await click(barButton("Save as v4"));
    await settle(() => assert(saves.length === 2, "the html save left"));
    equal(Object.keys(saves[1]).filter((k) => k === "blocks" || k === "html"), ["html"], "an HTML paste saves html and never blocks");
    equal(saves[1].html, "<p>Pasted <b>body</b></p><script>x()</script>", "the paste goes to the host unmodified (the host sanitises)");
    assert(saves[1].expected_revision === 3, "the html save is pinned to v3");
    await settle(() => assert(text().includes("Saved v4"), "html save lands"));
    // After saving an HTML body, Visual explains instead of faking blocks.
    await click(byText(t.host, "button", "Visual"));
    assert(q('[data-state="html-body"]') && !q('[aria-label="Email body blocks"]'), "an HTML body is not editable as blocks");

    // Text override: generated by default, Write my own enables it.
    await click(byText(t.host, "button", "Text"));
    assert(!q("#cmp-text-override"), "no override textarea until Write my own is ticked");
    assert((q('pre[data-preview="text"]')?.textContent ?? "").includes("GENERATED"), "the generated text is the default");
    await click(Array.from(t.host.querySelectorAll("label")).find((l) => (l.textContent ?? "").includes("Write my own"))!.querySelector("input"));
    await type(q("#cmp-text-override"), "My own plain text");
    await click(barButton("Save as v5"));
    await settle(() => assert(saves.length === 3, "the text-override save left"));
    equal([saves[2].text, "html" in saves[2], "blocks" in saves[2]], ["My own plain text", true, false], "the override rides with exactly one body kind");
    await settle(() => assert((q('pre[data-preview="text"]')?.textContent ?? "").includes("My own plain text"), "the host text render shows the override"));
    // Untick: the override is cleared on the next save (no text key).
    await click(Array.from(t.host.querySelectorAll("label")).find((l) => (l.textContent ?? "").includes("Write my own"))!.querySelector("input"));
    await click(barButton("Save as v6"));
    await settle(() => assert(saves.length === 4 && !("text" in saves[3]), "unticking Write my own saves without an override"));
    assert(bodyKinds().every(([b, h]) => b !== h), "every save carries exactly one of blocks or html");

    // CAS conflict keeps local edits and says why.
    await click(byText(t.host, "button", "Visual"));
    doc.revision += 1; // a concurrent writer saved v8 behind the editor's back
    await type(q("#cmp-subject"), "My unsaved subject");
    await click(barButton("Save as v7"));
    await settle(() => assert(text().includes("since you started editing"), "the conflict says the email moved"));
    equal((q("#cmp-subject") as HTMLInputElement).value, "My unsaved subject", "a refused save keeps the local edit");
    assert(bar(), "the bar stays so the operator can decide");
    equal(saves.at(-1).expected_revision, 6, "the refused save was pinned to the source revision");
    await click(barButton("Discard"));
    await settle(() => assert(!bar(), "Discard clears the edits"));
  } else {
    // An HTML-bodied email: Visual is read-only text, HTML is the editor,
    // and Apply warns that it replaces the HTML body.
    assert(q('[data-state="html-body"]') && !q('[aria-label="Email body blocks"]'), "Visual does not pretend an HTML body is blocks");
    assert((q('[data-proposal="prop-1"] [data-state="replaces-html"]')?.textContent ?? "").includes("replaces this email's HTML body"), "Apply warns it replaces an HTML body with blocks");
    await click(byText(t.host, "button", "HTML"));
    equal((q("#cmp-html-source") as HTMLTextAreaElement).value, "<p>Original html body</p>", "the saved HTML opens in the source editor");
    await type(q("#cmp-html-source"), "<p>Edited html</p>");
    await click(barButton("Save as v3"));
    await settle(() => assert(saves.length === 1 && saves[0].html === "<p>Edited html</p>" && !("blocks" in saves[0]), "an HTML edit saves html only"));
  }
  await t.finish();
}

async function readOnlyViewer() {
  const t = await mount(true, false);
  const { q, text, host } = t;
  assert(!q("#cmp-subject") && !q("#cmp-preheader"), "a read-only viewer gets no subject editor");
  assert(!q('[aria-label="Add block"]') && !q('[aria-label="Email body blocks"]') && !q('section[aria-label="Email preview"] textarea'), "a read-only viewer gets no block, source or text editor");
  assert(!q("[data-unsaved-bar]"), "a read-only viewer never sees the save bar");
  assert(text().includes("Read-only view"), "a read-only viewer is told so");
  assert((q('[aria-label="Email envelope"]')?.textContent ?? "").includes("Welcome aboard"), "the envelope still shows the subject");
  void host;
  await t.finish();
}

void main();
