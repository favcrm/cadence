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
const { ImageReceiptPanel, imageSubject } = require("../src/features/workspace-apps/ImageReceiptPanel") as typeof import("../src/features/workspace-apps/ImageReceiptPanel");
const Buffer = require("buffer").Buffer as { from(value: string, encoding?: string): { length: number; toString(encoding: string): string } };
const png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/lqUAAAAASUVORK5CYII=";
const bytes = Buffer.from(png, "base64");
const digest = `sha256:${require("crypto").createHash("sha256").update(bytes).digest("hex")}`;
const run = {
  id: "image-run", install_id: "install-a", context_id: null, state: "succeeded", snapshot_digest: "snapshot", approved_digest: "snapshot",
  snapshot: { workflow: { title: "Caption and image", steps: [] }, inputs: { source: "Selected source" }, owner_pm: "pm-a", capabilities: { image: { id: "binding-image", revision: 1, digest: "binding-digest" } }, source: { receipt_id: "source-receipt", post: { id: "post-a", caption: "Original" } }, assignments: {} },
  steps: [], artifacts: [], reviews: [{ step_id: "review", artifact_digest: "text-digest", reviewer: "reviewer-a", decision: "approve", rationale: "Checked", asset_receipt_id: "image-receipt", asset_digest: digest }],
};
const receipt = { id: "image-receipt", run_id: run.id, slot: "image", digest: "receipt-digest", binding_digest: "binding-digest", asset: { media_type: "image/png", digest, size: bytes.length }, result: { schema: 1, kind: "media.generated.image", provider: "agenticos_external", model: "openai/gpt-image-2.5", aspect_ratio: "1:1", job_id: "med_job1", charge: { currency: "USD", scale: 6, amount: "0.031500" }, price_version: "v1", quoted_micros: 31500, repeated: false, asset_sha256: digest, asset_media_type: "image/png", source_receipt_id: "source-receipt", source_post_id: "post-a" } };
const json = (value: unknown) => new Response(JSON.stringify(value), { status: 200, headers: { "Content-Type": "application/json" } });
let returnedReceipt: typeof receipt = receipt;
let returnedBytes = png;
let holdResults: Promise<void> | null = null;
let denyResults = false;
const reads: string[] = [];
globalThis.fetch = async input => {
  const path = String(input); reads.push(path);
  if (["/api/app-runs/image-run/capability-results", "/api/app-runs/other-image-run/capability-results"].includes(path)) {
    if (holdResults) await holdResults;
    if (denyResults) return new Response(JSON.stringify({ error: "Access revoked" }), { status: 403 });
    return json({ results: [returnedReceipt] });
  }
  if (path === "/api/app-capability-results/image-receipt/asset") return json({ receipt_id: receipt.id, media_type: "image/png", digest, size: bytes.length, base64: returnedBytes });
  throw new Error(`Unexpected read ${path}`);
};
function assert(value: unknown, reason: string): asserts value { if (!value) throw new Error(reason); }
const host = document.createElement("div"); document.body.append(host);
const root = createRoot(host);
const verified: Array<{ runId: string; receiptId: string; digest: string; subject: string } | null> = [];
const onVerified = (value: { runId: string; receiptId: string; digest: string; subject: string } | null) => verified.push(value);
let denials = 0;
const onDenied = () => { denials++; };
const layoutImages: Array<string | null> = [];
function observedPanel({ nextRun }: { nextRun: typeof run }) {
  React.useLayoutEffect(() => { layoutImages.push(host.querySelector("img")?.getAttribute("src") ?? null); }, [nextRun]);
  return React.createElement(ImageReceiptPanel, { run: nextRun, onDenied, onVerified });
}
async function settleUntil(condition: () => boolean, reason: string) {
  const deadline = Date.now() + 3000;
  while (!condition()) {
    if (Date.now() >= deadline) throw new Error(reason);
    await React.act(async () => { await new Promise(resolve => setTimeout(resolve, 1)); });
  }
}
async function render(nextRun = run) {
  await React.act(async () => root.render(React.createElement(observedPanel, { nextRun })));
}
async function main() {
  await render();
  await settleUntil(() => verified.at(-1)?.receiptId === receipt.id && !!host.querySelector("img"), "Retained image verification did not complete");
  assert(host.querySelector("img")?.getAttribute("src") === `data:image/png;base64,${png}`, "Board renders the retained asset, not a temporary provider URL");
  assert(verified.at(-1)?.receiptId === receipt.id && verified.at(-1)?.digest === digest, "Exact retained digest can unlock release");
  assert(verified.at(-1)?.subject === imageSubject(run), "Release pin binds the selected run, image binding and source");
  assert(host.textContent?.includes("Independent review pinned these bytes"), "Board explains the reviewer pin");
  let releaseResults!: () => void;
  holdResults = new Promise(resolve => { releaseResults = resolve; });
  await render({ ...run, state: "failed" });
  assert(host.querySelector("img")?.getAttribute("src") === `data:image/png;base64,${png}`, "A later run failure must not hide retained image bytes while a repeated read is pending");
  releaseResults(); holdResults = null;
  await settleUntil(() => verified.at(-1)?.receiptId === receipt.id && !host.textContent?.includes("Checking retained image bytes"), "Retained image revalidation did not complete");
  assert(host.querySelector("img")?.getAttribute("src") === `data:image/png;base64,${png}`, "A later run failure must not hide retained image bytes");
  assert(host.textContent?.includes("This run did not complete"), "A retained image in a failed run is clearly not releasable");
  const changedSubjects = [
    { name: "run", value: { ...run, id: "other-image-run" } },
    { name: "binding", value: { ...run, snapshot: { ...run.snapshot, capabilities: { image: { ...run.snapshot.capabilities.image, digest: "different-binding" } } } } },
    { name: "source", value: { ...run, snapshot: { ...run.snapshot, source: { ...run.snapshot.source, receipt_id: "different-source" } } } },
  ];
  for (const changed of changedSubjects) {
    let releaseChangedResults!: () => void;
    holdResults = new Promise(resolve => { releaseChangedResults = resolve; });
    assert(verified.at(-1)?.subject !== imageSubject(changed.value), `A changed ${changed.name} cannot reuse a previous release pin`);
    await render(changed.value);
    assert(layoutImages.at(-1) === null, `A changed ${changed.name} must not paint the previous subject's verified image before effects run`);
    assert(!host.querySelector("img") && !host.textContent?.includes(`Receipt ${receipt.id}`), `A changed ${changed.name} must not display the old receipt during a pending read`);
    releaseChangedResults(); holdResults = null;
    await settleUntil(() => !!host.textContent?.includes("no verified retained image receipt"), `Changed ${changed.name} was not rejected`);
    await render(run);
    await settleUntil(() => verified.at(-1)?.receiptId === receipt.id && !!host.querySelector("img"), `Original ${changed.name} did not reverify`);
  }
  denyResults = true;
  await render({ ...run, state: "running" });
  await settleUntil(() => denials === 1, "Revoked access was not reported");
  assert(!host.querySelector("img") && verified.at(-1) === null, "Revoked access cannot retain a previously verified image");
  denyResults = false;
  await render({ ...run, state: "failed" });
  await settleUntil(() => verified.at(-1)?.receiptId === receipt.id && !!host.querySelector("img"), "Retained image did not recover after access was restored");
  returnedBytes = png.replace("AC1HAw", "BC1HAw");
  await render({ ...run, state: "running" });
  await settleUntil(() => !!host.textContent?.includes("retained image digest changed"), "Tampered asset verification did not complete");
  assert(!host.querySelector("img") && verified.at(-1) === null, "Tampered retained bytes revoke browser verification");
  assert(host.textContent?.includes("retained image digest changed"), "Operator sees why release is blocked");
  returnedBytes = png;
  returnedReceipt = { ...receipt, run_id: "forged-run" };
  await render(run);
  await settleUntil(() => !!host.textContent?.includes("no verified retained image receipt"), "Forged receipt verification did not complete");
  assert(!host.querySelector("img") && verified.at(-1) === null, "Receipt from another run cannot unlock release");
  assert(reads.includes("/api/app-runs/image-run/capability-results"), "The panel reads a stored result from the selected run");
  await React.act(async () => root.unmount());
  console.log("social image retained receipt flow passed");
}
void main();
