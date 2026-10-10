export {};
/**
 * CAD-1058 New campaign dialog: the short modal replaces the long New
 * page. Name (required, human) first; Assistant draft / Paste HTML /
 * Blank; optional audience (also preselected from the segment drawer's
 * ?segment=); the primary text follows the choice; it lands on the
 * saved campaign's Email tab. The list and header show the human name
 * (fallback: subject) and the id lives under Details.
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
  await clientBody();
  grammar();
  await mounted();
  console.log("crm new campaign checks passed");
}

/** The save body carries `name` only as the typed field; nothing else rides along. */
async function clientBody() {
  const { contentClient } = require("../src/features/app-shell/contentClient") as typeof import("../src/features/app-shell/contentClient");
  const scope = { installId: "i", contextId: "c" };
  const realFetch = (globalThis as any).fetch;
  const sent: any[] = [];
  (globalThis as any).fetch = async (_path: string, init: any) => {
    sent.push(JSON.parse(init.body));
    return new Response(JSON.stringify({ content: {} }), { status: 200, headers: { "Content-Type": "application/json" } });
  };
  try {
    await contentClient.save(scope, { campaignId: "c1", name: "Spring", subject: "S", html: "<p>x</p>" });
    equal(Object.entries(sent[0]).sort(), Object.entries({ campaign_id: "c1", name: "Spring", subject: "S", preheader: "", html: "<p>x</p>" }).sort(), "name rides the save body");
    await contentClient.save(scope, { campaignId: "c1", subject: "S", blocks: [{ type: "paragraph", text: "x" }] });
    assert(!("name" in sent[1]), "an unnamed save sends no name key");
    let refused = false;
    try { await (contentClient as any).save(scope, { campaignId: "c1", subject: "S", html: "<p>x</p>", actor: "operator" }); } catch { refused = true; }
    assert(refused, "a forged field is still refused client-side");
    equal(sent.length, 2, "the forged save never left the browser");
  } finally {
    (globalThis as any).fetch = realFetch;
  }
}

function grammar() {
  const { checkCampaignName, campaignTitle, parseContentDoc } = require("../src/features/app-shell/campaignGrammar") as typeof import("../src/features/app-shell/campaignGrammar");
  for (const ok of ["Spring launch", "A", "n".repeat(80), "Café 2026 — VIPs"]) checkCampaignName(ok);
  for (const bad of ["", " padded", "padded ", "n".repeat(81), "<b>x</b>", "a\nb", "a{{b}}", "`x`", "a\u0007b"]) {
    let refused = false;
    try { checkCampaignName(bad); } catch { refused = true; }
    assert(refused, `name refused client-side: ${JSON.stringify(bad)}`);
  }
  equal(campaignTitle({ name: "Spring", subject: "Hello" }), "Spring", "the name leads");
  equal(campaignTitle({ name: null, subject: "Hello" }), "Hello", "the subject is the fallback");
  const base = { campaign_id: "c", revision: 1, subject: "S", preheader: "", blocks: [], content_digest: "d" };
  equal(parseContentDoc({ content: { ...base, name: "N" } }).name, "N", "name parses");
  equal(parseContentDoc({ content: { ...base, name: null } }).name, null, "null name parses");
  equal(parseContentDoc({ content: { ...base, name: 7 } }).name, null, "a non-string name reads as none");
  equal(parseContentDoc({ content: base }).name, null, "an older receipt has no name");
}

