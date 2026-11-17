/** Bound the frame before passing a locally converted JPEG to the browser. */
export function jpegDimensions(bytes: Uint8Array): { width: number; height: number } {
  if (bytes[0] !== 0xff || bytes[1] !== 0xd8) throw new Error("Invalid converted image");
  let at = 2;
  let dimensions: { width: number; height: number } | undefined;
  let sawScan = false;
  while (at + 1 < bytes.length) {
    if (bytes[at++] !== 0xff) throw new Error("Invalid JPEG marker");
    while (at < bytes.length && bytes[at] === 0xff) at++;
    const marker = bytes[at++];
    if (marker === 0xd9) {
      if (!dimensions || !sawScan || at !== bytes.length) throw new Error("Invalid JPEG end");
      return dimensions;
    }
    if (marker === 0x00 || marker === 0xd8) throw new Error("Invalid JPEG marker");
    if (marker === 0x01 || (marker >= 0xd0 && marker <= 0xd7)) continue;
    if (at + 2 > bytes.length) throw new Error("Truncated JPEG segment");
    const length = (bytes[at] << 8) | bytes[at + 1];
    if (length < 2 || at + length > bytes.length) throw new Error("Invalid JPEG segment");
    if (marker >= 0xc0 && marker <= 0xcf && marker !== 0xc4 && marker !== 0xc8 && marker !== 0xcc) {
      if (length < 8 || dimensions) throw new Error("Invalid JPEG dimensions");
      dimensions = { height: (bytes[at + 3] << 8) | bytes[at + 4], width: (bytes[at + 5] << 8) | bytes[at + 6] };
    }
    at += length;
    if (marker === 0xda) {
      if (!dimensions) throw new Error("JPEG scan precedes its frame");
      sawScan = true;
      let hasData = false;
      while (at + 1 < bytes.length) {
        if (bytes[at] !== 0xff) { hasData = true; at++; continue; }
        let next = at + 1;
        while (next < bytes.length && bytes[next] === 0xff) next++;
        if (next >= bytes.length) throw new Error("Truncated JPEG scan");
        const code = bytes[next];
        if (code === 0x00 || (code >= 0xd0 && code <= 0xd7)) {
          if (code === 0x00) hasData = true;
          at = next + 1;
          continue;
        }
        break;
      }
      if (!hasData) throw new Error("Empty JPEG scan");
    }
  }
  throw new Error("JPEG has no end marker");
}
