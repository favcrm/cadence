/**
 * Pure TypeScript ISO/IEC 18004 byte-mode QR encoder for Versions 1–20.
 * Zero external runtime dependencies: prefers EC level M (up to 666 UTF-8
 * bytes) and falls back to EC level L (up to 858 UTF-8 bytes) for longer URIs.
 */

export type QrMatrix = {
  size: number;
  modules: boolean[][];
};

type BlockSpec = readonly [blocks: number, dataCodewords: number];
type VersionEcSpec = readonly [
  totalDataCodewords: number,
  ecPerBlock: number,
  groups: readonly BlockSpec[],
];

// ISO/IEC 18004 Table 7 & Table 9 for Versions 1..20 (EC level M & L)
const SPEC_M: readonly VersionEcSpec[] = [
  [16, 10, [[1, 16]]],
  [28, 16, [[1, 28]]],
  [44, 26, [[1, 44]]],
  [64, 18, [[2, 32]]],
  [86, 24, [[2, 43]]],
  [108, 16, [[4, 27]]],
  [124, 18, [[4, 31]]],
  [154, 22, [[2, 38], [2, 39]]],
  [182, 22, [[3, 36], [2, 37]]],
  [216, 26, [[4, 43], [1, 44]]],
  [254, 30, [[1, 50], [4, 51]]],
  [290, 22, [[6, 36], [2, 37]]],
  [334, 22, [[8, 37], [1, 38]]],
  [365, 24, [[4, 40], [5, 41]]],
  [415, 24, [[5, 41], [5, 42]]],
  [453, 28, [[7, 45], [3, 46]]],
  [507, 28, [[10, 46], [1, 47]]],
  [563, 26, [[9, 43], [4, 44]]],
  [627, 26, [[3, 44], [11, 45]]],
  [669, 26, [[3, 41], [13, 42]]],
];

const SPEC_L: readonly VersionEcSpec[] = [
  [19, 7, [[1, 19]]],
  [34, 10, [[1, 34]]],
  [55, 15, [[1, 55]]],
  [80, 20, [[1, 80]]],
  [108, 26, [[1, 108]]],
  [136, 18, [[2, 68]]],
  [156, 20, [[2, 78]]],
  [194, 24, [[2, 97]]],
  [232, 30, [[2, 116]]],
  [274, 18, [[2, 68], [2, 69]]],
  [324, 20, [[4, 81]]],
  [370, 24, [[2, 92], [2, 93]]],
  [428, 26, [[4, 107]]],
  [461, 30, [[3, 115], [1, 116]]],
  [523, 22, [[5, 87], [1, 88]]],
  [589, 24, [[5, 98], [1, 99]]],
  [647, 28, [[1, 107], [5, 108]]],
  [721, 30, [[5, 120], [1, 121]]],
  [795, 28, [[3, 113], [4, 114]]],
  [861, 28, [[3, 107], [5, 108]]],
];

const ALIGNMENT_POSITIONS: readonly (readonly number[])[] = [
  [],
  [6, 18],
  [6, 22],
  [6, 26],
  [6, 30],
  [6, 34],
  [6, 22, 38],
  [6, 24, 42],
  [6, 26, 46],
  [6, 28, 50],
  [6, 30, 54],
  [6, 32, 58],
  [6, 34, 62],
  [6, 26, 46, 66],
  [6, 26, 48, 70],
  [6, 26, 50, 74],
  [6, 30, 54, 78],
  [6, 30, 56, 82],
  [6, 30, 58, 86],
  [6, 34, 62, 90],
];

const GF_EXP = new Uint8Array(512);
const GF_LOG = new Uint8Array(256);
{
  let x = 1;
  for (let i = 0; i < 255; i += 1) {
    GF_EXP[i] = x;
    GF_LOG[x] = i;
    x <<= 1;
    if ((x & 0x100) !== 0) x ^= 0x11d;
  }
  for (let i = 255; i < 512; i += 1) {
    GF_EXP[i] = GF_EXP[i - 255]!;
  }
}