async function mounted() {
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
  const contents: Record<string, any> = {
    "legacy-1": {
      campaign_id: "legacy-1", install_id: "install-crm", context_id: "ctx-a", revision: 1, name: null,
      subject: "Old subject only", preheader: "", blocks: [{ type: "paragraph", text: "Hi" }],
      content_digest: "d1", approval: { revision: null, digest: null, valid: false, scope: "content-only" },
    },
    "named-1": {
      campaign_id: "named-1", install_id: "install-crm", context_id: "ctx-a", revision: 1, name: "Autumn sale",
      subject: "Big autumn savings", preheader: "", blocks: [{ type: "paragraph", text: "Hi" }],
      content_digest: "d2", approval: { revision: null, digest: null, valid: false, scope: "content-only" },
    },
  };
  const saveBodies: any[] = [];
  const sendBodies: any[] = [];
  const createBodies: any[] = [];
  const convs: any[] = [{ id: "conv-general", subject: null, is_general: true, title: null }];
  let failNextSend = false;
  const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
  const refused = (message: string, status = 409) => json({ error: message }, status);
  const realFetch = globalThis.fetch;
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const path = String(input);
    const url = new URL(path, "http://localhost");
    const method = init?.method ?? "GET";
    const parts = url.pathname.split("/");
    if (url.pathname === "/api/crm-send/list") return json({ sends: [] });
    if (url.pathname === "/api/crm-send/origin") return json({ unsubscribe_origin: null, stored: false });
    if (url.pathname === "/api/crm-smtp/show") return refused("CRM SMTP sender refused or unavailable", 409);
    if (path === "/api/connections") return json({ connections: [] });
    if (path === "/api/app-installations/install-crm") return json(install);
    if (path === "/api/app-installations/install-crm/contexts") return json({ contexts: [
      { id: "ctx-a", install_id: "install-crm", revision: 1, state: "active", digest: "ca", config: { schema: 1, label: "Acme", input_defaults: {} } },
    ] });
    // CAD-1098: conversations — General plus one per campaign subject (idempotent).
    if (url.pathname === "/api/app-installations/install-crm/conversations") {
      if (method === "POST") {
        const body = JSON.parse(String(init!.body));
        createBodies.push(body);
        const c = { id: `conv-${body.subject}`, subject: body.subject, is_general: false, title: null };
        if (!convs.find((x: any) => x.id === c.id)) convs.push(c);
        return json({ conversation: c });
      }
      return json({ conversations: convs });
    }
    if (method === "POST" && url.pathname === "/api/threads/master/messages") {
      const body = JSON.parse(String(init!.body));
      sendBodies.push(body);
      if (failNextSend) { failNextSend = false; return refused("the assistant is busy", 503); }
      return json({ message: body.message, state: "queued", duplicate: false });
    }
    if (path.startsWith("/api/threads/master")) return json({ entries: [], more_before: false });
    if (method === "POST" && url.pathname.endsWith("/audience/preview")) {
      return json({ base: {}, base_count: 4, exclusion_list_id: null, exclusion_count: 0, final_excluded: { invalid_email: 0, no_consent: 0, unsubscribed: 0, suppressed: 0 }, final_count: 4, sample: [], digest: "aud" });
    }
    if (url.pathname.endsWith("/segments/list")) return json({ segments: [{ id: "seg-vip", name: "VIP customers" }, { id: "seg-all", name: "Everyone" }] });
    if (url.pathname.endsWith("/exclusions/list")) return json({ exclusions: [] });
    if (url.pathname.endsWith("/suppressions/list")) return json({ suppressions: [] });
    if (url.pathname.endsWith("/content/proposals/list")) return json({ proposals: [] });
    if (url.pathname.endsWith("/content/campaigns/list")) return json({ contents: Object.values(contents) });
    if (method === "POST" && url.pathname.endsWith("/content/campaigns")) {
      const body = JSON.parse(String(init!.body));
      saveBodies.push(body);
      contents[body.campaign_id] = {
        campaign_id: body.campaign_id, install_id: "install-crm", context_id: "ctx-a", revision: 1,
        name: body.name ?? null, subject: body.subject, preheader: body.preheader ?? "", blocks: body.blocks ?? [],
        ...(body.html === undefined ? {} : { html: body.html, mode: "html" }),
        content_digest: "dn", approval: { revision: null, digest: null, valid: false, scope: "content-only" },
      };
      return json({ content: contents[body.campaign_id] });
    }
    if (method === "POST" && url.pathname.endsWith("/render")) {
      const doc = contents[parts.at(-2)!];
      return json({ render: {
        campaign_id: doc.campaign_id, install_id: "install-crm", context_id: "ctx-a", revision: doc.revision,
        content_digest: doc.content_digest, sample_first_name: null,
        binding: { binding_id: "preview", revision: 1, digest: "b", preview_only: true },
        preview_only: true, send_ready: false, sender: { name: "CRM News", address: "news@example.com" },
        unsubscribe_url: "https://unsub.example.invalid/preview", html: "<p>x</p>", text: "x", render_digest: "sha256:r",
      } });
    }
    if (url.pathname.includes("/content/campaigns/")) {
      const doc = contents[parts.at(-1)!];
      return doc ? json({ content: doc }) : refused("email content is unavailable for this installation and context", 404);
    }
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
  const byText = (tag: string, label: string) =>
    Array.from(host.querySelectorAll(tag)).find((el) => (el.textContent ?? "").trim() === label) ?? null;
  async function fill(selector: string, value: string) {
    const element = host.querySelector(selector) as HTMLInputElement | HTMLTextAreaElement;
    assert(element, `field exists: ${selector}`);
    const proto = element instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    await React.act(async () => {
      Object.getOwnPropertyDescriptor(proto, "value")!.set!.call(element, value);
      element.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await flush();
  }
  const dialog = () => host.querySelector('[role="dialog"][aria-labelledby="crm-newc-title"]');
  const openDialog = async (viewer: { operator: boolean; readOnly: boolean }, search = "") => {
    win.sessionStorage.clear();
    history.pushState(null, "", `/app-installations/install-crm?ctx=ctx-a&crm=campaigns${search}`);
    await React.act(async () => { root.render(React.createElement(AppShell, { installId: "install-crm", viewer })); });
    await settle(() => assert(host.querySelector('section[aria-label="Campaigns list"]'), "the campaigns list renders"));
  };
  const operator = { operator: true, readOnly: false };

  // ---- List: the human name leads, the subject is the fallback, ids stay out of the row.
  await openDialog(operator);
  await settle(() => assert(text().includes("Autumn sale") && text().includes("Old subject only"), "list shows names and fallback subjects"));
  const rows = Array.from(host.querySelectorAll("tbody tr")).map((tr) => tr.textContent ?? "");
  assert(rows.some((r) => r.includes("Autumn sale") && r.includes("Big autumn savings")), "a named row keeps the subject as the secondary line");
  assert(!rows.some((r) => r.includes("named-1") || r.includes("legacy-1")), "no campaign ids on the list rows");

  // ---- Read-only and non-operator viewers never get the dialog.
  await React.act(async () => { root.unmount(); });
  const rootRo = createRoot(host);
  win.sessionStorage.clear();
  history.pushState(null, "", "/app-installations/install-crm?ctx=ctx-a&crm=campaigns&appview=new");
  await React.act(async () => { rootRo.render(React.createElement(AppShell, { installId: "install-crm", viewer: { operator: true, readOnly: true } })); });
  await settle(() => assert(host.querySelector('section[aria-label="Campaigns list"]'), "read-only list renders"));
  assert(!dialog(), "a read-only viewer gets no dialog even on ?appview=new");
  assert(!byText("button", "New campaign"), "a read-only viewer has no New campaign button");
  await React.act(async () => { rootRo.unmount(); });

  const root2 = createRoot(host);
  const mount = async (search: string) => {
    win.sessionStorage.clear();
    history.pushState(null, "", `/app-installations/install-crm?ctx=ctx-a&crm=campaigns${search}`);
    await React.act(async () => { root2.render(React.createElement(AppShell, { installId: "install-crm", viewer: operator })); });
    await settle(() => assert(host.querySelector('section[aria-label="Campaigns list"]'), "list renders"));
  };

  // ---- Segment drawer's "Use in campaign" (?segment=) preselects the audience;
  // Paste HTML saves through the operator html path; lands on Email.
  await mount("&appview=new&segment=seg-vip");
  await settle(() => assert(dialog(), "?appview=new opens the dialog"));
  await settle(() => assert((dialog()!.textContent ?? "").includes("VIP customers"), "the segment from ?segment= is preselected in the dialog"));
  await click(host.querySelector('[data-start="html"]'));
  assert(dialog()!.textContent!.includes("Create from HTML"), "primary follows Paste HTML");
  await fill('[role="dialog"] input', "VIP thanks");
  await click(byText("button", "Create from HTML"));
  assert(text().includes("Paste the email HTML"), "an empty HTML start is refused");
  equal(saveBodies.length, 0, "nothing saved yet");
  await fill('[role="dialog"] textarea', "<h1>Thanks {{first_name|friend}}</h1><script>alert(1)</script>");
  await click(byText("button", "Create from HTML"));
  await settle(() => assert(saveBodies.length === 1, "one save"));
  equal(Object.keys(saveBodies[0]).sort(), ["campaign_id", "html", "name", "preheader", "subject"], "an html save carries html and no blocks");
  equal(saveBodies[0].name, "VIP thanks", "the name is saved");
  equal(sendBodies.length, 0, "Paste HTML sends nothing to the chat");
  const htmlId = saveBodies[0].campaign_id as string;
  await settle(() => assert(location.search.includes(`record=${htmlId}`) && !location.search.includes("appview") && !location.search.includes("segment"), "lands on the record and drops the dialog params"));
  await settle(() => assert(host.querySelector('[data-tab="email"][aria-selected="true"]'), "lands on the Email tab"));
  assert((host.querySelector("[data-outlet-heading]")?.textContent ?? "").includes("VIP thanks"), "header shows the name");
  // The chosen segment follows the operator to the Audience tab (one-shot).
  await click(host.querySelector('[data-tab="audience"]'));
  await settle(() => assert(host.querySelector('[role="radiogroup"][aria-label="Base audience mode"]'), "audience tab renders"));
  await settle(() => assert((host.querySelector("#aud-segment")?.textContent ?? "").includes("VIP customers"), "the preselected segment is the campaign's audience pick"));

  // ---- Blank: one heading + one paragraph block, nothing sent to the chat.
  await mount("");
  await click(byText("button", "New campaign"));
  await settle(() => assert(dialog(), "New campaign button opens the dialog"));
  await click(host.querySelector('[data-start="blank"]'));
  assert(text().includes("Opens the editor with a heading and a paragraph."), "Blank explains itself");
  await fill('[role="dialog"] input', "Plain start");
  await click(byText("button", "Create blank campaign"));
  await settle(() => assert(saveBodies.length === 2, "second save"));
  equal(saveBodies[1].blocks.map((b: any) => b.type), ["heading", "paragraph"], "blank is one heading and one paragraph");
  assert(saveBodies[1].html === undefined, "blank carries no html");
  equal(sendBodies.length, 0, "Blank sends nothing to the chat");
  await settle(() => assert(host.querySelector('[data-tab="email"][aria-selected="true"]'), "blank lands on Email"));
  // The landing is one-shot: opening another campaign starts on Overview.
  await click(byText("button", "← Campaigns"));
  await settle(() => assert(host.querySelector('section[aria-label="Campaigns list"]'), "back to the list"));
  const open = Array.from(host.querySelectorAll("tbody tr")).find((tr) => (tr.textContent ?? "").includes("Autumn sale"))!.querySelector("button")!;
  await click(open);
  await settle(() => assert(host.querySelector('[data-tab="overview"][aria-selected="true"]'), "a later campaign opens on Overview"));

  // ---- CAD-1097: an untouched brief is empty; Create sends and saves nothing.
  await mount("&appview=new");
  await settle(() => assert(dialog(), "dialog opens"));
  equal((host.querySelector('[role="dialog"] textarea') as HTMLTextAreaElement).value, "", "the brief starts empty");
  await fill('[role="dialog"] input', "Untouched brief");
  const savesBefore = saveBodies.length;
  const sendsBefore = sendBodies.length;
  await click(byText("button", "Create and draft"));
  await settle(() => assert(dialog() && text().includes("Describe what the email should say"), "an empty brief is refused"));
  equal(saveBodies.length, savesBefore, "nothing is saved without a brief");
  equal(sendBodies.length, sendsBefore, "no example text is sent");

  // ---- Assistant draft with a failed chat send: saved once, retry sends the brief only.
  await mount("&appview=new");
  await settle(() => assert(dialog(), "dialog opens"));
  await fill('[role="dialog"] input', "Brief retry");
  await fill('[role="dialog"] textarea', "Welcome people warmly.");
  failNextSend = true;
  await click(byText("button", "Create and draft"));
  await settle(() => assert(text().includes("the brief was not sent to the assistant"), "a failed brief is reported, not swallowed"));
  assert(dialog(), "the dialog stays so the operator can retry");
  equal(saveBodies.length, 3, "the campaign was saved once");
  assert(byText("button", "Send brief again") && byText("button", "Open without the brief"), "retry and skip are offered");
  await click(byText("button", "Send brief again"));
  await settle(() => assert(!dialog(), "the retry lands"));
  equal(saveBodies.length, 3, "a retry never saves a second campaign");
  equal(sendBodies.length, 2, "the retry sent the brief again");
  equal(sendBodies.at(-1).app, { install_id: "install-crm", context_id: "ctx-a" }, "the brief is scoped");
  assert(String(sendBodies.at(-1).text).includes("Welcome people warmly."), "the brief text is sent");
  // CAD-1098: the brief lands in the campaign's own conversation, opened once per campaign.
  const briefCampaign = saveBodies[2].campaign_id as string;
  equal(sendBodies.at(-1).conversation, `conv-campaign:${briefCampaign}`, "the brief goes to the campaign conversation");
  equal(createBodies.filter((b: any) => b.subject === `campaign:${briefCampaign}`).length, 2, "each attempt opens the same subject (idempotent server side)");
  assert(createBodies.every((b: any) => String(b.subject).startsWith("campaign:")), "only campaign subjects are created here");

  // ---- Name rules: a bad name refuses before any request; Escape and Cancel close with nothing saved.
  await mount("&appview=new");
  await settle(() => assert(dialog(), "dialog opens"));
  await click(host.querySelector('[data-start="blank"]'));
  for (const bad of ["<b>x</b>", "a{{b}}"]) {
    await fill('[role="dialog"] input', bad);
    await click(byText("button", "Create blank campaign"));
    assert(text().includes("Give the campaign a short plain name"), `refused: ${bad}`);
  }
  equal(saveBodies.length, 3, "no bad name reached the server");
  await React.act(async () => { dialog()!.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })); });
  await settle(() => assert(!dialog() && !location.search.includes("appview"), "Escape closes the dialog and leaves the New view"));
  equal(saveBodies.length, 3, "closing saved nothing");

  await React.act(async () => { root2.unmount(); });
  globalThis.fetch = realFetch;
  host.remove();
}

void main();
