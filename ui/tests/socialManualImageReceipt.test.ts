export {};
declare function require(name: string): any;
const { Window } = require("happy-dom");
const { createHash } = require("node:crypto");
const { Buffer } = require("node:buffer");
const win = new Window({ url: "http://localhost/app-installations/install-a" });
Object.defineProperty(globalThis, "window", { value: win, configurable: true });
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function(this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  return originalRequire.apply(this, arguments);
};
const { retainedImage } = require("../src/features/workspace-apps/ImageReceiptPanel") as typeof import("../src/features/workspace-apps/ImageReceiptPanel");
type WorkspaceRun = import("../src/features/workspace-apps/workspaceApps").WorkspaceRun;
type ImageReceipt = import("../src/features/workspace-apps/workspaceApps").ImageReceipt;
const bytes = Uint8Array.from([1, 2, 3, 4]);
const digest = `sha256:${createHash("sha256").update(bytes).digest("hex")}`;
const run = { id: "run-manual", snapshot: { source: null, capabilities: { image: { digest: "binding-image" } } } } as unknown as WorkspaceRun;
const receipt: ImageReceipt = {
  id: "receipt-image", run_id: "run-manual", slot: "image", digest: "sha256:receipt", binding_digest: "binding-image",
  asset: { media_type: "image/png", digest, size: bytes.length },
  result: { schema: 1, kind: "media.generated.image", provider: "agenticos_external", model: "image-01", aspect_ratio: "1:1", asset_sha256: digest, asset_media_type: "image/png", source_receipt_id: null, source_post_id: null },
};
globalThis.fetch = async input => {
  if (String(input) !== "/api/app-capability-results/receipt-image/asset") throw new Error(`Unexpected read ${input}`);
  return new Response(JSON.stringify({ receipt_id: receipt.id, media_type: "image/png", digest, size: bytes.length, base64: Buffer.from(bytes).toString("base64") }), { status: 200, headers: { "Content-Type": "application/json" } });
};
async function main() {
  const image = await retainedImage(receipt, run, new AbortController().signal);
  if (image !== `data:image/png;base64,${Buffer.from(bytes).toString("base64")}`) throw new Error("Manual facts image bytes were not verified");
  const forged = { ...receipt, result: { ...receipt.result, source_receipt_id: "unexpected-source" } };
  await retainedImage(forged, run, new AbortController().signal).then(
    () => { throw new Error("Forged source linkage was accepted"); },
    (error: Error) => { if (!error.message.includes("no verified retained image receipt")) throw error; },
  );
  console.log("manual image receipt verification passed");
}
void main();
