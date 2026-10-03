import { ASSET_BYTES_MAX, ASSET_PX_MAX } from "./screenProtocol";

/**
 * CAD-1123 HP1 image channel: the board downscales a retained image it
 * already holds (the operator-only asset route) to at most 512 px on its
 * longer side and at most 96 KiB as a `data:` URL, so the frame — which
 * has no network — can show it. Decoding happens in the board's own
 * browser context; nothing here reaches the daemon or a third party.
 */
const MEDIA = new Set(["image/png", "image/jpeg", "image/webp"]);

function blobToDataUrl(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => typeof reader.result === "string" ? resolve(reader.result) : reject(new Error("unreadable"));
    reader.onerror = () => reject(new Error("unreadable"));
    reader.readAsDataURL(blob);
  });
}

export async function downscaleToDataUrl(base64: string, mediaType: string): Promise<string | null> {
  if (!MEDIA.has(mediaType) || typeof createImageBitmap !== "function") return null;
  const bytes = Uint8Array.from(atob(base64), c => c.charCodeAt(0));
  const bitmap = await createImageBitmap(new Blob([bytes], { type: mediaType }));
  try {
    for (const side of [ASSET_PX_MAX, 384, 256]) {
      const scale = Math.min(1, side / Math.max(bitmap.width, bitmap.height));
      const canvas = document.createElement("canvas");
      canvas.width = Math.max(1, Math.round(bitmap.width * scale));
      canvas.height = Math.max(1, Math.round(bitmap.height * scale));
      const context = canvas.getContext("2d");
      if (!context) return null;
      context.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
      for (const quality of [0.85, 0.7, 0.55]) {
        const blob = await new Promise<Blob | null>(resolve => canvas.toBlob(resolve, "image/jpeg", quality));
        if (!blob) return null;
        const url = await blobToDataUrl(blob);
        if (url.length <= ASSET_BYTES_MAX) return url;
      }
    }
    return null;
  } finally {
    bitmap.close();
  }
}
