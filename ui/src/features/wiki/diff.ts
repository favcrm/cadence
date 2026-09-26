/**
 * Diff display (CAD-581): `git diff` output from the history route, and
 * the editor's "review diff" — the local draft against the server's copy.
 * Both come out as the same line list, so one component renders them.
 */

export type DiffKind = "add" | "del" | "ctx" | "hunk";

export interface DiffLine {
  kind: DiffKind;
  text: string;
}

const META = /^(diff |index |--- |\+\+\+ |new file|deleted file|similarity |rename |old mode|new mode|Binary files)/;

/** Parse unified-diff text into display lines. */
export function unifiedDiffLines(diff: string): DiffLine[] {
  const out: DiffLine[] = [];
  for (const raw of diff.split("\n")) {
    if (raw === "" ) continue;
    if (raw.startsWith("@@")) out.push({ kind: "hunk", text: raw });
    else if (META.test(raw)) out.push({ kind: "hunk", text: raw });
    else if (raw.startsWith("+")) out.push({ kind: "add", text: raw.slice(1) });
    else if (raw.startsWith("-")) out.push({ kind: "del", text: raw.slice(1) });
    else out.push({ kind: "ctx", text: raw.startsWith(" ") ? raw.slice(1) : raw });
  }
  return out;
}

/** Longest-common-subsequence line diff, capped so a huge paste cannot hang. */
const MAX_CELLS = 4_000_000;

export function lineDiff(before: string, after: string): DiffLine[] {
  const a = before.split("\n");
  const b = after.split("\n");
  if (a.length * b.length > MAX_CELLS) {
    return [
      ...a.map((text): DiffLine => ({ kind: "del", text })),
      ...b.map((text): DiffLine => ({ kind: "add", text })),
    ];
  }
  const n = a.length;
  const m = b.length;
  // lcs[i][j] = the length of the common subsequence of a[i:], b[j:].
  const lcs: number[][] = Array.from({ length: n + 1 }, () => new Array<number>(m + 1).fill(0));
  for (let i = n - 1; i >= 0; i -= 1) {
    for (let j = m - 1; j >= 0; j -= 1) {
      lcs[i][j] = a[i] === b[j] ? lcs[i + 1][j + 1] + 1 : Math.max(lcs[i + 1][j], lcs[i][j + 1]);
    }
  }
  const out: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) {
      out.push({ kind: "ctx", text: a[i] });
      i += 1;
      j += 1;
    } else if (lcs[i + 1][j] >= lcs[i][j + 1]) {
      out.push({ kind: "del", text: a[i] });
      i += 1;
    } else {
      out.push({ kind: "add", text: b[j] });
      j += 1;
    }
  }
  while (i < n) out.push({ kind: "del", text: a[i++] });
  while (j < m) out.push({ kind: "add", text: b[j++] });
  return out;
}

/** One line per change, for a summary chip. */
export function diffCounts(lines: DiffLine[]): { added: number; removed: number } {
  return {
    added: lines.filter((l) => l.kind === "add").length,
    removed: lines.filter((l) => l.kind === "del").length,
  };
}

/** The heading over the diff, e.g. `diff — f0c33d8 → a41f9c2`. */
export function diffLabel(from: string, to: string): string {
  return `diff — ${from} → ${to}`;
}
