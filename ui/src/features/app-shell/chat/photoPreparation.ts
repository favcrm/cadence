import type { HeifAssets } from "./heifAssets";

const HEIF_BRANDS = new Set([0x68656963, 0x68656978, 0x68657663, 0x68657678, 0x68656973, 0x6865766d, 0x68657673]);
const HEIF_GENERIC_BRANDS = new Set([0x6d696631, 0x6d736631]);
const EXPORT_GUIDANCE = "Could not convert this HEIC/HEIF photo. Export it as JPEG or PNG and attach again.";
const LARGE_GUIDANCE = "This photo is too large to convert safely. Export it as JPEG or PNG and attach again.";

function assertActive(signal: AbortSignal): void {
  if (signal.aborted) throw new DOMException("Photo conversion canceled", "AbortError");
}

/** Identify HEIF by its ftyp bytes, not by the browser's declared MIME. */
async function isHeif(file: File, signal: AbortSignal): Promise<boolean> {
  const head = new Uint8Array(await file.slice(0, 24).arrayBuffer());
  assertActive(signal);
  if (head.length < 12 || head[4] !== 0x66 || head[5] !== 0x74 || head[6] !== 0x79 || head[7] !== 0x70) return false;
  const view = new DataView(head.buffer, head.byteOffset, head.byteLength);
  let size = view.getUint32(0);
  let header = 8;
  if (size === 1) {
    if (head.length < 24) return false;
    const extended = view.getBigUint64(8);
    if (extended > BigInt(file.size)) size = file.size + 1;
    else size = Number(extended);
    header = 16;
  }
  if (head.length < header + 4) return false;
  const brand = view.getUint32(header);
  if (HEIF_BRANDS.has(brand)) return true;
  if (size < header + 8 || size > file.size || (size - header - 8) % 4 !== 0) return HEIF_GENERIC_BRANDS.has(brand);
  let avif = brand === 0x61766966 || brand === 0x61766973;
  for (let start = header + 8; start < size; start += 64 * 1024) {
    const compatible = new DataView(await file.slice(start, Math.min(size, start + 64 * 1024)).arrayBuffer());
    assertActive(signal);
    for (let offset = 0; offset < compatible.byteLength; offset += 4) {
      const code = compatible.getUint32(offset);
      if (HEIF_BRANDS.has(code)) return true;
      if (code === 0x61766966 || code === 0x61766973) avif = true;
    }
  }
  return HEIF_GENERIC_BRANDS.has(brand) && !avif;
}

async function decodeInWorker(file: File, signal: AbortSignal): Promise<{ blob: Blob; assets: HeifAssets }> {
  let HeifWorker: new () => Worker;
  let assets: HeifAssets;
  try {
    [{ default: HeifWorker }, { default: assets }] = await Promise.all([
      import("./heifWorker?worker"),
      import("./heifAssets"),
    ]);
  } catch {
    assertActive(signal);
    throw new Error(EXPORT_GUIDANCE);
  }
  assertActive(signal);
  return new Promise((resolve, reject) => {
    assertActive(signal);
    let worker: Worker;
    try {
      worker = new HeifWorker();
    } catch {
      reject(new Error(EXPORT_GUIDANCE));
      return;
    }
    let settled = false;
    const timeout = setTimeout(() => finish(() => reject(new Error(EXPORT_GUIDANCE))), 60_000);
    const abort = () => finish(() => reject(new DOMException("Photo conversion canceled", "AbortError")));
    const finish = (done: () => void) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      signal.removeEventListener("abort", abort);
      worker.terminate();
      done();
    };
    signal.addEventListener("abort", abort, { once: true });
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
    try {
      worker.postMessage({ file, assets });
    } catch {
      finish(() => reject(new Error(EXPORT_GUIDANCE)));
    }
  });
}

/** Convert HEIF locally before upload. Ordinary files retain their original bytes. */
export async function preparePhoto(file: File, signal: AbortSignal, onConverting: () => void, allowConversion: boolean): Promise<{ file: File; converted: boolean; decoderLicenseUrl?: string; decoderNoticesUrl?: string }> {
  const heif = await isHeif(file, signal);
  if (!heif) {
    if (/\.(?:heic|heif)$/i.test(file.name)) throw new Error(EXPORT_GUIDANCE);
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
