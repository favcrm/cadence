// One worker owns one decoder. Cap its WebAssembly linear memory at 256 MiB,
// independent of container dimensions or the decoder's much larger defaults.
export const HEIF_WASM_MAX_PAGES = 4_096;

export function boundHeifWasmMemory(bytes: Uint8Array, maxPages = HEIF_WASM_MAX_PAGES): void {
  if (!Number.isSafeInteger(maxPages) || maxPages < 1 || bytes.length < 8 ||
      bytes[0] !== 0 || bytes[1] !== 0x61 || bytes[2] !== 0x73 || bytes[3] !== 0x6d ||
      bytes[4] !== 1 || bytes[5] !== 0 || bytes[6] !== 0 || bytes[7] !== 0) {
    throw new Error("Invalid decoder module");
  }
  let at = 8;
  const readU32 = (end: number) => {
    const start = at;
    let value = 0;
    for (let i = 0; i < 5; i++) {
      if (at >= end) throw new Error("Truncated decoder module");
      const byte = bytes[at++];
      if (i === 4 && (byte & 0xf0) !== 0) throw new Error("Invalid decoder integer");
      value += (byte & 0x7f) * 2 ** (7 * i);
      if ((byte & 0x80) === 0) return { value, start, length: at - start };
    }
    throw new Error("Invalid decoder integer");
  };
  let found = false;
  while (at < bytes.length) {
    const id = bytes[at++];
    const size = readU32(bytes.length).value;
    const end = at + size;
    if (end > bytes.length) throw new Error("Truncated decoder section");
    if (id === 5) {
      if (found || readU32(end).value !== 1 || readU32(end).value !== 1) throw new Error("Unsupported decoder memory");
      const minimum = readU32(end).value;
      const maximum = readU32(end);
      if (at !== end || minimum > maxPages || maximum.value < minimum) throw new Error("Unsupported decoder memory limit");
      let limit = Math.min(maximum.value, maxPages);
      for (let i = 0; i < maximum.length; i++) {
        bytes[maximum.start + i] = (limit % 128) | (i + 1 < maximum.length ? 0x80 : 0);
        limit = Math.floor(limit / 128);
      }
      if (limit !== 0) throw new Error("Decoder memory limit cannot be encoded");
      found = true;
    }
    at = end;
  }
  if (!found) throw new Error("Decoder has no bounded memory");
}
