export const MAX_SOURCE_PIXELS = 32_000_000;
const MAX_OUTPUT_PIXELS = 4_000_000;
const MAX_EDGE = 8_192;

export function photoDimensions(width: number, height: number): { width: number; height: number } | null {
  if (!Number.isSafeInteger(width) || !Number.isSafeInteger(height) || width < 1 || height < 1 || width * height > MAX_SOURCE_PIXELS) return null;
  const scale = Math.min(1, Math.sqrt(MAX_OUTPUT_PIXELS / (width * height)), MAX_EDGE / Math.max(width, height));
  return { width: Math.max(1, Math.floor(width * scale)), height: Math.max(1, Math.floor(height * scale)) };
}
