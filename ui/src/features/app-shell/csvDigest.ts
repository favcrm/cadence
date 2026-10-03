/**
 * CAD-1016: the browser-side reproduction of the daemon's
 * `material_digest` — `sha256:` of `cadence-app-material-v1\n` followed
 * by the canonical JSON of the value. `csv-confirm` binds the confirmed
 * decision set via this digest; the assistant import recomputes it from
 * its own `decisions` param and the daemon refuses on a mismatch. The
 * serialization must match exactly: object keys sorted (B-Tree order),
 * numbers as serde JSON renders them, no whitespace.
 *
 * The digest binds `[{row, action, expected_revision?}]` — the SAME
 * normalized array the store's `csv_decisions_digest` digests. We build
 * the canonical string first, then hash it with a compact synchronous
 * SHA-256 (happy-dom and the older engines this UI targets do not always
 * expose `crypto.subtle`, and async hashing complicates the confirm path
 * for no benefit — the input is bounded to the 500-row decision list).
 */

/* Canonical JSON: sort object keys (BTreeMap order), arrays keep order,
 * numbers render as serde_json prints them (integers bare, no quotes),
 * strings/bool/null as JSON. No whitespace. */
function canonical(value: unknown): string {
  if (value === null || value === undefined) return "null";
  if (typeof value === "boolean") return value ? "true" : "false";
  if (typeof value === "number") {
    if (!Number.isFinite(value)) return "null";
    // serde_json prints integer-valued numbers without a trailing `.0`.
    return Number.isInteger(value) ? String(value) : String(value);
  }
  if (typeof value === "string") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  const obj = value as Record<string, unknown>;
  const keys = Object.keys(obj).sort();
  return `{${keys.map((k) => `${JSON.stringify(k)}:${canonical(obj[k])}`).join(",")}}`;
}

/** The canonical JSON string for a CSV decisions array
 *  `[{row, action, expected_revision?}]` — row/action always present,
 *  expected_revision only when a positive integer. */
export function decisionsCanonicalJson(
  decisions: ReadonlyArray<{ row: number; action: string; expected_revision?: number }>,
): string {
  const normalized = decisions.map((item) => {
    const entry: Record<string, unknown> = { row: item.row, action: item.action };
    if (typeof item.expected_revision === "number" && item.expected_revision > 0) {
      entry.expected_revision = item.expected_revision;
    }
    return entry;
  });
  return canonical(normalized);
}

/* --- Compact synchronous SHA-256 (public-domain style), bytes→hex --- */

const K = [
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

function sha256Hex(bytes: Uint8Array): string {
  const len = bytes.length;
  const bitLen = len * 8;
  // Padding: 1 bit, zeros, 64-bit length → multiple of 64 bytes.
  const withLen = (((len + 8) >> 6) + 1) << 6;
  const msg = new Uint8Array(withLen);
  msg.set(bytes);
  msg[len] = 0x80;
  const view = new DataView(msg.buffer);
  view.setUint32(withLen - 4, bitLen >>> 0, false);
  view.setUint32(withLen - 8, Math.floor(bitLen / 0x100000000), false);

  let h0 = 0x6a09e667, h1 = 0xbb67ae85, h2 = 0x3c6ef372, h3 = 0xa54ff53a;
  let h4 = 0x510e527f, h5 = 0x9b05688c, h6 = 0x1f83d9ab, h7 = 0x5be0cd19;
  const w = new Uint32Array(64);

  for (let block = 0; block < withLen; block += 64) {
    for (let t = 0; t < 16; t++) w[t] = view.getUint32(block + t * 4, false);
    for (let t = 16; t < 64; t++) {
      const s0 = (rotr(w[t - 15], 7) ^ rotr(w[t - 15], 18) ^ (w[t - 15] >>> 3)) >>> 0;
      const s1 = (rotr(w[t - 2], 17) ^ rotr(w[t - 2], 19) ^ (w[t - 2] >>> 10)) >>> 0;
      w[t] = (w[t - 16] + s0 + w[t - 7] + s1) >>> 0;
    }
    let a = h0, b = h1, c = h2, d = h3, e = h4, f = h5, g = h6, h = h7;
    for (let t = 0; t < 64; t++) {
      const S1 = (rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25)) >>> 0;
      const ch = ((e & f) ^ (~e & g)) >>> 0;
      const t1 = (h + S1 + ch + K[t] + w[t]) >>> 0;
      const S0 = (rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22)) >>> 0;
      const maj = ((a & b) ^ (a & c) ^ (b & c)) >>> 0;
      const t2 = (S0 + maj) >>> 0;
      h = g; g = f; f = e; e = (d + t1) >>> 0;
      d = c; c = b; b = a; a = (t1 + t2) >>> 0;
    }
    h0 = (h0 + a) >>> 0; h1 = (h1 + b) >>> 0; h2 = (h2 + c) >>> 0; h3 = (h3 + d) >>> 0;
    h4 = (h4 + e) >>> 0; h5 = (h5 + f) >>> 0; h6 = (h6 + g) >>> 0; h7 = (h7 + h) >>> 0;
  }
  const out = new Uint8Array(32);
  const o = new DataView(out.buffer);
  [h0, h1, h2, h3, h4, h5, h6, h7].forEach((word, i) => o.setUint32(i * 4, word >>> 0, false));
  return Array.from(out).map((b) => b.toString(16).padStart(2, "0")).join("");
}

function rotr(x: number, n: number): number {
  return ((x >>> n) | (x << (32 - n))) >>> 0;
}

/** `sha256:` hex of the UTF-8 bytes of `text`. */
export function sha256Text(text: string): string {
  const bytes = new TextEncoder().encode(text);
  return `sha256:${sha256Hex(bytes)}`;
}

/** `sha256:` hex of the UTF-8 bytes of `cadence-app-material-v1\n` +
 *  canonical JSON — the exact form the daemon's `material_digest` emits. */
export function materialDigest(value: unknown): string {
  return sha256Text(`cadence-app-material-v1\n${canonical(value)}`);
}

/** The `decisions_digest` `csv-confirm` expects: the material digest of
 *  the canonical `[{row, action, expected_revision?}]` array. Takes the
 *  UI's `CsvRowDecision` shape (camelCase `expectedRevision`) and emits
 *  the snake_case wire form the store digests. */
export function csvDecisionsDigest(
  decisions: ReadonlyArray<{ row: number; action: string; expectedRevision?: number }>,
): string {
  return materialDigest(toWireDecisions(decisions));
}

/** `CsvRowDecision[]` → the wire `[{row, action, expected_revision?}]`
 *  the daemon's `csv_decisions_digest` / assistant-import `decisions`
 *  param consumes — `expected_revision` only when a positive integer. */
export function toWireDecisions(
  decisions: ReadonlyArray<{ row: number; action: string; expectedRevision?: number }>,
): { row: number; action: string; expected_revision?: number }[] {
  return decisions.map((item) => {
    const entry: { row: number; action: string; expected_revision?: number } = {
      row: item.row,
      action: item.action,
    };
    if (typeof item.expectedRevision === "number" && item.expectedRevision > 0) {
      entry.expected_revision = item.expectedRevision;
    }
    return entry;
  });
}
