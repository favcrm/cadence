export {};
/** The operator sees retained bytes only after the run, receipt, and digest agree. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/app-installations/install-a" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "SVGElement", "navigator", "MutationObserver", "Event", "MouseEvent", "location", "history", "sessionStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function(this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const { ImageReceiptPanel } = require("../src/features/workspace-apps/ImageReceiptPanel") as typeof import("../src/features/workspace-apps/ImageReceiptPanel");
const Buffer = require("buffer").Buffer as { from(value: string, encoding?: string): { length: number; toString(encoding: string): string } };
const png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/lqUAAAAASUVORK5CYII=";
const bytes = Buffer.from(png, "base64");
const digest = `sha256:${require("crypto").createHash("sha256").update(bytes).digest("hex")}`;
const run = {
  id: "image-run", install_id: "install-a", context_id: null, state: "succeeded", snapshot_digest: "snapshot", approved_digest: "snapshot",
  snapshot: { workflow: { title: "Caption and image", steps: [] }, inputs: { source: "Selected source" }, owner_pm: "pm-a", capabilities: { image: { id: "binding-image", revision: 1, digest: "binding-digest" } }, source: { receipt_id: "source-receipt", post: { id: "post-a", caption: "Original" } }, assignments: {} },
  steps: [], artifacts: [], reviews: [{ step_id: "review", artifact_digest: "text-digest", reviewer: "reviewer-a", decision: "approve", rationale: "Checked", asset_receipt_id: "image-receipt", asset_digest: digest }],
};
const receipt = { id: "image-receipt", run_id: run.id, slot: "image", digest: "receipt-digest", binding_digest: "binding-digest", asset: { media_type: "image/png", digest, size: bytes.length }, result: { schema: 1, kind: "media.generated.image", provider: "agenticos_external", model: "image-01", aspect_ratio: "1:1", asset_sha256: digest, asset_media_type: "image/png", source_receipt_id: "source-receipt", source_post_id: "post-a" } };
const json = (value: unknown) => new Response(JSON.stringify(value), { status: 200, headers: { "Content-Type": "application/json" } });
let returnedReceipt: typeof receipt = receipt;
let returnedBytes = png;
const reads: string[] = [];
globalThis.fetch = async input => {
  const path = String(input); reads.push(path);
  if (path === "/api/app-runs/image-run/capability-results") return json({ results: [returnedReceipt] });
  if (path === "/api/app-capability-results/image-receipt/asset") return json({ receipt_id: receipt.id, media_type: "image/png", digest, size: bytes.length, base64: returnedBytes });
  throw new Error(`Unexpected read ${path}`);
};
function assert(value: unknown, reason: string): asserts value { if (!value) throw new Error(reason); }
const host = document.createElement("div"); document.body.append(host);
const root = createRoot(host);
const verified: Array<{ runId: string; receiptId: string; digest: string } | null> = [];
const onVerified = (value: { runId: string; receiptId: string; digest: string } | null) => verified.push(value);
const flush = () => React.act(async () => { await new Promise(resolve => setTimeout(resolve, 0)); });
async function render() {
  await React.act(async () => root.render(React.createElement(ImageReceiptPanel, { run, onDenied: () => { throw new Error("Unexpected denial"); }, onVerified })));
  await flush();
}
async function main() {
  await render();
  assert(host.querySelector("img")?.getAttribute("src") === `data:image/png;base64,${png}`, "Board renders the retained asset, not a temporary provider URL");
  assert(verified.at(-1)?.receiptId === receipt.id && verified.at(-1)?.digest === digest, "Exact retained digest can unlock release");
  assert(host.textContent?.includes("Independent review pinned these bytes"), "Board explains the reviewer pin");
  await React.act(async () => root.render(React.createElement(ImageReceiptPanel, { run: { ...run, state: "failed" }, onDenied: () => {}, onVerified })));
  await flush();
  assert(host.querySelector("img")?.getAttribute("src") === `data:image/png;base64,${png}`, "A later run failure must not hide retained image bytes");
  assert(host.textContent?.includes("This run did not complete"), "A retained image in a failed run is clearly not releasable");
  returnedBytes = png.replace("AC1HAw", "BC1HAw");
  await React.act(async () => root.render(React.createElement(ImageReceiptPanel, { run: { ...run, state: "running" }, onDenied: () => {}, onVerified })));
  await flush();
  assert(!host.querySelector("img") && verified.at(-1) === null, "Tampered retained bytes revoke browser verification");
  assert(host.textContent?.includes("retained image digest changed"), "Operator sees why release is blocked");
  returnedBytes = png;
  returnedReceipt = { ...receipt, run_id: "forged-run" };
  await render();
  assert(!host.querySelector("img") && verified.at(-1) === null, "Receipt from another run cannot unlock release");
  assert(reads.includes("/api/app-runs/image-run/capability-results"), "The panel reads a stored result from the selected run");
  await React.act(async () => root.unmount());
  console.log("social image retained receipt flow passed");
}
void main();
