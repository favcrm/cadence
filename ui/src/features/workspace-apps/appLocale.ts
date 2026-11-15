export interface AppDisplayLocale { title?: string; summary?: string; tagline?: string; }
export type AppDisplayI18n = Record<string, AppDisplayLocale>;
export interface ListingDisplayLocale extends AppDisplayLocale { about?: string; can?: string[]; access_notes?: Record<string, string>; }
export type AppListingLocale = ListingDisplayLocale;
export type AppListingI18n = Record<string, AppListingLocale>;
export type TranslateFn = (key: string) => string;

export const MAX_I18N_LOCALES = 8;
const LOCALE_TAG_RE = /^[A-Za-z]{2,3}(-[A-Za-z0-9]{2,8}){0,2}$/;
const SLOT_KEY_RE = /^[a-z0-9][a-z0-9._-]{0,63}$/;
const CTRL_SINGLE_LINE_RE = /[\u0000-\u001f\u007f-\u009f\u2028\u2029]/;
const CTRL_MULTILINE_RE = /[\u0000-\u0009\u000b-\u001f\u007f-\u009f\u2028\u2029]/;
const TRADITIONAL_CHINESE_TAGS = new Set(["zh-tw", "zh-hk", "zh-mo", "zh-hant"]);
const FORBIDDEN_PLAIN_OBJECT_KEYS = new Set(["__proto__", "prototype", "constructor"]);
const ZH_TW_APP_CATEGORIES: Record<string, string> = {
  marketing: "行銷", customers: "客戶", operations: "營運", finance: "財務", content: "內容", other: "其他",
};
const DISPLAY_KEYS = new Set(["title", "summary", "tagline"]);
const LISTING_KEYS = new Set(["title", "summary", "tagline", "about", "can", "access_notes"]);

export function isPlainJsonObject(value: unknown): value is Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return false;
  const proto = Object.getPrototypeOf(value);
  if (proto !== Object.prototype && proto !== null) return false;
  if (Object.getOwnPropertySymbols(value).length > 0) return false;
  const descs = Object.getOwnPropertyDescriptors(value);
  return Object.keys(descs).every((k) => {
    const d = descs[k];
    return !FORBIDDEN_PLAIN_OBJECT_KEYS.has(k) && !!d && typeof d.get !== "function" && typeof d.set !== "function";
  });
}

function setOwn<T>(target: Record<string, T>, key: string, value: T): void {
  Object.defineProperty(target, key, { value, enumerable: true, configurable: true, writable: true });
}

export function validLocaleTag(tag: string): boolean {
  return tag.length >= 2 && tag.length <= 20 && !/[\r\n]/.test(tag) && LOCALE_TAG_RE.test(tag);
}

export function isPlainTextValue(value: unknown, max: number, multiline = false): value is string {
  if (typeof value !== "string") return false;
  const trimmed = value.trim();
  const ctrlRe = multiline ? CTRL_MULTILINE_RE : CTRL_SINGLE_LINE_RE;
  return trimmed.length > 0 && trimmed.length <= max && !ctrlRe.test(trimmed);
}

export function isEnglishLocale(locale: string): boolean {
  const norm = locale.trim().replace(/_/g, "-").toLowerCase();
  return norm === "" || norm === "en" || norm.startsWith("en-");
}

/** Ordered locale candidates; Traditional Chinese never falls back to `zh`/`zh-CN`/`zh-Hans`. */
export function localeCandidates(locale: string): string[] {
  const raw = locale.trim().replace(/_/g, "-");
  if (!raw) return [];
  const lower = raw.toLowerCase();
  const seen = new Set<string>(), out: string[] = [];
  const push = (tag: string) => { const k = tag.toLowerCase(); if (!seen.has(k)) { seen.add(k); out.push(tag); } };
  push(raw);
  if (TRADITIONAL_CHINESE_TAGS.has(lower) || lower.startsWith("zh-hant-")) {
    push("zh-TW"); push("zh-HK"); push("zh-Hant");
    return out;
  }
  const base = lower.split("-")[0];
  if (base && base !== "zh") push(base);
  return out;
}

export function pickLocaleEntry<T>(map: Record<string, T> | null | undefined, locale: string): T | undefined {
  if (!map || typeof map !== "object" || Array.isArray(map)) return undefined;
  const byLower = new Map<string, T>(Object.entries(map).map(([k, v]) => [k.toLowerCase(), v]));
  for (const c of localeCandidates(locale)) {
    const found = byLower.get(c.toLowerCase());
    if (found !== undefined) return found;
  }
  return undefined;
}

function parseLocaleEntries<T extends object>(
  value: unknown,
  allowedKeys: Set<string>,
  parseEntry: (raw: Record<string, unknown>) => T | null,
): Record<string, T> | null {
  if (!isPlainJsonObject(value)) return null;
  const entries = Object.entries(value);
  if (entries.length === 0 || entries.length > MAX_I18N_LOCALES) return null;
  const out: Record<string, T> = {}, seen = new Set<string>();
  for (const [tag, raw] of entries) {
    if (!validLocaleTag(tag)) return null;
    const lower = tag.toLowerCase();
    if (seen.has(lower)) return null;
    seen.add(lower);
    if (!isPlainJsonObject(raw)) return null;
    const keys = Object.keys(raw);
    if (keys.length === 0 || keys.some((k) => !allowedKeys.has(k))) return null;
    const parsed = parseEntry(raw);
    if (!parsed || Object.keys(parsed).length === 0) return null;
    setOwn(out, tag, parsed);
  }
  return out;
}

