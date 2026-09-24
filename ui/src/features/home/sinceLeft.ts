/**
 * "Since you left" (CAD-328): the browser remembers when it last had
 * Home open, and a return after at least an hour shows a compact card
 * with the daemon's `master_summary` (CAD-339) for that span. The
 * last-seen time is per browser (localStorage); storage that throws or
 * is missing (private mode, blocked site data) means no card, never an
 * error. The summary is read through `summaryView`, so a daemon that
 * omits a section shows it as unknown rather than breaking the card.
 * Tested in tests/sinceLeft.test.ts.
 */

export const LAST_SEEN_KEY = "cadence-home-last-seen";
/** A return after this long shows the card. */
export const AWAY_SECS = 3600;

/** The part of `Storage` used here. */
export interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

function browserStorage(): StorageLike | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

/** Epoch seconds Home was last seen in this browser, or null. */
export function readLastSeen(storage: StorageLike | null = browserStorage()): number | null {
  try {
    const raw = storage?.getItem(LAST_SEEN_KEY);
    const n = raw == null ? NaN : Number(raw);
    return Number.isFinite(n) && n > 0 ? Math.floor(n) : null;
  } catch {
    return null;
  }
}

export function writeLastSeen(now: number, storage: StorageLike | null = browserStorage()): void {
  try {
    storage?.setItem(LAST_SEEN_KEY, String(Math.floor(now)));
  } catch {
    // Storage full or blocked: the next visit just shows no card.
  }
}

/** The span to summarise, or null: first visit, or back within the hour. */
export function awaySince(lastSeen: number | null, now: number): number | null {
  if (lastSeen === null || lastSeen > now) return null;
  return now - lastSeen >= AWAY_SECS ? lastSeen : null;
}

/** `5400` → `1 h`, `93600` → `1 d 2 h`. */
export function awayLabel(secs: number): string {
  const h = Math.floor(Math.max(0, secs) / 3600);
  if (h < 24) return `${Math.max(1, h)} h`;
  const d = Math.floor(h / 24);
  return h % 24 ? `${d} d ${h % 24} h` : `${d} d`;
}

export interface SummaryRow {
  key: string;
  /** An issue id to open, when the row has one. */
  issue: string | null;
  text: string;
}

export interface SummarySection {
  label: string;
  /** null — the daemon could not tell (e.g. tracker history unreadable). */
  rows: SummaryRow[] | null;
}

export interface SummaryView {
  sections: SummarySection[];
  /** Nothing happened at all. */
  quiet: boolean;
  /** Reports held back from the master by the router's per-pass cap. */
  backlog: number;
}

function list(v: unknown): Record<string, unknown>[] | null {
  return Array.isArray(v) ? v.filter((r): r is Record<string, unknown> => !!r && typeof r === "object") : null;
}

function s(v: unknown): string {
  return typeof v === "string" ? v : typeof v === "number" ? String(v) : "";
}

function rows(
  v: unknown,
  line: (r: Record<string, unknown>) => string,
  issueOf: (r: Record<string, unknown>) => unknown,
): SummaryRow[] | null {
  const items = list(v);
  if (!items) return null;
  return items.map((r, i) => ({
    key: `${s(issueOf(r))}:${i}`,
    issue: s(issueOf(r)) || null,
    text: line(r),
  }));
}

/** `master_summary`'s JSON → the card's sections. */
export function summaryView(summary: Record<string, unknown>): SummaryView {
  const plans: Record<string, unknown>[] = [
    ...(list(summary.plans_proposed) ?? []).map((p) => ({ ...p, _verb: "proposed" })),
    ...(list(summary.plans_decided) ?? []).map((p) => ({ ...p, _verb: s(p.state) || "decided" })),
  ];
  const plansKnown = list(summary.plans_proposed) !== null || list(summary.plans_decided) !== null;
  const sections: SummarySection[] = [
    {
      label: "Plans",
      rows: plansKnown
        ? plans.map((p, i) => ({
            key: `${s(p.epic)}:${i}`,
            issue: s(p.epic) || null,
            text: `${s(p.epic)} ${s(p.title)} — ${s(p._verb)}`.trim(),
          }))
        : null,
    },
    {
      label: "Tickets moved",
      rows: rows(summary.tickets_moved, (t) => `${s(t.issue)} → ${s(t.status)}`, (t) => t.issue),
    },
    {
      label: "Reports",
      rows: rows(summary.reports, (r) => `${s(r.issue)} ${s(r.kind)} by ${s(r.agent)}`, (r) => r.issue),
    },
    {
      label: "Open questions",
      rows: rows(
        summary.open_questions,
        (q) => `${s(q.issue)} from ${s(q.agent)}${q.escalated === true ? " · with you" : ""}`,
        (q) => q.issue,
      ),
    },
  ];
  const quiet = sections.every((sec) => sec.rows !== null && sec.rows.length === 0);
  const backlog = typeof summary.routing_backlog === "number" ? summary.routing_backlog : 0;
  return { sections, quiet, backlog };
}
