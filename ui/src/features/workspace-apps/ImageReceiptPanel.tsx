import { useEffect, useState } from "react";
import { ApiError } from "../../lib/api";
import { workspaceApps, type ImageReceipt, type WorkspaceRun } from "./workspaceApps";

const imageTypes = new Set(["image/png", "image/jpeg", "image/webp"]);
const assetLimit = 2 * 1024 * 1024;

async function retainedImage(receipt: ImageReceipt, run: WorkspaceRun, signal: AbortSignal) {
  const meta = receipt.asset;
  if (receipt.run_id !== run.id || receipt.slot !== "image"
    || receipt.binding_digest !== run.snapshot.capabilities?.image?.digest
    || receipt.result?.schema !== 1 || receipt.result.kind !== "media.generated.image"
    || receipt.result.model !== "image-01" || receipt.result.aspect_ratio !== "1:1"
    || receipt.result.source_receipt_id !== run.snapshot.source?.receipt_id
    || receipt.result.source_post_id !== run.snapshot.source?.post.id
    || !meta || !imageTypes.has(meta.media_type) || meta.media_type !== receipt.result.asset_media_type
    || meta.digest !== receipt.result.asset_sha256 || !/^sha256:[a-f0-9]{64}$/.test(meta.digest)
    || !Number.isSafeInteger(meta.size) || meta.size < 1 || meta.size > assetLimit) {
    throw new Error("This run has no verified retained image receipt.");
  }
  const asset = await workspaceApps.capabilityAsset(receipt.id, signal);
  if (asset.receipt_id !== receipt.id || asset.digest !== meta.digest
    || asset.media_type !== meta.media_type || asset.size !== meta.size
    || typeof asset.base64 !== "string" || asset.base64.length > Math.ceil(assetLimit / 3) * 4 + 4) {
    throw new Error("The retained image changed after its receipt was recorded.");
  }
  const binary = window.atob(asset.base64);
  if (binary.length !== meta.size) throw new Error("The retained image byte count changed.");
  const bytes = Uint8Array.from(binary, character => character.charCodeAt(0));
  const hash = Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", bytes.buffer)))
    .map(value => value.toString(16).padStart(2, "0")).join("");
  if (`sha256:${hash}` !== meta.digest) throw new Error("The retained image digest changed.");
  return `data:${meta.media_type};base64,${asset.base64}`;
}

export type VerifiedImage = { runId: string; receiptId: string; digest: string };

export function ImageReceiptPanel({ run, onDenied, onVerified }: { run: WorkspaceRun; onDenied: () => void; onVerified: (image: VerifiedImage | null) => void }) {
  const [receipt, setReceipt] = useState<ImageReceipt | null>(null);
  const [image, setImage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  useEffect(() => {
    setReceipt(null); setImage(null); setError(null);
    onVerified(null);
    if (!["running", "succeeded"].includes(run.state)) return;
    const controller = new AbortController();
    setLoading(true);
    void workspaceApps.imageResults(run.id, controller.signal).then(async results => {
      if (controller.signal.aborted) return;
      const matches = results.filter(value => value.slot === "image");
      if (matches.length === 0) return;
      if (matches.length !== 1) throw new Error("This run has ambiguous image receipts.");
      const source = await retainedImage(matches[0], run, controller.signal);
      if (!controller.signal.aborted) {
        setReceipt(matches[0]); setImage(source);
        onVerified({ runId: run.id, receiptId: matches[0].id, digest: matches[0].asset!.digest });
      }
    }).catch(reason => {
      if (controller.signal.aborted) return;
      if (reason instanceof ApiError && [401, 403].includes(reason.status)) { onDenied(); return; }
      setError(reason instanceof Error ? reason.message : "Could not inspect retained image bytes.");
    }).finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [run.id, run.state, run.snapshot.capabilities?.image?.digest, onDenied, onVerified]);
  const approved = receipt && run.reviews.some(review => review.decision === "approve"
    && review.asset_receipt_id === receipt.id && review.asset_digest === receipt.asset?.digest);
  return <section className="wa-stack" aria-label="Retained generated image">
    <h3>Generated image</h3>
    {loading && <p className="wa-muted" role="status">Checking retained image bytes…</p>}
    {error && <p className="wa-alert" data-tone="fail" role="alert">{error}</p>}
    {!loading && !receipt && !error && <p className="wa-muted">No image asset has been retained yet. A provider URL or worker description cannot stand in for one.</p>}
    {image && receipt?.asset && <figure className="wa-retained-image">
      <img src={image} alt="Generated draft awaiting review against the selected source" width="512" height="512" />
      <figcaption>{receipt.asset.media_type} · {receipt.asset.size.toLocaleString()} bytes · {approved ? "Independent review pinned these bytes" : "Awaiting independent review"}</figcaption>
    </figure>}
    {receipt?.asset && <p className="wa-digest">Receipt {receipt.id}<br />{receipt.asset.digest}</p>}
  </section>;
}
