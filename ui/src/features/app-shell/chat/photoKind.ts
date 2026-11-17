const brandCode = (brand: string) =>
  ((brand.charCodeAt(0) << 24) | (brand.charCodeAt(1) << 16) | (brand.charCodeAt(2) << 8) | brand.charCodeAt(3)) >>> 0;
// Image-specific and structural HEIF brands, including AVIF. Generic ISO BMFF
// movie brands are deliberately absent so ordinary MP4 videos keep working.
const HEIF_BRANDS = new Set([
  "heic", "heix", "heim", "heis", "hevc", "hevx", "hevm", "hevs",
  "mif1", "mif2", "mif3", "msf1", "miaf", "1pic", "avif", "avis",
  "vvic", "vvis", "evbi", "evmi", "evbs", "evms", "jpeg", "jpgs",
  "j2ki", "j2is", "avci", "avcs",
].map(brandCode));

/** Identify HEIF by its ftyp bytes, not by the browser's declared MIME. */
export async function isHeif(file: File, signal: AbortSignal): Promise<boolean> {
  const head = new Uint8Array(await file.slice(0, 24).arrayBuffer());
  signal.throwIfAborted();
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
  if (size === 0) size = file.size;
  if (head.length < header + 4) return false;
  const brand = view.getUint32(header);
  // An image-labelled BMFF with an unknown brand is ambiguous; do not send its
  // original bytes just because the file was renamed or uses a newer brand.
  if (file.type.toLowerCase().startsWith("image/") || /\.(?:jpe?g|png|webp|gif|heic|heif|hif|avif)$/i.test(file.name)) return true;
  if (HEIF_BRANDS.has(brand)) return true;
  if (size < header + 8 || size > file.size || (size - header - 8) % 4 !== 0) return false;
  for (let start = header + 8; start < size; start += 64 * 1024) {
    const compatible = new DataView(await file.slice(start, Math.min(size, start + 64 * 1024)).arrayBuffer());
    signal.throwIfAborted();
    for (let offset = 0; offset < compatible.byteLength; offset += 4) {
      if (HEIF_BRANDS.has(compatible.getUint32(offset))) return true;
    }
  }
  return false;
}
