import type { HeifAssets } from "./heifAssets";
import { isHeif } from "./photoKind";

const EXPORT_GUIDANCE = "Could not convert this HEIC/HEIF photo. Export it as JPEG or PNG and attach again.";
const LARGE_GUIDANCE = "This photo is too large to convert safely. Export it as JPEG or PNG and attach again.";

function assertActive(signal: AbortSignal): void {
  if (signal.aborted) throw new DOMException("Photo conversion canceled", "AbortError");
}

function decodeInWorker(file: File, signal: AbortSignal): Promise<{ blob: Blob; assets: HeifAssets }> {
  return new Promise((resolve, reject) => {
    assertActive(signal);
    let worker: Worker | undefined;
    let settled = false;
    const timeout = setTimeout(() => finish(() => reject(new Error(EXPORT_GUIDANCE))), 60_000);
    const abort = () => finish(() => reject(new DOMException("Photo conversion canceled", "AbortError")));
    const finish = (done: () => void) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      signal.removeEventListener("abort", abort);
      worker?.terminate();
      done();
    };
    signal.addEventListener("abort", abort, { once: true });
    void Promise.all([import("./heifWorker?worker"), import("./heifAssets")]).then(([workerModule, assetModule]) => {
      if (settled) return;
      const assets = assetModule.default;
      try {
        worker = new workerModule.default();
        worker.onmessage = (event: MessageEvent<{ blob?: Blob; tooLarge?: boolean }>) => {
          const { blob, tooLarge } = event.data ?? {};
          finish(() => {
            if (blob instanceof Blob && blob.type === "image/jpeg" && blob.size > 0) resolve({ blob, assets });
            else reject(new Error(tooLarge ? LARGE_GUIDANCE : EXPORT_GUIDANCE));
          });
        };
        worker.onerror = (event) => {
          event.preventDefault();
          finish(() => reject(new Error(EXPORT_GUIDANCE)));
        };
        worker.postMessage({ file, assets });
      } catch {
        finish(() => reject(new Error(EXPORT_GUIDANCE)));
      }
    }).catch(() => finish(() => reject(new Error(EXPORT_GUIDANCE))));
  });
}

/** Convert HEIF locally before upload. Ordinary files retain their original bytes. */
export async function preparePhoto(file: File, signal: AbortSignal, onConverting: () => void, allowConversion: boolean): Promise<{ file: File; converted: boolean; decoderLicenseUrl?: string; decoderNoticesUrl?: string }> {
  const heif = await isHeif(file, signal);
  if (!heif) {
    if (/\.(?:heic|heif|hif|avif)$/i.test(file.name) || /^image\/(?:heic|heif|avif)(?:-sequence)?$/i.test(file.type)) throw new Error(EXPORT_GUIDANCE);
    return { file, converted: false };
  }
  if (!allowConversion) throw new Error(EXPORT_GUIDANCE);
  onConverting();
  assertActive(signal);
  const { blob, assets } = await decodeInWorker(file, signal);
  assertActive(signal);
  const stem = file.name.replace(/\.[^.]+$/, "") || "photo";
  return { file: new File([blob], `${stem}.jpg`, { type: "image/jpeg", lastModified: file.lastModified }), converted: true, decoderLicenseUrl: assets.licenseUrl, decoderNoticesUrl: assets.noticesUrl };
}
