import type { MilestoneRow } from "../../lib/types";
import type { Locale } from "../../lib/locale";

export const CHECKPOINT_LABELS = { planned: "Planned", active: "Active", achieved: "Achieved", cancelled: "Cancelled" };
type Translate = (english: string) => string;

/** Missing metadata on older servers stays unknown, never inferred from work %. */
export function checkpointLabel(row: MilestoneRow, t: Translate = (english) => english): string {
  if (row.status) return t(CHECKPOINT_LABELS[row.status] ?? "Status unknown");
  return t(row.configured ? "Status not set" : "Needs definition");
}

/** A calendar date, formatted in UTC so users west of UTC see the same day. */
export function checkpointDate(date: string | null | undefined, locale: Locale = "en"): string | null {
  if (!date || !/^\d{4}-\d{2}-\d{2}$/.test(date)) return null;
  const value = new Date(`${date}T00:00:00Z`);
  if (Number.isNaN(value.getTime()) || value.toISOString().slice(0, 10) !== date) return null;
  return value.toLocaleDateString(locale, { month: "short", day: "numeric", year: "numeric", timeZone: "UTC" });
}

export function checkpointWork(row: MilestoneRow): string {
  const counts = row.progress.counts;
  const total = Math.max(0, (counts.total ?? 0) - (counts.dropped ?? 0));
  return total ? `${counts.done ?? 0} of ${total} tasks complete` : "No delivery scope yet";
}

/** Explicitly active checkpoints first. Upcoming uses declared roadmap order. */
export function currentCheckpoints(rows: MilestoneRow[]): MilestoneRow[] {
  const active = rows.filter((row) => row.configured && row.status === "active");
  if (active.length) return active;
  const next = rows.find((row) => row.configured && row.status === "planned");
  return next ? [next] : [];
}

export function scheduleLabel(row: MilestoneRow, t: Translate = (english) => english, formatNumber: (value: number) => string = String): string | null {
  const schedule = row.schedule;
  if (!schedule) return null;
  if (schedule.state === "overdue") return t("{days} days overdue").replace("{days}", formatNumber(Math.abs(schedule.days_remaining ?? 0)));
  if (schedule.state === "due_today") return t("Due today");
  return null;
}
