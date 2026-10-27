import { useTranslate } from "../../lib/locale";
import type { ThreadItem } from "./thread";

/** Local day key for an epoch-ms instant (the reader's own timezone). */
export function dayKey(ms: number): string {
  const d = new Date(ms);
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
}

/** The instant an item was created, or null when it carries none. */
export function itemCreated(item: ThreadItem): number | null {
  if (item.type === "pending") return null;
  const entry =
    item.type === "tools" ? item.entries[item.entries.length - 1] : item.entry;
  const created = entry?.created;
  if (typeof created === "number") return created < 1e12 ? created * 1000 : created;
  if (typeof created === "string") {
    const ms = Date.parse(created);
    return Number.isNaN(ms) ? null : ms;
  }
  return null;
}

/** "Today", "Yesterday", or a short local date (with the year when it differs). */
export function dayText(ms: number, now: number, t: (key: string) => string): string {
  const day = new Date(ms);
  const today = new Date(now);
  const diff = Math.round(
    (new Date(today.getFullYear(), today.getMonth(), today.getDate()).getTime() -
      new Date(day.getFullYear(), day.getMonth(), day.getDate()).getTime()) /
      86_400_000,
  );
  if (diff === 0) return t("Today");
  if (diff === 1) return t("Yesterday");
  return day.toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    ...(day.getFullYear() === today.getFullYear() ? {} : { year: "numeric" }),
  });
}

/** A quiet day rule between two days of a transcript (CAD-1886). */
export function DayDivider({ ms, now = Date.now() }: { ms: number; now?: number }) {
  const t = useTranslate();
  return (
    <li className="day-sep" data-day-sep>
      <span>{dayText(ms, now, t)}</span>
    </li>
  );
}
