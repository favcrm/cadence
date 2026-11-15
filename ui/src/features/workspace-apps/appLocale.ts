export interface AppDisplayLocale {
  title?: string;
  summary?: string;
  tagline?: string;
}

export type AppDisplayI18n = Record<string, AppDisplayLocale>;

export interface ListingDisplayLocale extends AppDisplayLocale {
  about?: string;
  can?: string[];
  access_notes?: Record<string, string>;
}

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
  marketing: "行銷",
  customers: "客戶",
  operations: "營運",
  finance: "財務",
  content: "內容",
  other: "其他",
};

export function isPlainJsonObject(value: unknown): value is Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return false;
  const proto = Object.getPrototypeOf(value);
  if (proto !== Object.prototype && proto !== null) return false;
  if (Object.getOwnPropertySymbols(value).length > 0) return false;
  const descs = Object.getOwnPropertyDescriptors(value);
  for (const key of Object.keys(descs)) {
    if (FORBIDDEN_PLAIN_OBJECT_KEYS.has(key)) return false;
    const desc = descs[key];
    if (!desc || typeof desc.get === "function" || typeof desc.set === "function") return false;
  }
  return true;
}

function setOwnDataProperty<T>(target: Record<string, T>, key: string, value: T): void {
  Object.defineProperty(target, key, {
    value,
    enumerable: true,
    configurable: true,
    writable: true,
  });
}

