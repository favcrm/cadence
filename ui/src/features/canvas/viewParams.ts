/**
 * CAD-2555: the compact line a view card and the canvas header show for a
 * declared view's params, e.g. "1 Jul – 30 Sep 2026". Generic: it knows no app,
 * only that a param value shaped like a calendar day reads better as one.
 * Everything else shows as written, in the card's own order.
 */
import type { RouteParams } from "../workspace-apps/screen/routeParams";

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const DAY = /^(\d{4})-(\d{2})-(\d{2})$/;

type Day = { y: number; m: number; d: number };

function day(value: string): Day | null {
  const hit = DAY.exec(value);
  if (!hit) return null;
  const [y, m, d] = [Number(hit[1]), Number(hit[2]), Number(hit[3])];
  const real = new Date(Date.UTC(y, m - 1, d));
  return real.getUTCFullYear() === y && real.getUTCMonth() === m - 1 && real.getUTCDate() === d ? { y, m, d } : null;
}

function format(value: Day, locale: string, withYear = true): string {
  if (locale === "zh-TW") return `${withYear ? `${value.y}年` : ""}${value.m}月${value.d}日`;
  return `${value.d} ${MONTHS[value.m - 1]}${withYear ? ` ${value.y}` : ""}`;
}

/** One chip per param, in the card's own order; two calendar days read as one range ("1 Jul – 30 Sep 2026"). */
export function paramChips(params: RouteParams | undefined, locale: string): string[] {
  const values = Object.values(params ?? {});
  const days = values.map(day);
  if (values.length === 2 && days[0] && days[1]) {
    const [from, to] = days;
    return [`${format(from, locale, from.y !== to.y)} – ${format(to, locale)}`];
  }
  return values.map((value, i) => { const d = days[i]; return d ? format(d, locale) : value; });
}

/** "" when there are no params; else the chips on one line. */
export function paramsLine(params: RouteParams | undefined, locale: string): string {
  return paramChips(params, locale).join(" · ");
}