function gfMul(a: number, b: number): number {
  if (a === 0 || b === 0) return 0;
  return GF_EXP[GF_LOG[a]! + GF_LOG[b]!]!;
}

function rsGenerator(degree: number): Uint8Array {
  let poly = new Uint8Array([1]);
  for (let i = 0; i < degree; i += 1) {
    const next = new Uint8Array(poly.length + 1);
    const root = GF_EXP[i]!;
    for (let j = 0; j < poly.length; j += 1) {
      next[j]! ^= poly[j]!;
      next[j + 1]! ^= gfMul(poly[j]!, root);
    }
    poly = next;
  }
  return poly;
}

function rsRemainder(data: Uint8Array, ecCount: number): Uint8Array {
  const gen = rsGenerator(ecCount);
  const work = new Uint8Array(data.length + ecCount);
  work.set(data);
  for (let i = 0; i < data.length; i += 1) {
    const coef = work[i]!;
    if (coef === 0) continue;
    for (let j = 0; j < gen.length; j += 1) {
      work[i + j]! ^= gfMul(gen[j]!, coef);
    }
  }
  return work.slice(data.length);
}

function pushBits(bytes: number[], bitBuf: { value: number; length: number }, val: number, count: number): void {
  for (let i = count - 1; i >= 0; i -= 1) {
    bitBuf.value = (bitBuf.value << 1) | ((val >>> i) & 1);
    bitBuf.length += 1;
    if (bitBuf.length === 8) {
      bytes.push(bitBuf.value & 0xff);
      bitBuf.value = 0;
      bitBuf.length = 0;
    }
  }
}

function encodeDataCodewords(payload: Uint8Array, version: number, totalDataCw: number): Uint8Array {
  const bytes: number[] = [];
  const bitBuf = { value: 0, length: 0 };
  const countBits = version >= 10 ? 16 : 8;
  pushBits(bytes, bitBuf, 0b0100, 4);
  pushBits(bytes, bitBuf, payload.length, countBits);
  for (const byte of payload) {
    pushBits(bytes, bitBuf, byte, 8);
  }
  const terminatorBits = Math.min(4, totalDataCw * 8 - (bytes.length * 8 + bitBuf.length));
  pushBits(bytes, bitBuf, 0, terminatorBits);
  if (bitBuf.length > 0) {
    pushBits(bytes, bitBuf, 0, 8 - bitBuf.length);
  }
  const padBytes = [0xec, 0x11];
  let padIdx = 0;
  while (bytes.length < totalDataCw) {
    bytes.push(padBytes[padIdx % 2]!);
    padIdx += 1;
  }
  return new Uint8Array(bytes);
}

function interleaveBlocks(data: Uint8Array, spec: VersionEcSpec): Uint8Array {
  const [, ecPerBlock, groups] = spec;
  const dataBlocks: Uint8Array[] = [];
  const ecBlocks: Uint8Array[] = [];
  let offset = 0;
  for (const [blockCount, dataCw] of groups) {
    for (let b = 0; b < blockCount; b += 1) {
      const slice = data.slice(offset, offset + dataCw);
      offset += dataCw;
      dataBlocks.push(slice);
      ecBlocks.push(rsRemainder(slice, ecPerBlock));
    }
  }
  const maxDataLen = Math.max(...dataBlocks.map((b) => b.length));
  const out: number[] = [];
  for (let i = 0; i < maxDataLen; i += 1) {
    for (const block of dataBlocks) {
      if (i < block.length) out.push(block[i]!);
    }
  }
  for (let i = 0; i < ecPerBlock; i += 1) {
    for (const block of ecBlocks) {
      out.push(block[i]!);
    }
  }
  return new Uint8Array(out);
}

function createEmptyGrid(size: number): { modules: boolean[][]; reserved: boolean[][] } {
  const modules = Array.from({ length: size }, () => Array<boolean>(size).fill(false));
  const reserved = Array.from({ length: size }, () => Array<boolean>(size).fill(false));
  return { modules, reserved };
}

