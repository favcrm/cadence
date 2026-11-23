/**
 * CAD-2555 / CAD-2690: the line a view card and the canvas header show for a
 * declared view's params, e.g. "1 Jul – 30 Sep 2026 · Prior year · Monthly".
 * Generic: it knows no app, only that a param value shaped like a calendar day
 * reads better as one and that a raw value reads better humanized.
 */
import type { RouteParams } from "../workspace-apps/screen/routeParams";
import type { DeclaredView } from "../workspace-apps/workspaceApps";

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

/** "Prior year" from `prior_year`: underscores read as spaces, first letter up. */
function humanize(value: string): string {
  const words = value.replace(/_+/g, " ").trim();
  return words.charAt(0).toUpperCase() + words.slice(1);
}

/** The app's own words for one param value, or undefined (CAD-2710). */
export type OptionLabel = (param: string, value: string) => string | undefined;

const own = (obj: object, key: string) => Object.prototype.hasOwnProperty.call(obj, key);

/** The exact locale, then its language, then `en`; undefined when the declaration has none of them. */
function localeText(texts: Record<string, string>, locale: string): string | undefined {
  const lower = new Map(Object.entries(texts).map(([k, v]) => [k.toLowerCase(), v]));
  const tag = locale.toLowerCase();
  return lower.get(tag) ?? lower.get(tag.split("-")[0]) ?? lower.get("en");
}

/** The declared labels of one view's enum params for a locale; undefined when the declaration has no such view. */
export function declaredOptionLabel(views: readonly DeclaredView[] | null | undefined, viewId: string | undefined, locale: string): OptionLabel | undefined {
  const params = viewId === undefined ? undefined : views?.find((v) => v.id === viewId)?.params;
  if (!params) return undefined;
  return (param, value) => {
    const labels = own(params, param) ? params[param]?.labels : undefined;
    const texts = labels && own(labels, value) ? labels[value] : undefined;
    return texts ? localeText(texts, locale) : undefined;
  };
}

/**
 * CAD-2690: the view's options in words on one line ("" when there are none), in the card's own order.
 * `from` + `to` days read as one range ("1 Jul – 30 Sep 2026"), any other day as "as at <day>",
 * and every other value humanized (`prior_year` → "Prior year"). `t` is the catalogue.
 * CAD-2710: `label` supplies the app's declared words for a value; one it has none for is humanized as before.
 */
export function optionsLine(params: RouteParams | undefined, locale: string, t: (english: string) => string, label?: OptionLabel): string {
  const entries = Object.entries(params ?? {});
  const days = entries.map(([, value]) => day(value));
  const at = (key: string) => days[entries.findIndex(([k]) => k === key)] ?? null;
  const [from, to] = [at("from"), at("to")];
  const range = from && to ? `${format(from, locale, from.y !== to.y)} – ${format(to, locale)}` : null;
  const parts: string[] = [];
  let ranged = false;
  entries.forEach(([key, value], i) => {
    const d = days[i];
    if (range !== null && (key === "from" || key === "to")) {
      if (!ranged) parts.push(range);
      ranged = true;
    } else parts.push(label?.(key, value) ?? (d ? t("as at {date}").replace("{date}", format(d, locale)) : t(humanize(value))));
  });
  return parts.join(" · ");
}
