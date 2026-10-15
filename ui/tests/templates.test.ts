import type * as HappyDomModule from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDomModule from "react-dom/client";
import type * as TemplatesModule from "../src/features/templates/Templates";
import type * as PrivateSessionModule from "../src/lib/privateSession";
/** Real Templates downloads across the admitted Activity lifecycle. */
declare function require(name: string): unknown;
const { Window } = require("happy-dom") as typeof HappyDomModule;
const win = new Window({ url: "http://localhost/settings/templates" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "navigator", "Event", "MouseEvent", "location", "history", "sessionStorage"] as const)
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener"] as const)
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module") as { prototype: { require: (id: string) => unknown } }, originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  return originalRequire.call(this, id);
};
const React = require("react") as typeof ReactModule;
const { createRoot } = require("react-dom/client") as typeof ReactDomModule;
const Templates = (require("../src/features/templates/Templates") as typeof TemplatesModule).default;
const { allowPrivateReads, pausePrivateReads, resetPrivateSession, privateSessionGeneration } = require("../src/lib/privateSession") as typeof PrivateSessionModule;
const process = require("process") as { argv: string[]; exitCode: number };
const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const assert: (condition: unknown, message: string) => asserts condition = (condition, message) => {
  if (!condition) throw new Error(message);
};
const flush = async () => { await React.act(async () => {}); };
const originalFetch = globalThis.fetch, originalCreate = URL.createObjectURL, originalRevoke = URL.revokeObjectURL;
const live = new Map<string, Blob>();
let created = 0;
URL.createObjectURL = (blob) => {
  assert(blob instanceof Blob, "Downloads own actual document blobs");
  const url = `blob:http://localhost/template-${++created}`;
  live.set(url, blob);
  return url;
};
URL.revokeObjectURL = (url) => { live.delete(url); };
const sample = { sample: true, template: "acme", version: 1, status: "approved", context: { customer: "SAMPLE customer" }, docx_base64: btoa("sample docx bytes"), pdf_base64: btoa("sample pdf bytes") };
const exported = { id: "draft-1", docx_base64: btoa("private rendered docx bytes"), pdf_base64: btoa("private rendered pdf bytes") };
const reference = { file: "reference.pdf", provenance: { source: { filename: "reference.pdf", sha256: "reference-digest", preserved_as: "reference.pdf" }, evidence: { format: "pdf", pages: [{ page: 1, text: "Private reference" }] }, reconstruction: { file: "reference.docx", kind: "docx", limitations: ["Text only"] } }, reconstruction_base64: btoa("reconstructed docx bytes") };
let hold: string | null = null;
let release: (() => void) | null = null;
let bodyStarted = false;
const calls: string[] = [];
globalThis.fetch = async (input) => {
  const path = String(input);
  calls.push(path);
  let data: unknown;
  if (path.startsWith("/api/templates/show")) data = { name: "acme", versions: [{ name: "acme", version: 1, status: "approved", based_on: null, kind: "quotation", content_sha256: "content", source_sha256: "source", approved_source: "operator", created: 1 }] };
  else if (path.startsWith("/api/templates/sample")) data = sample;
  else if (path === "/api/documents/create") data = { id: "draft-1", template: "acme", version: 1, template_sha256: "content", kind: "quotation", state: "draft", input: {}, context: { customer: "Private customer" }, created: 1 };
  else if (path.startsWith("/api/documents/export")) data = exported;
  else if (path === "/api/templates/intake") data = reference;
  else throw new Error(`Unexpected template request ${path}`);
  const response = new Response(JSON.stringify(data), { headers: { "Content-Type": "application/json" } });
  if (hold && path.startsWith(hold)) response.json = async () => {
    bodyStarted = true;
    await new Promise<void>((resolve) => { release = resolve; });
    return data;
  };
  return response;
};
const render = async (mode: "visible" | "hidden" = "visible") => {
  await React.act(async () => root.render(React.createElement(React.Activity, { mode, key: privateSessionGeneration(), children: React.createElement(Templates) })));
  await flush();
};
const click = async (label: string) => {
  const button = Array.from(host.querySelectorAll("button")).find((element) => element.textContent?.trim() === label);
  assert(button && !button.disabled, `Enabled template action: ${label}`);
  await React.act(async () => button.dispatchEvent(new MouseEvent("click", { bubbles: true })));
  await flush();
};
const enter = async (selector: string, value: string) => {
  const input = host.querySelector<HTMLInputElement | HTMLTextAreaElement>(selector);
  assert(input, `Template input exists: ${selector}`);
  await React.act(async () => {
    const prototype = input.tagName === "TEXTAREA" ? win.HTMLTextAreaElement.prototype : win.HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(prototype, "value")!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await flush();
};
const download = async (label: string, bytes: string) => {
  const link = Array.from(host.querySelectorAll("a[download]")).find((element) => element.textContent?.trim() === label);
  assert(link, `Download remains available: ${label}`);
  const url = link.getAttribute("href");
  assert(url && live.has(url), `${label} points to a live, non-revoked object URL`);
  assert(await live.get(url)!.text() === bytes, `${label} downloads the expected document bytes`);
  return url;
};
const prepare = async () => {
  await React.act(async () => root.render(null));
  assert(live.size === 0, "Unmount releases every document handle");
  hold = null;
  release = null;
  bodyStarted = false;
  allowPrivateReads();
  await render();
  await enter('input[placeholder="quotation-acme"]', "acme");
  await click("Show versions");
};
const createDraft = async () => {
  await enter('section[aria-labelledby="templates-versions"] textarea', '{"customer":"Private draft"}');
  await click("Create draft from v1");
};
const pause = async () => {
  await React.act(async () => pausePrivateReads());
  await render("hidden");
  assert(live.size === 0, "Auth pause releases every document handle while Activity is hidden");
};
const resume = async () => { allowPrivateReads(); await render(); };
const scenario = process.argv[2] ?? "all";
async function run() {
  try {
    if (scenario === "all" || scenario === "sample") {
      await prepare();
      await click("Preview sample v1");
      const before = await download("Download SAMPLE DOCX", "sample docx bytes");
      await download("Download SAMPLE PDF", "sample pdf bytes");
      const requestCount = calls.length;
      await pause();
      await resume();
      assert(await download("Download SAMPLE DOCX", "sample docx bytes") !== before, "Same-session resume recreates the sample URL, not a revoked string");
      const pdfUrl = await download("Download SAMPLE PDF", "sample pdf bytes");
      assert(host.querySelector("iframe")?.getAttribute("src") === pdfUrl, "Resumed sandboxed sample frame uses the live PDF handle");
      assert(host.querySelector("iframe")?.hasAttribute("sandbox"), "Sample PDF stays sandboxed");
      assert(calls.length === requestCount, "Same-session resume reuses accepted sample bytes without another private request");
      assert(host.querySelector<HTMLInputElement>('input[placeholder="quotation-acme"]')?.value === "acme", "Outage preserves the same-session template draft");
    }
    if (scenario === "all" || scenario === "rendered") {
      await prepare();
      await createDraft();
      await click("Render DOCX and PDF preview");
      const before = await download("Download DOCX", "private rendered docx bytes");
      await download("Download PDF", "private rendered pdf bytes");
      const count = created;
      await enter('input[placeholder="quotation-acme"]', "acme-edited");
      assert(await download("Download DOCX", "private rendered docx bytes") === before && created === count, "Unrelated renders neither replace nor leak document download handles");
      const requests = calls.length;
      await pause();
      await resume();
      assert(await download("Download DOCX", "private rendered docx bytes") !== before, "Resume reconstructs rendered document downloads");
      await download("Download PDF", "private rendered pdf bytes");
      assert(calls.length === requests, "Rendered results survive same-session recovery without repeating export");
      assert(host.querySelector("textarea")?.value === "", "Unrelated spec draft remains unchanged");
      assert(host.querySelector<HTMLTextAreaElement>('section[aria-labelledby="templates-versions"] textarea')?.value.includes("Private draft"), "Document draft input survives Activity recovery");
    }
    if (scenario === "all" || scenario === "reconstruction") {
      await prepare();
      const input = host.querySelector('input[type="file"]');
      assert(input, "Reference upload is available");
      const file = new win.File(["reference bytes"], "reference.pdf");
      file.arrayBuffer = async () => new TextEncoder().encode("reference bytes").buffer;
      Object.defineProperty(input, "files", { value: [file], configurable: true });
      await React.act(async () => input.dispatchEvent(new Event("change", { bubbles: true })));
      await click("Read reference");
      const before = await download("Download reconstruction DOCX", "reconstructed docx bytes");
      const count = created;
      await enter('input[placeholder="quotation-acme"]', "acme-reference");
      assert(await download("Download reconstruction DOCX", "reconstructed docx bytes") === before && created === count, "Reference reconstruction has one owned handle, not a render allocation");
      await pause();
      await resume();
      assert(await download("Download reconstruction DOCX", "reconstructed docx bytes") !== before, "Reference reconstruction download survives same-session recovery");
    }
    for (const kind of ["sample", "export"] as const) {
      if (scenario !== "all" && scenario !== `late-${kind}`) continue;
      await prepare();
      if (kind === "export") await createDraft();
      hold = kind === "sample" ? "/api/templates/sample" : "/api/documents/export";
      await click(kind === "sample" ? "Preview sample v1" : "Render DOCX and PDF preview");
      assert(bodyStarted && release, "Response headers passed admission while private body decoding remains pending");
      const count = created;
      await pause();
      await resume();
      await React.act(async () => release!());
      await flush();
      assert(created === count && live.size === 0, `Late ${kind} body cannot allocate private handles after Activity pause/resume`);
      assert(!host.querySelector("a[download]"), `Late ${kind} body cannot publish stale download results after resume`);
      assert(!host.textContent?.includes("Rendered"), "Suspended operation does not publish a stale success message");
      hold = null;
      await click(kind === "sample" ? "Preview sample v1" : "Render DOCX and PDF preview");
      await download(kind === "sample" ? "Download SAMPLE DOCX" : "Download DOCX", kind === "sample" ? "sample docx bytes" : "private rendered docx bytes");
      await React.act(async () => resetPrivateSession());
      assert(live.size === 0, "Confirmed session reset synchronously revokes every handle");
      allowPrivateReads();
      await render();
      assert(!host.querySelector("a[download], [data-draft], [data-sample]"), "New session generation inherits no previous sample, rendered result or draft");
      assert(host.querySelector<HTMLInputElement>('input[placeholder="quotation-acme"]')?.value === "", "New generation discards old private template inputs");
    }
    await React.act(async () => root.render(null));
    assert(live.size === 0, "Final unmount leaves no document handles");
    console.log(`templates ${scenario}: passed`);
  } finally {
    await React.act(async () => root.unmount());
    globalThis.fetch = originalFetch;
    URL.createObjectURL = originalCreate;
    URL.revokeObjectURL = originalRevoke;
    loader.prototype.require = originalRequire;
    host.remove();
    await win.happyDOM.close();
  }
}
run().catch((error) => { console.error(error); process.exitCode = 1; });
