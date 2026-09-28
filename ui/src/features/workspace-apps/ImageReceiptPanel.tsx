import { useEffect, useRef, useState } from "react";
import { ApiError } from "../../lib/api";
import { workspaceApps, type ImageReceipt, type WorkspaceRun } from "./workspaceApps";

const imageTypes = new Set(["image/png", "image/jpeg", "image/webp"]);
const assetLimit = 2 * 1024 * 1024;

export async function retainedImage(receipt: ImageReceipt, run: WorkspaceRun, signal: AbortSignal) {
  const meta = receipt.asset;
  if (receipt.run_id !== run.id || receipt.slot !== "image"
    || receipt.binding_digest !== run.snapshot.capabilities?.image?.digest
    || receipt.result?.schema !== 1 || receipt.result.kind !== "media.generated.image"
    || receipt.result.model !== "image-01" || receipt.result.aspect_ratio !== "1:1"
    || receipt.result.source_receipt_id !== (run.snapshot.source?.receipt_id ?? null)
    || receipt.result.source_post_id !== (run.snapshot.source?.post.id ?? null)
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

export function imageSubject(run: WorkspaceRun) {
  return JSON.stringify([run.id, run.snapshot.capabilities?.image?.digest, run.snapshot.source?.receipt_id, run.snapshot.source?.post.id]);
}

export type VerifiedImage = { runId: string; receiptId: string; digest: string; subject: string };

export function ImageReceiptPanel({ run, onDenied, onVerified }: { run: WorkspaceRun; onDenied: () => void; onVerified: (image: VerifiedImage | null) => void }) {
  const [receipt, setReceipt] = useState<ImageReceipt | null>(null);
  const [image, setImage] = useState<string | null>(null);
  const [verifiedSubject, setVerifiedSubject] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const subject = imageSubject(run);
  const priorSubject = useRef<string | null>(null);
  useEffect(() => {
    // A status-only update must not hide bytes already verified for this run
    // while the repeated read is pending. A different receipt subject cannot
    // inherit those bytes, and every new read revokes release until it verifies.
    if (priorSubject.current !== subject) { setReceipt(null); setImage(null); setVerifiedSubject(null); }
    priorSubject.current = subject;
    setError(null);
    onVerified(null);
    const controller = new AbortController();
    setLoading(true);
    void workspaceApps.imageResults(run.id, controller.signal).then(async results => {
      if (controller.signal.aborted) return;
      const matches = results.filter(value => value.slot === "image");
      if (matches.length === 0) { setReceipt(null); setImage(null); return; }
      if (matches.length !== 1) throw new Error("This run has ambiguous image receipts.");
      const source = await retainedImage(matches[0], run, controller.signal);
      if (!controller.signal.aborted) {
        setReceipt(matches[0]); setImage(source); setVerifiedSubject(subject);
        onVerified({ runId: run.id, receiptId: matches[0].id, digest: matches[0].asset!.digest, subject });
      }
    }).catch(reason => {
      if (controller.signal.aborted) return;
      setReceipt(null); setImage(null); setVerifiedSubject(null);
      if (reason instanceof ApiError && [401, 403].includes(reason.status)) { onDenied(); return; }
      setError(reason instanceof Error ? reason.message : "Could not inspect retained image bytes.");
    }).finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [subject, run.state, onDenied, onVerified]);
  const visibleReceipt = verifiedSubject === subject ? receipt : null;
  const visibleImage = verifiedSubject === subject ? image : null;
  const approved = visibleReceipt && run.reviews.some(review => review.decision === "approve"
    && review.asset_receipt_id === visibleReceipt.id && review.asset_digest === visibleReceipt.asset?.digest);
  return <section className="wa-stack" aria-label="Retained generated image">
    <h3>Generated image</h3>
    {run.state === "failed" && <p className="wa-alert" data-tone="fail" role="status">This run did not complete. Any retained image remains available for inspection, but Local release is blocked.</p>}
    {loading && <p className="wa-muted" role="status">Checking retained image bytes…</p>}
    {error && <p className="wa-alert" data-tone="fail" role="alert">{error}</p>}
    {!loading && !visibleReceipt && !error && <p className="wa-muted">No image asset has been retained yet. A provider URL or worker description cannot stand in for one.</p>}
    {visibleImage && visibleReceipt?.asset && <figure className="wa-retained-image">
      <img src={visibleImage} alt={run.snapshot.source ? "Generated draft awaiting review against the selected source" : "Generated draft awaiting review against the pasted facts"} width="512" height="512" />
      <figcaption>{visibleReceipt.asset.media_type} · {visibleReceipt.asset.size.toLocaleString()} bytes · {approved ? "Independent review pinned these bytes" : "Awaiting independent review"}</figcaption>
    </figure>}
    {visibleReceipt?.asset && <p className="wa-digest">Receipt {visibleReceipt.id}<br />{visibleReceipt.asset.digest}</p>}
  </section>;
}
