import type { OutboxItem } from "../../lib/types";

export function readOutboxFilters(search: string) {
  const params = new URLSearchParams(search);
  return {
    query: params.get("outbox_q") ?? "",
    project: params.get("outbox_project") ?? "",
    item: params.get("item") || null,
  };
}

/** Keep list filters and unrelated parameters when opening/closing a post. */
export function outboxHref(
  search: string,
  changes: { item?: string | null; query?: string; project?: string },
): string {
  const params = new URLSearchParams(search);
  const keys = {
    item: "item",
    query: "outbox_q",
    project: "outbox_project",
  } as const;
  for (const key of Object.keys(keys) as (keyof typeof keys)[]) {
    if (changes[key] === undefined) continue;
    const value = changes[key];
    if (value) params.set(keys[key], value);
    else params.delete(keys[key]);
  }
  const query = params.toString();
  return `/outbox${query ? `?${query}` : ""}`;
}

export function postTitle(item: OutboxItem): string {
  return item.title.trim() || "Untitled post";
}

/** Exact project identity; all search terms must match the publication. */
export function visiblePosts(
  items: OutboxItem[],
  query: string,
  project: string,
) {
  const terms = query.toLowerCase().trim().split(/\s+/).filter(Boolean);
  return items
    .filter((item) => {
      if (project && item.project !== project) return false;
      const text = [
        item.title,
        item.project,
        item.preview,
        item.effect_id,
        ...(item.attachments ?? []).map((attachment) => attachment.name),
      ]
        .join(" ")
        .toLowerCase();
      return terms.every((term) => text.includes(term));
    })
    .sort((a, b) => {
      const aTime = Date.parse(a.published_at);
      const bTime = Date.parse(b.published_at);
      const time =
        (Number.isFinite(bTime) ? bTime : -Infinity) -
        (Number.isFinite(aTime) ? aTime : -Infinity);
      if (time && !Number.isNaN(time)) return time;
      return a.effect_id < b.effect_id ? -1 : a.effect_id > b.effect_id ? 1 : 0;
    });
}

/** Include the year and timezone: publication history can span years. */
export function publicationTime(value: string): string {
  const date = new Date(value);
  if (!Number.isFinite(date.getTime())) return value || "Date unavailable";
  return `${new Intl.DateTimeFormat("en-GB", {
    day: "numeric",
    month: "short",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    timeZone: "UTC",
    hourCycle: "h23",
  }).format(date)} UTC`;
}