function setFunc(modules: boolean[][], reserved: boolean[][], row: number, col: number, dark: boolean): void {
  if (row < 0 || row >= modules.length || col < 0 || col >= modules.length) return;
  modules[row]![col] = dark;
  reserved[row]![col] = true;
}

function drawFinder(modules: boolean[][], reserved: boolean[][], topRow: number, leftCol: number): void {
  for (let dy = -1; dy <= 7; dy += 1) {
    for (let dx = -1; dx <= 7; dx += 1) {
      const dark =
        dx >= 0 &&
        dx <= 6 &&
        dy >= 0 &&
        dy <= 6 &&
        (dx === 0 || dx === 6 || dy === 0 || dy === 6 || (dx >= 2 && dx <= 4 && dy >= 2 && dy <= 4));
      setFunc(modules, reserved, topRow + dy, leftCol + dx, dark);
    }
  }
}

function drawAlignment(modules: boolean[][], reserved: boolean[][], centerRow: number, centerCol: number): void {
  for (let dy = -2; dy <= 2; dy += 1) {
    for (let dx = -2; dx <= 2; dx += 1) {
      const dist = Math.max(Math.abs(dx), Math.abs(dy));
      setFunc(modules, reserved, centerRow + dy, centerCol + dx, dist !== 1);
    }
  }
}

function drawFunctionPatterns(modules: boolean[][], reserved: boolean[][], version: number): void {
  const size = modules.length;
  drawFinder(modules, reserved, 0, 0);
  drawFinder(modules, reserved, 0, size - 7);
  drawFinder(modules, reserved, size - 7, 0);

  for (let i = 8; i < size - 8; i += 1) {
    const dark = i % 2 === 0;
    setFunc(modules, reserved, 6, i, dark);
    setFunc(modules, reserved, i, 6, dark);
  }

  const align = ALIGNMENT_POSITIONS[version - 1] ?? [];
  for (let rIdx = 0; rIdx < align.length; rIdx += 1) {
    for (let cIdx = 0; cIdx < align.length; cIdx += 1) {
      const isFinderCorner =
        (rIdx === 0 && cIdx === 0) ||
        (rIdx === 0 && cIdx === align.length - 1) ||
        (rIdx === align.length - 1 && cIdx === 0);
      if (isFinderCorner) continue;
      drawAlignment(modules, reserved, align[rIdx]!, align[cIdx]!);
    }
  }

  // Reserve format info strips + mandatory dark module before placing data
  for (let i = 0; i <= 8; i += 1) {
    if (i !== 6) {
      reserved[8]![i] = true;
      reserved[i]![8] = true;
    }
  }
  for (let i = 0; i < 8; i += 1) {
    reserved[8]![size - 1 - i] = true;
    reserved[size - 1 - i]![8] = true;
  }
  setFunc(modules, reserved, size - 8, 8, true);

  if (version >= 7) {
    let rem = version;
    for (let i = 0; i < 12; i += 1) {
      rem = (rem << 1) ^ (((rem >>> 11) & 1) * 0x1f25);
    }
    const bits = (version << 12) | rem;
    for (let i = 0; i < 18; i += 1) {
      const bit = ((bits >>> i) & 1) !== 0;
      const a = size - 11 + (i % 3);
      const b = Math.floor(i / 3);
      setFunc(modules, reserved, a, b, bit);
      setFunc(modules, reserved, b, a, bit);
    }
  }
}