export function validLocaleTag(tag: string): boolean {
  return tag.length >= 2 && tag.length <= 20 && LOCALE_TAG_RE.test(tag);
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

/**
 * Ordered locale lookup tags for display resolution.
 * Traditional Chinese alternates (`zh-TW` <-> `zh-HK` <-> `zh-Hant`) never
 * fall back to Simplified Chinese (`zh`, `zh-CN`, `zh-Hans`).
 */
export function localeCandidates(locale: string): string[] {
  const raw = locale.trim().replace(/_/g, "-");
  if (!raw) return [];
  const lower = raw.toLowerCase();
  const seen = new Set<string>();
  const out: string[] = [];
  const push = (tag: string) => {
    const key = tag.toLowerCase();
    if (!seen.has(key)) {
      seen.add(key);
      out.push(tag);
    }
  };

  push(raw);
  if (TRADITIONAL_CHINESE_TAGS.has(lower) || lower.startsWith("zh-hant-")) {
    push("zh-TW");
    push("zh-HK");
    push("zh-Hant");
    return out;
  }

  const base = lower.split("-")[0];
  if (base && base !== "zh") {
    push(base);
  }
  return out;
}

export function pickLocaleEntry<T>(
  map: Record<string, T> | null | undefined,
  locale: string,
): T | undefined {
  if (!map || typeof map !== "object" || Array.isArray(map)) return undefined;
  const byLower = new Map<string, T>();
  for (const [key, val] of Object.entries(map)) {
    byLower.set(key.toLowerCase(), val);
  }
  for (const candidate of localeCandidates(locale)) {
    const found = byLower.get(candidate.toLowerCase());
    if (found !== undefined) return found;
  }
  return undefined;
}

const DISPLAY_I18N_KEYS = new Set(["title", "summary", "tagline"]);
const LISTING_DISPLAY_I18N_KEYS = new Set(["title", "summary", "tagline", "about", "can", "access_notes"]);

export function parseAppDisplayI18n(value: unknown): AppDisplayI18n | null {
  if (!isPlainJsonObject(value)) return null;
  const entries = Object.entries(value);
  if (entries.length === 0 || entries.length > MAX_I18N_LOCALES) return null;
  const out: AppDisplayI18n = {};
  const seenLocales = new Set<string>();
  for (const [tag, rawEntry] of entries) {
    if (!validLocaleTag(tag)) return null;
    const lowerTag = tag.toLowerCase();
    if (seenLocales.has(lowerTag)) return null;
    seenLocales.add(lowerTag);
    if (!isPlainJsonObject(rawEntry)) return null;
    const objKeys = Object.keys(rawEntry);
    if (objKeys.length === 0 || objKeys.some((key) => !DISPLAY_I18N_KEYS.has(key))) return null;
    const parsed: AppDisplayLocale = {};
    if (rawEntry.title !== undefined) {
      if (!isPlainTextValue(rawEntry.title, 80)) return null;
      parsed.title = rawEntry.title.trim();
    }
    if (rawEntry.summary !== undefined) {
      if (!isPlainTextValue(rawEntry.summary, 160)) return null;
      parsed.summary = rawEntry.summary.trim();
    }
    if (rawEntry.tagline !== undefined) {
      if (!isPlainTextValue(rawEntry.tagline, 80)) return null;
      parsed.tagline = rawEntry.tagline.trim();
    }
    if (Object.keys(parsed).length === 0) return null;
    setOwnDataProperty(out, tag, parsed);
  }
  return out;
}

export function parseListingDisplayI18n(value: unknown): AppListingI18n | null {
  if (!isPlainJsonObject(value)) return null;
  const entries = Object.entries(value);
  if (entries.length === 0 || entries.length > MAX_I18N_LOCALES) return null;
  const out: AppListingI18n = {};
  const seenLocales = new Set<string>();
  for (const [tag, rawEntry] of entries) {
    if (!validLocaleTag(tag)) return null;
    const lowerTag = tag.toLowerCase();
    if (seenLocales.has(lowerTag)) return null;
    seenLocales.add(lowerTag);
    if (!isPlainJsonObject(rawEntry)) return null;
    const objKeys = Object.keys(rawEntry);
    if (objKeys.length === 0 || objKeys.some((key) => !LISTING_DISPLAY_I18N_KEYS.has(key))) return null;
    const loc: AppListingLocale = {};
    if (rawEntry.title !== undefined) {
      if (!isPlainTextValue(rawEntry.title, 80)) return null;
      loc.title = rawEntry.title.trim();
    }
    if (rawEntry.summary !== undefined) {
      if (!isPlainTextValue(rawEntry.summary, 160)) return null;
      loc.summary = rawEntry.summary.trim();
    }
    if (rawEntry.tagline !== undefined) {
      if (!isPlainTextValue(rawEntry.tagline, 80)) return null;
      loc.tagline = rawEntry.tagline.trim();
    }
    if (rawEntry.about !== undefined) {
      if (!isPlainTextValue(rawEntry.about, 600, true)) return null;
      loc.about = rawEntry.about.trim();
    }
    if (rawEntry.can !== undefined) {
      if (!Array.isArray(rawEntry.can) || rawEntry.can.length === 0 || rawEntry.can.length > 5) return null;
      const can: string[] = [];
      for (const item of rawEntry.can) {
        if (!isPlainTextValue(item, 100)) return null;
        can.push(item.trim());
      }
      loc.can = can;
    }
    if (rawEntry.access_notes !== undefined) {
      if (!isPlainJsonObject(rawEntry.access_notes)) return null;
      const noteEntries = Object.entries(rawEntry.access_notes);
      if (noteEntries.length === 0 || noteEntries.length > 16) return null;
      const notes: Record<string, string> = {};
      for (const [k, v] of noteEntries) {
        if (FORBIDDEN_PLAIN_OBJECT_KEYS.has(k) || !SLOT_KEY_RE.test(k) || !isPlainTextValue(v, 140)) {
          return null;
        }
        setOwnDataProperty(notes, k, v.trim());
      }
      loc.access_notes = notes;
    }
    if (Object.keys(loc).length === 0) return null;
    setOwnDataProperty(out, tag, loc);
  }
  return out;
}

export function displayI18nFromListing(value: unknown): AppDisplayI18n | null {
  const listingI18n = parseListingDisplayI18n(value);
  if (!listingI18n) return null;
  const out: AppDisplayI18n = {};
  for (const [tag, entry] of Object.entries(listingI18n)) {
    const parsed: AppDisplayLocale = {};
    if (entry.title !== undefined) parsed.title = entry.title;
    if (entry.summary !== undefined) parsed.summary = entry.summary;
    if (entry.tagline !== undefined) parsed.tagline = entry.tagline;
    if (Object.keys(parsed).length > 0) {
      setOwnDataProperty(out, tag, parsed);
    }
  }
  return Object.keys(out).length > 0 ? out : null;
}

interface LocalizedAppSource {
  title: string;
  summary?: string | null;
  tagline?: string | null;
  i18n?: AppDisplayI18n | null;
  listing?: {
    tagline?: string | null;
    i18n?: Record<string, ListingDisplayLocale> | null;
  } | null;
}

function pickField(
  source: LocalizedAppSource,
  field: keyof AppDisplayLocale,
  fallback: string,
  locale: string,
  t?: TranslateFn,
): string {
  const validTop = parseAppDisplayI18n(source.i18n);
  const top = pickLocaleEntry(validTop, locale)?.[field];
  if (typeof top === "string" && top.trim().length > 0) return top;
  const validListing = parseListingDisplayI18n(source.listing?.i18n);
  const listed = pickLocaleEntry(validListing, locale)?.[field];
  if (typeof listed === "string" && listed.trim().length > 0) return listed;
  if (!fallback || isEnglishLocale(locale)) return fallback;
  return t ? t(fallback) : fallback;
}

export function localizedAppTitle(
  source: Pick<LocalizedAppSource, "title" | "i18n" | "listing">,
  locale: string,
  t?: TranslateFn,
): string {
  return pickField(source, "title", source.title, locale, t);
}

export function localizedAppSummary(
  source: { summary?: string | null; i18n?: AppDisplayI18n | null; listing?: LocalizedAppSource["listing"] },
  locale: string,
  t?: TranslateFn,
): string {
  const base = source.summary ?? "";
  return pickField({ title: "", ...source }, "summary", base, locale, t);
}

export function localizedAppTagline(
  source: { tagline?: string | null; i18n?: AppDisplayI18n | null; listing?: LocalizedAppSource["listing"] },
  locale: string,
  t?: TranslateFn,
): string {
  const base = source.tagline ?? source.listing?.tagline ?? "";
  return pickField({ title: "", ...source }, "tagline", base, locale, t);
}

export function localizedSectionLabel(
  section: { label: string; i18n?: Record<string, { label: string }> | null },
  locale: string,
  t?: TranslateFn,
): string {
  const localized = pickLocaleEntry(section.i18n, locale)?.label;
  if (typeof localized === "string" && localized.trim().length > 0) return localized;
  if (!section.label || isEnglishLocale(locale)) return section.label;
  return t ? t(section.label) : section.label;
}

export function localizedAppCategory(
  category: string | null | undefined,
  locale: string,
): string {
  if (!category) return "";
  const norm = locale.trim().replace(/_/g, "-").toLowerCase();
  const isZhTw =
    TRADITIONAL_CHINESE_TAGS.has(norm) ||
    norm.startsWith("zh-tw-") ||
    norm.startsWith("zh-hk-") ||
    norm.startsWith("zh-mo-") ||
    norm.startsWith("zh-hant-");
  if (!isZhTw) return category;
  return ZH_TW_APP_CATEGORIES[category.trim().toLowerCase()] ?? category;
}
