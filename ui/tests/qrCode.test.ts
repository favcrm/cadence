export {};
import { encodeQrMatrix, qrSvgPath, type QrMatrix } from "../src/features/settings/qrCode";

function assert(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(msg);
}

function assertFinderAt(m: QrMatrix, topRow: number, leftCol: number, label: string): void {
  for (let r = 0; r < 7; r += 1) {
    for (let c = 0; c < 7; c += 1) {
      const border = r === 0 || r === 6 || c === 0 || c === 6;
      const center = r >= 2 && r <= 4 && c >= 2 && c <= 4;
      const expected = border || center;
      assert(m.modules[topRow + r]?.[leftCol + c] === expected, `${label} finder at (${r},${c})`);
    }
  }
}

function readFormatSyndrome(m: QrMatrix): { copyA: number; copyB: number; syndromeA: number; syndromeB: number } {
  const s = m.size;
  let a = 0;
  for (const [r, c] of [
    [8, 0], [8, 1], [8, 2], [8, 3], [8, 4], [8, 5], [8, 7], [8, 8],
    [7, 8], [5, 8], [4, 8], [3, 8], [2, 8], [1, 8], [0, 8],
  ] as const) {
    a = (a << 1) | (m.modules[r]![c]! ? 1 : 0);
  }
  let b = 0;
  for (let r = s - 1; r >= s - 7; r -= 1) b = (b << 1) | (m.modules[r]![8]! ? 1 : 0);
  for (let c = s - 8; c < s; c += 1) b = (b << 1) | (m.modules[8]![c]! ? 1 : 0);

  const bchRem = (raw: number) => {
    let val = raw ^ 0x5412;
    for (let shift = 4; shift >= 0; shift -= 1) {
      if ((val >>> (shift + 10)) & 1) val ^= 0x537 << shift;
    }
    return val;
  };
  return { copyA: a, copyB: b, syndromeA: bchRem(a), syndromeB: bchRem(b) };
}

function readVersionSyndrome(m: QrMatrix): {
  version: number;
  copyA: number;
  copyB: number;
  syndromeA: number;
  syndromeB: number;
} {
  const version = (m.size - 17) / 4;
  let copyA = 0;
  let copyB = 0;
  for (let i = 0; i < 18; i += 1) {
    const a = m.size - 11 + (i % 3);
    const b = Math.floor(i / 3);
    if (m.modules[a]![b]) copyA |= 1 << i;
    if (m.modules[b]![a]) copyB |= 1 << i;
  }
  const bchRem = (raw: number) => {
    let val = raw;
    for (let shift = 5; shift >= 0; shift -= 1) {
      if ((val >>> (shift + 12)) & 1) val ^= 0x1f25 << shift;
    }
    return val;
  };
  return {
    version,
    copyA,
    copyB,
    syndromeA: bchRem(copyA),
    syndromeB: bchRem(copyB),
  };
}

// 1. Standard TOTP provisioning URI -> crisp square QR matrix + SVG path
{
  const uri = "otpauth://totp/AgenticOS:owner%40example.com?secret=JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP&issuer=AgenticOS&digits=6&period=30";
  const m = encodeQrMatrix(uri);
  assert(m !== null, "totpURI encodes cleanly");
  assert(m.size >= 21 && (m.size - 17) % 4 === 0, "valid QR size");
  assertFinderAt(m, 0, 0, "TL");
  assertFinderAt(m, 0, m.size - 7, "TR");
  assertFinderAt(m, m.size - 7, 0, "BL");
  // Dark module at (size - 8, 8)
  assert(m.modules[m.size - 8]?.[8] === true, "mandatory dark module set");
  // Timing strips alternate on row 6 and col 6
  for (let i = 8; i < m.size - 8; i += 1) {
    assert(m.modules[6]![i] === (i % 2 === 0), `row timing at ${i}`);
    assert(m.modules[i]![6] === (i % 2 === 0), `col timing at ${i}`);
  }
  const fmt = readFormatSyndrome(m);
  assert(fmt.copyA === fmt.copyB, "format copies agree");
  assert(fmt.syndromeA === 0 && fmt.syndromeB === 0, "format BCH check passes");

  const d = qrSvgPath(m);
  assert(d.startsWith("M0,0h1v1h-1z"), "SVG path starts at top-left finder corner");
  const count = d.match(/h1v1h-1z/g)?.length ?? 0;
  const dark = m.modules.flat().filter(Boolean).length;
  assert(count === dark && dark > 0, "SVG path covers exact count of dark modules");
}

// 2. Versions 1..20 capacities, WhatsApp pairing lengths, and version BCH checks
{
  const small = encodeQrMatrix("hi");
  assert(small !== null && small.size === 21, "v1 size 21");

  const v10M = encodeQrMatrix("x".repeat(213));
  assert(v10M !== null && v10M.size === 57, "v10 EC M fits 213 bytes");
  assert(readFormatSyndrome(v10M).syndromeA === 0, "v10 EC M format BCH valid");
  assert(readVersionSyndrome(v10M).syndromeA === 0, "v10 version BCH valid");

  // Typical WhatsApp multi-device pairing payload (~220 bytes) upgrades cleanly into V11+
  const waTypical = encodeQrMatrix(`2@${"A".repeat(44)},${"B".repeat(44)},${"C".repeat(44)},${"D".repeat(83)}`);
  assert(waTypical !== null && waTypical.size >= 61, "220-byte WhatsApp payload encodes cleanly");
  assertFinderAt(waTypical, 0, 0, "waTypical TL");
  assertFinderAt(waTypical, 0, waTypical.size - 7, "waTypical TR");
  assertFinderAt(waTypical, waTypical.size - 7, 0, "waTypical BL");
  assert(readFormatSyndrome(waTypical).syndromeA === 0, "waTypical format BCH valid");
  const waVer = readVersionSyndrome(waTypical);
  assert(waVer.copyA === waVer.copyB && waVer.syndromeA === 0, "waTypical version BCH valid");

  // Longer payloads across V12..V20 (300, 600, 666 EC M max, 858 EC L max)
  for (const len of [300, 600, 666, 858]) {
    const matrix = encodeQrMatrix("w".repeat(len));
    assert(matrix !== null, `${len}-byte payload encodes cleanly`);
    assert(matrix.size >= 21 && (matrix.size - 17) % 4 === 0, `${len}-byte size aligned`);
    if (len === 666 || len === 858) {
      assert(matrix.size === 97, `${len}-byte payload hits Version 20 size 97`);
    }
    const fmt = readFormatSyndrome(matrix);
    assert(fmt.copyA === fmt.copyB && fmt.syndromeA === 0 && fmt.syndromeB === 0, `${len}-byte format BCH valid`);
    const ver = readVersionSyndrome(matrix);
    assert(ver.copyA === ver.copyB && ver.syndromeA === 0 && ver.syndromeB === 0, `${len}-byte version BCH valid`);
    assert((ver.copyA >>> 12) === ver.version, `${len}-byte decoded version matches size`);
  }

  assert(encodeQrMatrix("x".repeat(859)) === null, "over Version 20 capacity (859 bytes) returns null");
  assert(encodeQrMatrix("安".repeat(287)) === null, "multibyte UTF-8 over 858 bytes returns null");
}

console.log("qrCode tests passed");