function drawFormatBits(modules: boolean[][], reserved: boolean[][], ecIndicator: 0 | 1, maskId: number): void {
  const size = modules.length;
  const data = (ecIndicator << 3) | maskId;
  let rem = data;
  for (let i = 0; i < 10; i += 1) {
    rem = (rem << 1) ^ (((rem >>> 9) & 1) * 0x537);
  }
  const bits = ((data << 10) | rem) ^ 0x5412;

  for (let i = 0; i <= 5; i += 1) setFunc(modules, reserved, 8, i, ((bits >>> (14 - i)) & 1) !== 0);
  setFunc(modules, reserved, 8, 7, ((bits >>> 8) & 1) !== 0);
  setFunc(modules, reserved, 8, 8, ((bits >>> 7) & 1) !== 0);
  setFunc(modules, reserved, 7, 8, ((bits >>> 6) & 1) !== 0);
  for (let i = 0; i <= 5; i += 1) setFunc(modules, reserved, 5 - i, 8, ((bits >>> (5 - i)) & 1) !== 0);

  for (let i = 0; i < 7; i += 1) setFunc(modules, reserved, size - 1 - i, 8, ((bits >>> (14 - i)) & 1) !== 0);
  for (let i = 0; i < 8; i += 1) setFunc(modules, reserved, 8, size - 8 + i, ((bits >>> (7 - i)) & 1) !== 0);
}

function placeCodewords(modules: boolean[][], reserved: boolean[][], codewords: Uint8Array): void {
  const size = modules.length;
  const totalBits = codewords.length * 8;
  let bitIdx = 0;
  let upward = true;
  for (let rightCol = size - 1; rightCol >= 1; rightCol -= 2) {
    if (rightCol === 6) rightCol = 5;
    for (let step = 0; step < size; step += 1) {
      const row = upward ? size - 1 - step : step;
      for (let offset = 0; offset < 2; offset += 1) {
        const col = rightCol - offset;
        if (reserved[row]![col]) continue;
        const bit = bitIdx < totalBits ? ((codewords[bitIdx >>> 3]! >>> (7 - (bitIdx & 7))) & 1) !== 0 : false;
        modules[row]![col] = bit;
        bitIdx += 1;
      }
    }
    upward = !upward;
  }
}

function maskMatches(maskId: number, row: number, col: number): boolean {
  switch (maskId) {
    case 0:
      return (row + col) % 2 === 0;
    case 1:
      return row % 2 === 0;
    case 2:
      return col % 3 === 0;
    case 3:
      return (row + col) % 3 === 0;
    case 4:
      return (Math.floor(row / 2) + Math.floor(col / 3)) % 2 === 0;
    case 5:
      return ((row * col) % 2) + ((row * col) % 3) === 0;
    case 6:
      return (((row * col) % 2) + ((row * col) % 3)) % 2 === 0;
    default:
      return (((row + col) % 2) + ((row * col) % 3)) % 2 === 0;
  }
}

function penaltyLineRuns(line: boolean[]): number {
  let penalty = 0;
  let runColor = line[0]!;
  let runLen = 1;
  for (let i = 1; i < line.length; i += 1) {
    if (line[i] === runColor) {
      runLen += 1;
    } else {
      if (runLen >= 5) penalty += 3 + (runLen - 5);
      runColor = line[i]!;
      runLen = 1;
    }
  }
  if (runLen >= 5) penalty += 3 + (runLen - 5);
  return penalty;
}

function countFinderPatterns(line: boolean[]): number {
  let count = 0;
  for (let i = 0; i <= line.length - 11; i += 1) {
    const fwd =
      line[i] &&
      !line[i + 1] &&
      line[i + 2] &&
      line[i + 3] &&
      line[i + 4] &&
      !line[i + 5] &&
      line[i + 6] &&
      !line[i + 7] &&
      !line[i + 8] &&
      !line[i + 9] &&
      !line[i + 10];
    const rev =
      !line[i] &&
      !line[i + 1] &&
      !line[i + 2] &&
      !line[i + 3] &&
      line[i + 4] &&
      !line[i + 5] &&
      line[i + 6] &&
      line[i + 7] &&
      line[i + 8] &&
      !line[i + 9] &&
      line[i + 10];
    if (fwd || rev) count += 1;
  }
  return count;
}