function parseBaseDisplay(raw: Record<string, unknown>): AppDisplayLocale | null {
  const out: AppDisplayLocale = {};
  for (const [k, max, multi] of [["title", 80, false], ["summary", 280, true], ["tagline", 80, false]] as const) {
    if (raw[k] !== undefined) {
      if (!isPlainTextValue(raw[k], max, multi)) return null;
      out[k] = raw[k].trim();
    }
  }
  return out;
}

export function parseAppDisplayI18n(value: unknown): AppDisplayI18n | null {
  return parseLocaleEntries(value, DISPLAY_KEYS, parseBaseDisplay);
}

export function parseListingDisplayI18n(value: unknown): AppListingI18n | null {
  return parseLocaleEntries(value, LISTING_KEYS, (raw) => {
    const base = parseBaseDisplay(raw);
    if (!base) return null;
    const loc: AppListingLocale = { ...base };
    if (raw.about !== undefined) {
      if (!isPlainTextValue(raw.about, 600, true)) return null;
      loc.about = raw.about.trim();
    }
    if (raw.can !== undefined) {
      if (!Array.isArray(raw.can) || raw.can.length === 0 || raw.can.length > 5) return null;
      const can: string[] = [];
      for (const item of raw.can) {
        if (!isPlainTextValue(item, 100)) return null;
        can.push(item.trim());
      }
      loc.can = can;
    }
    if (raw.access_notes !== undefined) {
      if (!isPlainJsonObject(raw.access_notes)) return null;
      const noteEntries = Object.entries(raw.access_notes);
      if (noteEntries.length === 0 || noteEntries.length > 16) return null;
      const notes: Record<string, string> = {};
      for (const [k, v] of noteEntries) {
        if (FORBIDDEN_PLAIN_OBJECT_KEYS.has(k) || !SLOT_KEY_RE.test(k) || !isPlainTextValue(v, 140)) return null;
        setOwn(notes, k, v.trim());
      }
      loc.access_notes = notes;
    }
    return loc;
  });
}

export function displayI18nFromListing(value: unknown): AppDisplayI18n | null {
  const listingI18n = parseListingDisplayI18n(value);
  if (!listingI18n) return null;
  const out: AppDisplayI18n = {};
  for (const [tag, e] of Object.entries(listingI18n)) {
    const parsed: AppDisplayLocale = {};
    if (e.title !== undefined) parsed.title = e.title;
    if (e.summary !== undefined) parsed.summary = e.summary;
    if (e.tagline !== undefined) parsed.tagline = e.tagline;
    if (Object.keys(parsed).length > 0) setOwn(out, tag, parsed);
  }
  return Object.keys(out).length > 0 ? out : null;
}

interface LocalizedAppSource {
  title: string; summary?: string | null; tagline?: string | null;
  i18n?: AppDisplayI18n | null;
  listing?: { tagline?: string | null; i18n?: Record<string, ListingDisplayLocale> | null } | null;
}

function pickField(src: LocalizedAppSource, field: keyof AppDisplayLocale, fallback: string, locale: string, t?: TranslateFn): string {
  const top = pickLocaleEntry(parseAppDisplayI18n(src.i18n), locale)?.[field];
  if (typeof top === "string" && top.trim().length > 0) return top;
  const listed = pickLocaleEntry(parseListingDisplayI18n(src.listing?.i18n), locale)?.[field];
  if (typeof listed === "string" && listed.trim().length > 0) return listed;
  return !fallback || isEnglishLocale(locale) ? fallback : (t ? t(fallback) : fallback);
}

export const localizedAppTitle = (s: Pick<LocalizedAppSource, "title" | "i18n" | "listing">, locale: string, t?: TranslateFn): string =>
  pickField(s, "title", s.title, locale, t);
export const localizedAppSummary = (s: { summary?: string | null; i18n?: AppDisplayI18n | null; listing?: LocalizedAppSource["listing"] }, locale: string, t?: TranslateFn): string =>
  pickField({ title: "", ...s }, "summary", s.summary ?? "", locale, t);
export const localizedAppTagline = (s: { tagline?: string | null; i18n?: AppDisplayI18n | null; listing?: LocalizedAppSource["listing"] }, locale: string, t?: TranslateFn): string =>
  pickField({ title: "", ...s }, "tagline", s.tagline ?? s.listing?.tagline ?? "", locale, t);

export function localizedSectionLabel(section: { label: string; i18n?: Record<string, { label: string }> | null }, locale: string, t?: TranslateFn): string {
  const localized = pickLocaleEntry(section.i18n, locale)?.label;
  if (typeof localized === "string" && localized.trim().length > 0) return localized;
  return !section.label || isEnglishLocale(locale) ? section.label : (t ? t(section.label) : section.label);
}

export function localizedAppCategory(category: string | null | undefined, locale: string): string {
  if (!category) return "";
  const norm = locale.trim().replace(/_/g, "-").toLowerCase();
  const isZhTw = TRADITIONAL_CHINESE_TAGS.has(norm) || ["zh-tw-", "zh-hk-", "zh-mo-", "zh-hant-"].some((p) => norm.startsWith(p));
  return isZhTw ? (ZH_TW_APP_CATEGORIES[category.trim().toLowerCase()] ?? category) : category;
}
