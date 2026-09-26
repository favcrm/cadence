/**
 * A page's history (CAD-581): one row per revision, newest first, the
 * current one marked, and a relative time for the list (the board's
 * fmtTime prints an absolute stamp where one is needed).
 */

export interface WikiVersion {
  rev: string;
  author?: string | null;
  at?: string | null;
  summary?: string | null;
}

export interface VersionRow extends WikiVersion {
  current: boolean;
  when: string;
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const WEEK = 7 * DAY;

/** `2 h ago`, `yesterday`, `3 d ago`, `1 wk ago`; non-ISO input passes through. */
export function relTime(iso: string | null | undefined, now = Date.now()): string {
  if (!iso) return "";
  const at = Date.parse(iso);
  if (Number.isNaN(at)) return iso;
  const delta = now - at;
  if (delta < MINUTE) return "just now";
  if (delta < HOUR) return `${Math.floor(delta / MINUTE)} min ago`;
  if (delta < DAY) return `${Math.floor(delta / HOUR)} h ago`;
  if (delta < 2 * DAY) return "yesterday";
  if (delta < WEEK) return `${Math.floor(delta / DAY)} d ago`;
  if (delta < 5 * WEEK) return `${Math.floor(delta / WEEK)} wk ago`;
  return `${Math.floor(delta / (30 * DAY))} mo ago`;
}

/** Newest first; `currentRev` marks the row the page is on. */
export function versionRows(
  entries: WikiVersion[],
  currentRev: string | null,
  now = Date.now(),
): VersionRow[] {
  return entries.map((entry) => ({
    ...entry,
    current: currentRev != null && entry.rev === currentRev,
    when: relTime(entry.at, now),
  }));
}

/** `master · current`, or the author alone. */
export function versionWho(row: VersionRow): string {
  const who = row.author || "unknown";
  return row.current ? `${who} · current` : who;
}