function penaltyScore(modules: boolean[][]): number {
  const size = modules.length;
  let penalty = 0;
  let darkCount = 0;

  for (let r = 0; r < size; r += 1) {
    const row = modules[r]!;
    penalty += penaltyLineRuns(row);
    penalty += countFinderPatterns(row) * 40;
    const col: boolean[] = [];
    for (let c = 0; c < size; c += 1) {
      if (row[c]) darkCount += 1;
      col.push(modules[c]![r]!);
    }
    penalty += penaltyLineRuns(col);
    penalty += countFinderPatterns(col) * 40;
  }

  for (let r = 0; r < size - 1; r += 1) {
    for (let c = 0; c < size - 1; c += 1) {
      const m = modules[r]![c]!;
      if (m === modules[r]![c + 1] && m === modules[r + 1]![c] && m === modules[r + 1]![c + 1]) {
        penalty += 3;
      }
    }
  }

  const total = size * size;
  const k = Math.floor(Math.abs(darkCount * 20 - total * 10) / total);
  return penalty + k * 10;
}

function applyMask(modules: boolean[][], reserved: boolean[][], maskId: number): boolean[][] {
  return modules.map((row, r) =>
    row.map((val, c) => (reserved[r]![c] ? val : maskMatches(maskId, r, c) ? !val : val)),
  );
}

/**
 * Encode UTF-8 text into a Version 1..20 QR matrix (EC level M, fallback EC L).
 * Returns `null` when the payload exceeds 858 UTF-8 bytes.
 */
export function encodeQrMatrix(text: string): QrMatrix | null {
  const payload = new TextEncoder().encode(text);
  let chosen: { version: number; ecIndicator: 0 | 1; spec: VersionEcSpec } | null = null;

  for (let v = 1; v <= SPEC_M.length; v += 1) {
    const headerBits = v >= 10 ? 4 + 16 : 4 + 8;
    const neededBytes = Math.ceil((headerBits + payload.length * 8) / 8);
    const spec = SPEC_M[v - 1]!;
    if (neededBytes <= spec[0]) {
      chosen = { version: v, ecIndicator: 0, spec };
      break;
    }
  }
  if (!chosen) {
    for (let v = 1; v <= SPEC_L.length; v += 1) {
      const headerBits = v >= 10 ? 4 + 16 : 4 + 8;
      const neededBytes = Math.ceil((headerBits + payload.length * 8) / 8);
      const spec = SPEC_L[v - 1]!;
      if (neededBytes <= spec[0]) {
        chosen = { version: v, ecIndicator: 1, spec };
        break;
      }
    }
  }
  if (!chosen) return null;

  const size = 17 + 4 * chosen.version;
  const { modules, reserved } = createEmptyGrid(size);
  drawFunctionPatterns(modules, reserved, chosen.version);
  const data = encodeDataCodewords(payload, chosen.version, chosen.spec[0]);
  const codewords = interleaveBlocks(data, chosen.spec);
  placeCodewords(modules, reserved, codewords);

  let bestScore = Infinity;
  let bestGrid = modules;

  for (let maskId = 0; maskId < 8; maskId += 1) {
    const candidate = applyMask(modules, reserved, maskId);
    const candidateReserved = reserved.map((row) => [...row]);
    drawFormatBits(candidate, candidateReserved, chosen.ecIndicator, maskId);
    const score = penaltyScore(candidate);
    if (score < bestScore) {
      bestScore = score;
      bestGrid = candidate;
    }
  }

  return { size, modules: bestGrid };
}

/** Build an SVG `<path d="...">` string covering every dark QR module in `matrix`. */
export function qrSvgPath(matrix: QrMatrix): string {
  const parts: string[] = [];
  for (let r = 0; r < matrix.size; r += 1) {
    const row = matrix.modules[r]!;
    for (let c = 0; c < matrix.size; c += 1) {
      if (row[c]) parts.push(`M${c},${r}h1v1h-1z`);
    }
  }
  return parts.join("");
}
