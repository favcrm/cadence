import type { TimelineEvent, TimelineKind } from "../../lib/types";

/** CAD-2240: pure model for the hosted Timeline tab. */
export const TIMELINE_KINDS: { kind: TimelineKind; label: string }[] = [
  { kind: "created", label: "Created" },
  { kind: "status", label: "Status" },
  { kind: "owner", label: "Owner" },
  { kind: "claim", label: "Claimed" },
  { kind: "comment", label: "Comment" },
  { kind: "message", label: "Message" },
  { kind: "review", label: "Review" },
];

/** The kind chips. No Claim chip: boards record no claim events yet (agenticos-v2#1945);
 *  add it when they do. A claim entry that does arrive still renders under All. */
export const FILTER_KINDS = TIMELINE_KINDS.filter((k) => k.kind !== "claim");

/** The kind label shown on an entry; a status event that landed work reads "Landed". */
export function entryLabel(event: TimelineEvent): string {
  if (event.kind === "status" && event.landed) return "Landed";
  return TIMELINE_KINDS.find((k) => k.kind === event.kind)?.label ?? event.kind;
}

/** The change an entry records, as untranslated parts (`from`/`to` stay as the board wrote them). A landed one keeps its change: the label already says "Landed". */
export function entryChange(event: TimelineEvent): { from: string | null; to: string | null } | null {
  if (event.kind === "status") return { from: event.from, to: event.to };
  if (event.kind === "owner") return { from: event.from, to: event.to };
  if (event.kind === "claim" && (event.from || event.to)) return { from: event.from, to: event.to };
  return null;
}

export interface TimelineDay {
  /** Local calendar day, `YYYY-MM-DD`. */
  key: string;
  at: string;
  entries: { event: TimelineEvent; hereBefore: boolean }[];
}

const pad = (n: number) => String(n).padStart(2, "0");
export function localDay(ms: number): string {
  const d = new Date(ms);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/**
 * Newest-first events grouped by local day. `hereBefore` marks the first entry
 * at or before `lastVisit`, so "You were last here" sits between newer and
 * older entries — and only when entries exist on both sides of it.
 */
export function groupByDay(events: readonly TimelineEvent[], lastVisit: number | null): TimelineDay[] {
  const days: TimelineDay[] = [];
  const firstOlder = lastVisit === null ? -1 : events.findIndex((e) => Date.parse(e.at) <= lastVisit);
  events.forEach((event, index) => {
    const key = localDay(Date.parse(event.at));
    let day = days[days.length - 1];
    if (!day || day.key !== key) {
      day = { key, at: event.at, entries: [] };
      days.push(day);
    }
    day.entries.push({ event, hereBefore: index > 0 && index === firstOlder });
  });
  return days;
}

const visitKey = (project: string) => `cadence.timeline.visit.${project}`;

/** The previous visit (ms) for this project in this browser; null when unknown or storage throws. */
export function readLastVisit(project: string): number | null {
  try {
    const raw = globalThis.localStorage?.getItem(visitKey(project));
    const ms = raw === null || raw === undefined ? NaN : Number(raw);
    return Number.isFinite(ms) ? ms : null;
  } catch {
    return null;
  }
}

export function writeLastVisit(project: string, ms: number): void {
  try {
    globalThis.localStorage?.setItem(visitKey(project), String(ms));
  } catch {
    // Storage can be blocked or full; the page works without a remembered visit.
  }
}
