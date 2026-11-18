import type * as React from "react";

declare function require(name: string): unknown;

async function main() {
  const { Window } = require("happy-dom") as typeof import("happy-dom");
  const win = new Window({ url: "http://localhost/settings/templates", settings: { disableIframePageLoading: true } });
  const w = win as unknown as Record<string, unknown>;
  const def = (k: string, v: unknown) => Object.defineProperty(globalThis, k, { value: v, configurable: true, writable: true });
  for (const k of "window document Node Element HTMLElement HTMLInputElement HTMLTextAreaElement navigator Event MouseEvent location history sessionStorage localStorage".split(" ")) def(k, k === "window" ? win : w[k]);
  for (const k of "addEventListener removeEventListener requestAnimationFrame cancelAnimationFrame".split(" ")) if (typeof w[k] === "function") def(k, (w[k] as Function).bind(win));
  def("IS_REACT_ACT_ENVIRONMENT", true);

  const mod = require("module") as { prototype: { require: (id: string) => unknown } }, origReq = mod.prototype.require;
  mod.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return origReq.call(this, id);
  };

  const R = require("react") as typeof React, E = R.createElement, A = R.act;
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const Templates = (require("../src/features/templates/Templates") as typeof import("../src/features/templates/Templates")).default;
  const CanvasDrawer = (require("../src/features/canvas/CanvasDrawer") as { default: React.ComponentType }).default;
  const ArtifactCard = (require("../src/features/canvas/ArtifactCard") as { default: React.ComponentType<{ entry: { id: string; kind: string; payload: unknown; created: number } }> }).default;
  const { WriteGate } = require("../src/features/auth/WriteGate") as typeof import("../src/features/auth/WriteGate");
  const ps = require("../src/lib/privateSession") as typeof import("../src/lib/privateSession");
  const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
  const cs = require("../src/features/canvas/canvasStore") as typeof import("../src/features/canvas/canvasStore");

  const ok: (c: unknown, m: string) => asserts c = (c, m) => { if (!c) throw new Error(m); };
  const flush = async () => { for (let r = 0; r < 3; r++) await A(async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); }); };
  const origF = globalThis.fetch, origC = URL.createObjectURL, origR = URL.revokeObjectURL, live = new Map<string, Blob>();
  let nb = 0, intake = 0, st: "draft" | "approved" = "draft";
  URL.createObjectURL = (b) => { ok(b instanceof Blob, "blob"); const u = `blob:h/${++nb}`; live.set(u, b); return u; };
  URL.revokeObjectURL = (u) => { live.delete(u); };

  const host = document.createElement("div"); document.body.append(host); const root = createRoot(host);
  const calls: Array<{ p: string; m: string; b: Record<string, unknown> | null }> = [];
  const sha = "1".repeat(64), doc = (s: string) => `<!doctype html><html><body>${s}</body></html>`, j = (d: unknown) => new Response(JSON.stringify(d));
  const T = "/api/templates/", D = "/api/documents/", NI = 'input[placeholder="quotation-acme"]';
  const HQ = "html-quote", NW = "Northwind Ltd", HP = "HTML5 + print CSS", CDV = "Create draft version", RR = "Read reference", SV = "Show versions", ST = "Save as Template", SAT = "[data-save-as-template]", FRM = "iframe.templates-frame", RO = "[data-templates-readonly]";
  const SPEC = { kind: "quotation", fields: [{ key: "customer", label: "Customer", kind: "text", required: true }], fixed_text: {}, calculation: { confirmed: true, currency: "HKD", tax_rate_bps: 0, discount_bps: 0 } };
  const tpl = (o: Record<string, unknown> = {}) => ({ name: HQ, version: 1, based_on: null, status: st, kind: "quotation", source_format: "html", content_sha256: sha, source_sha256: "2".repeat(64), approved_source: st === "approved" ? "owner" : null, created: 1, ...o });
  const artH = doc("{{ customer }}{% for item in items %}{{ item.description }}{% endfor %}"), concH = doc(`<p>${NW}</p>`), sugH = doc("Suggested {{ customer }}");
  const pv = (f: string, ev: unknown) => ({ file: f, provenance: { source: { filename: f, sha256: "r1", preserved_as: null }, evidence: ev }, suggested_html: sugH });
  const ref = (id: string, title: string) => ({ id, version: 1, kind: "html" as const, title, bytes: 95 });

  globalThis.fetch = async (inp, init) => {
    const p = String(inp), m = (init?.method ?? "GET").toUpperCase();
    const b = typeof init?.body === "string" ? (JSON.parse(init.body) as Record<string, unknown>) : null;
    calls.push({ p, m, b });
    if (p.startsWith("/api/artifacts/")) return new Response(p.includes("concrete") ? concH : artH);
    if (p === `${T}list`) return j({ templates: [{ name: HQ, latest_version: 1, latest_approved_version: st === "approved" ? 1 : null, kind: "quotation", source_format: "html", status: st }, { name: "hosted-approved", latest_version: 2, approved_version: 2, kind: "invoice", source_format: "html", status: "approved" }] });
    if (p === `${T}create` || p === `${T}revise`) { st = "draft"; const r = p.endsWith("/revise"); return j({ template: tpl({ name: String(b?.["name"] ?? HQ), version: r ? 2 : 1, based_on: r ? Number(b?.["base"] ?? 1) : null, status: "draft", source_format: b?.["html"] ? "html" : "docx", approved_source: null }), spec: b?.["spec"], html: b?.["html"] }); }
    if (p.startsWith(`${T}show`)) return j(p.includes("&version=") ? { template: tpl(), spec: SPEC, html: doc("{{ customer }}") } : { name: HQ, versions: [tpl()] });
    if (p === `${T}intake`) return j(++intake === 1 ? pv("ref.docx", { format: "docx", paragraphs: ["Quotation for {{ customer }}"], tables: [[["A", "1"]]], placeholders: ["customer"] }) : pv("ref.pdf", { format: "pdf", pages: [{ page: 1, text: "Q" }] }));
    if (p.startsWith(`${T}sample`)) return j({ sample: true, template: HQ, version: 1, status: st, context: { customer: "SAMPLE Customer" }, html: doc("SAMPLE") });
    if (p === `${T}approve`) { ok(b?.["digest"] === sha, "digest"); st = "approved"; return j({ template: tpl({ status: "approved", approved_source: "owner" }), spec: SPEC }); }
    if (p === `${D}create`) { ok(b?.["allow_partial"] === true, "partial"); return j({ id: "d1", template: HQ, version: 1, template_sha256: sha, kind: "quotation", state: "draft", has_unresolved_pricing: true, input: b?.["data"] ?? {}, context: { customer: NW, has_unresolved_pricing: true }, created: 2 }); }
    if (p.startsWith(`${D}export`)) return j(m === "POST" ? { id: "d1", pdf_base64: btoa("%PDF-1.4\n%%EOF\n") } : { id: "d1", html: doc("UNRESOLVED") });
    throw new Error(`Unexpected: ${m} ${p}`);
  };

  const render = async (n: React.ReactNode, loc = true) => { await A(async () => root.render(loc && n ? E(LocaleProvider, null, n) : n)); await flush(); };
  const q = <T extends Element = Element>(s: string, b: ParentNode = host) => b.querySelector<T>(s)!;
  const qAll = <T extends Element = Element>(s: string, b: ParentNode = host) => Array.from(b.querySelectorAll<T>(s));
  const has = (t: string, s?: string, b: ParentNode = host) => Boolean((s ? q(s, b) : b).textContent?.includes(t));
  const btn = (l: string, ex = true) => qAll<HTMLButtonElement>("button").find((e) => ex ? e.textContent?.trim() === l : e.textContent?.includes(l));
  const tap = async (e: Element) => { await A(async () => { e.dispatchEvent(new MouseEvent("click", { bubbles: true })); }); await flush(); };
  const click = async (l: string) => { const b = btn(l); ok(b && !b.disabled, l); await tap(b); };
  const enterEl = async (el: HTMLInputElement | HTMLTextAreaElement, v: string) => { await A(async () => { Object.getOwnPropertyDescriptor(Object.getPrototypeOf(el), "value")!.set!.call(el, v); el.dispatchEvent(new Event("input", { bubbles: true })); }); await flush(); };
  const enter = (s: string, v: string, b: ParentNode = host) => enterEl(q(s, b), v);
  const pick = async (txt: string, name: string) => { const el = q<HTMLInputElement>('input[type="file"]'), f = new win.File([txt], name); f.arrayBuffer = async () => new TextEncoder().encode(txt).buffer; Object.defineProperty(el, "files", { value: [f], configurable: true }); await A(async () => { el.dispatchEvent(new Event("change", { bubbles: true })); }); await flush(); };
  const feed = async (r: ReturnType<typeof ref>, turn: string, open: boolean) => { await A(async () => { cs.feedCanvas({ conversation: "c1", composerKey: null, surface: "home", autoOpen: open, turn, working: null, cards: [{ key: `${r.id}@1`, ref: r, live: true, operatorTurn: true, turn }] }); if (open) cs.openArtifact(r); }); await flush(); };
  const mountCanvas = async () => { await render(E(CanvasDrawer)); await feed(ref("art-1", "Quote Draft"), "t1", true); };
  const call = (p: string, m?: string) => calls.find((c) => c.p.startsWith(p) && (!m || c.m === m));
  const areas = () => qAll<HTMLTextAreaElement>('section[aria-labelledby="templates-new"] textarea');
  const dl = (t: string) => qAll("a[download]").some((a) => a.textContent?.trim() === t);
  const sb = (s: string, b: ParentNode = host) => q<HTMLIFrameElement>(s, b).getAttribute("sandbox") === "";
  const showV = async (n: string) => { await enter(NI, n); await click(SV); };
  const save = async (sel: string, v: string, el: ParentNode) => { await enter(sel, v, el); await tap(q('input[type="checkbox"]', el)); await click("Save draft v1"); };

  try {
    ps.allowPrivateReads();
    await render(E(R.Activity, { mode: "visible", key: ps.privateSessionGeneration(), children: E(Templates) }), false);
    await click("Browse all templates");
    ok(call(`${T}list`, "GET") && has("(approved v2)", ".templates-directory-list"), "dir");
    await click(HP); await enter(NI, HQ);
    await enterEl(areas()[0]!, JSON.stringify(SPEC)); await enterEl(areas()[1]!, doc("{{ customer }}"));
    await click(CDV);
    const c1 = call(`${T}create`)?.b; ok(c1?.["html"] && !c1?.["docx_base64"], "create");

    await click("Legacy DOCX"); await pick("docx", "ref.docx"); await click(RR);
    ok(has("1 paragraph(s), 1 table(s)", ".templates-findings") && has("Quotation for {{ customer }}", ".templates-findings"), "findings");
    await pick("pdf", "ref.pdf"); await click(RR); await click("Use suggested HTML");
    ok(btn(HP)?.getAttribute("aria-pressed") === "true" && areas()[1]?.value === sugH, "sugH");

    await click("Preview sample v1 before approval");
    ok(call(`${T}sample`)?.p.includes("format=html") && sb(`section[data-sample] ${FRM}`) && (q<HTMLIFrameElement>(`section[data-sample] ${FRM}`).getAttribute("srcdoc") ?? "").includes("script-src 'none'") && dl("Download SAMPLE HTML"), "sample");

    await enter("input", "Owner UI test", q(".templates-approval-box")); await click("Approve v1 exactly");
    ok(call(`${T}approve`), "approve");

    await enter('section[aria-labelledby="templates-versions"] textarea', JSON.stringify({ customer: NW, items: [{ description: "A", quantity: 1 }] }));
    await tap(q('.templates-checkbox input[type="checkbox"]')); await click("Create draft from v1");
    ok(has("Contains UNRESOLVED pricing"), "UNRESOLVED");
    await click("Export HTML preview");
    ok(call(`${D}export`)?.p.includes("format=html") && sb(`section[data-draft] ${FRM}`), "expH");
    await click("Export PDF preview");
    ok(call(`${D}export`, "POST")?.b?.["format"] === "pdf" && dl("Download PDF"), "expP");

    await render(null, false); calls.length = 0; await mountCanvas(); await click(ST);
    const sf = q<HTMLFormElement>(SAT);
    ok(has("{{ customer }}", undefined, sf) && has("{{ item.description }}", undefined, sf), "ph");
    await save(NI, "canvas-saved-quote", sf);
    const dc = call(`${T}create`), sp = dc?.b?.["spec"] as { calculation?: { confirmed?: boolean }; fields?: Array<{ key: string }> } | undefined;
    ok(dc?.b?.["name"] === "canvas-saved-quote" && dc?.b?.["html"] === artH && sp?.calculation?.confirmed && sp.fields?.some((f) => f.key === "customer"), "drawer");
    ok(has(`sha256:${sha}`, undefined, sf) && sb("[data-canvas-sample-preview] iframe", sf), "iframe");

    await render(E(Templates)); await showV(HQ); await click("Load to revise v1");
    ok(host.querySelector("[data-revision-base='1']"), "rev"); await click("Create revision draft");
    ok(call(`${T}revise`)?.b?.["base"] === 1, "revise");

    await render(E(WriteGate.Provider, { value: "Board is read-only." }, E(Templates)));
    ok(has("Board is read-only.", RO) && btn(CDV)?.disabled === true, "WriteGate");
    await render(E(Templates, { viewer: { operator: false, hosted: true } }));
    ok(has("Only the workspace owner", RO) && btn(CDV)?.disabled && btn(SV)?.disabled, "hosted");
    await showV(HQ);
    ok(btn("Approve v1", false)?.disabled && !btn("Preview sample", false)?.disabled && btn(RR)?.disabled, "perms");

    calls.length = 0; win.history.replaceState({}, "", "/");
    const cr = ref("art-concrete", "Northwind Quote.html");
    await render(E("div", null, E(ArtifactCard, { entry: { id: "e1", kind: "artifact", created: 1, payload: cr } }), E(CanvasDrawer)));
    await feed(cr, "t2", false); await tap(q('[data-artifact-save-template="art-concrete"]'));
    const af = q<HTMLFormElement>(SAT);
    ok(af.querySelector("[data-auto-param-note]"), "auto-param");
    await save(`input[placeholder="${NW}"]`, NW, af);
    ok(String(call(`${T}create`)?.b?.["html"] ?? "").includes("{{ customer }}"), "inject");
    await tap(q("[data-open-in-templates]", af)); ok(win.location.pathname === "/settings/templates", "nav");
    await render(E(Templates));
    ok(q<HTMLInputElement>(NI).value === "northwind-quote" && areas()[1]?.value.includes("{{ customer }}"), "prefill");

    await mountCanvas(); await click(ST);
    await A(async () => { ps.pausePrivateReads(); }); await flush();
    ok(!host.querySelector(SAT), "pause");

    await render(null, false); await A(async () => ps.resetPrivateSession()); ps.allowPrivateReads();
    def("navigator", { ...win.navigator, language: "zh-HK", languages: ["zh-HK"] });
    await render(E(Templates)); ok(has("文件範本") && has("範本目錄"), "zh-TW");
    await mountCanvas(); await click("儲存為範本");
    ok(has("我確認以上計算規則", SAT) && has("儲存為草稿 v1", SAT), "zh-TW Canvas");

    await render(null, false); await A(async () => ps.resetPrivateSession());
    ok(live.size === 0, "blobs");
    console.log("templatesHtml checks passed");
  } finally {
    globalThis.fetch = origF; URL.createObjectURL = origC; URL.revokeObjectURL = origR;
    await A(async () => ps.resetPrivateSession());
  }
}
void main();
