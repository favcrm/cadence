export {};
/** A selected retained post exposes an image plan only with an exact binding quote — the quote still resolves (CAD-1129) but no price text is shown. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/app-installations/install-a" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLDialogElement", "HTMLInputElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function(this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const { NewPost } = require("../src/features/workspace-apps/NewPost") as typeof import("../src/features/workspace-apps/NewPost");
const quote = { slot: "image", binding_digest: "binding-image", quote_digest: "quote-image", quote: { schema: 1, currency: "USD", unit_price_micros: 31500, units: 1, total_price_micros: 31500, price_revision: "revision-image" } };
let currentQuote = quote;
let reads = 0;
globalThis.fetch = async input => {
  if (String(input) !== "/api/app-installations/install-a/bindings/image/quote") throw new Error(`Unexpected read ${input}`);
  reads++;
  return new Response(JSON.stringify(currentQuote), { status: 200, headers: { "Content-Type": "application/json" } });
};
const host = document.createElement("div"); document.body.append(host);
const root = createRoot(host);
const created: Array<import("../src/features/workspace-apps/NewPost").NewPostValues> = [];
const props = {
  workflows: [{ value: "instagram", label: "Instagram caption" }, { value: "image-instagram", label: "Instagram caption and image" }, { value: "image-manual", label: "Caption and image from pasted facts" }],
  workflowInputs: {
    instagram: [{ name: "content_prompt", default: "App content default" }],
    "image-instagram": [{ name: "content_prompt", default: "App content default" }, { name: "image_prompt", default: "App image default" }],
    "image-manual": [{ name: "content_prompt", default: "App content default" }, { name: "image_prompt", default: "App image default" }],
  },
  contextDefaults: { content_prompt: "Fav Limited content", image_prompt: "Fav Limited image" }, contextLabel: "Fav Limited",
  workers: [{ value: "writer-a", label: "writer-a", group: "pm-a" }, { value: "reviewer-a", label: "reviewer-a", group: "pm-a" }],
  managers: [{ value: "pm-a", label: "pm-a" }], busy: false, error: null, onClose: () => {}, onCreate: (value: import("../src/features/workspace-apps/NewPost").NewPostValues) => created.push(value),
  selectedSource: { receiptId: "source-receipt", postId: "post-a", handle: "juicysuite_crm", caption: "Original selected facts", permalink: "https://www.instagram.com/p/ABC123/" },
  installId: "install-a", imageSupported: true, imageBindingDigest: "binding-image", onDenied: () => { throw new Error("Unexpected denial"); },
};
function assert(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }
const flush = () => React.act(async () => { await new Promise(resolve => setTimeout(resolve, 0)); });
async function click(element: Element | null | undefined) {
  assert(element, "Click target exists");
  await React.act(async () => { element.dispatchEvent(new MouseEvent("click", { bubbles: true })); }); await flush();
}
async function choose(id: string, label: string) {
  await click(host.querySelector(`#${id}`));
  const option = Array.from(document.querySelectorAll('[role="option"]')).find(value => value.textContent?.includes(label));
  assert(option?.closest("dialog") === host.querySelector("dialog"), "Options stay inside the active dialog");
  await click(option);
}
async function fill(selector: string, value: string) {
  const element = host.querySelector(selector) as HTMLInputElement;
  assert(element, "Field exists");
  await React.act(async () => { Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(element, value); element.dispatchEvent(new Event("input", { bubbles: true })); });
}
async function fillPrompt(selector: string, value: string) {
  const element = host.querySelector(selector) as HTMLTextAreaElement;
  assert(element, "Prompt field exists");
  await React.act(async () => { Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(element, value); element.dispatchEvent(new Event("input", { bubbles: true })); });
}
async function main() {
  await React.act(async () => root.render(React.createElement(NewPost, props))); await flush();
  assert(reads === 1 && host.textContent?.includes("image option is ready"), "Operator sees the image option resolve before choosing a plan");
  // CAD-1129: no cost or price text anywhere in the board.
  assert(!host.textContent?.includes("USD") && !host.textContent?.includes("price") && !host.textContent?.includes("billed"), "the board shows no price or cost text");
  await fill("#wa-post-title", "Customer follow-up");
  await choose("wa-workflow", "Instagram caption and image");
  assert((host.querySelector("#wa-post-source") as HTMLTextAreaElement).readOnly, "Selected source facts stay immutable");
  assert((host.querySelector("#wa-content-prompt") as HTMLTextAreaElement).value === "Fav Limited content", "Saved content prompt is prefilled");
  assert((host.querySelector("#wa-image-prompt") as HTMLTextAreaElement).value === "Fav Limited image", "Saved image prompt is prefilled");
  await fillPrompt("#wa-content-prompt", "One post content override");
  await fillPrompt("#wa-image-prompt", "One post image override");
  assert(host.textContent?.includes("this post’s override"), "Effective prompt preview identifies overrides before creation");
  await fillPrompt("#wa-image-prompt", "");
  assert(host.textContent?.includes("Fav Limited image"), "Clearing the image prompt restores its saved default in the preview");
  await choose("wa-owner", "pm-a"); await choose("wa-writer", "writer-a"); await choose("wa-reviewer", "reviewer-a");
  await click(Array.from(host.querySelectorAll("button")).find(value => value.textContent?.trim() === "Create plan"));
  assert(created.length === 1 && created[0].workflow === "image-instagram" && created[0].sourceReceiptId === "source-receipt" && created[0].selectedPostId === "post-a", "Image plan pins the retained source and selected post");
  assert(created[0].contentPrompt === "One post content override" && created[0].imagePrompt === undefined, "Only nonempty per-post overrides are submitted");
  await React.act(async () => root.unmount());
  const manualRoot = createRoot(host);
  await React.act(async () => manualRoot.render(React.createElement(NewPost, { ...props, selectedSource: null }))); await flush();
  await fill("#wa-post-title", "Manual facts post");
  await fillPrompt("#wa-post-source", "Fav Limited helps teams manage customer relationships.");
  await choose("wa-workflow", "Caption and image from pasted facts");
  assert(!(host.querySelector("#wa-image-prompt") as HTMLTextAreaElement).disabled, "Manual facts can use the quoted image workflow");
  await choose("wa-owner", "pm-a"); await choose("wa-writer", "writer-a"); await choose("wa-reviewer", "reviewer-a");
  await click(Array.from(host.querySelectorAll("button")).find(value => value.textContent?.trim() === "Create plan"));
  assert(created[1]?.workflow === "image-manual" && !created[1].sourceReceiptId, "Manual image run uses pasted facts without inventing a retained source receipt");
  await React.act(async () => manualRoot.unmount());
  const badHost = document.createElement("div"); document.body.append(badHost);
  currentQuote = { ...quote, binding_digest: "stale-binding" };
  const badRoot = createRoot(badHost);
  await React.act(async () => badRoot.render(React.createElement(NewPost, props))); await flush();
  assert(badHost.textContent?.includes("image connection changed or is unavailable"), "Stale binding quote is explained");
  await React.act(async () => badRoot.unmount());
  console.log("social image priced plan flow passed");
}
void main();
