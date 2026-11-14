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

export type TranslateFn = (key: string) => string;

export const MAX_I18N_LOCALES = 8;

const LOCALE_TAG_RE = /^[A-Za-z]{2,3}(-[A-Za-z0-9]{2,8}){0,2}$/;
const CTRL_RE = /[\u0000-\u001f\u007f\u2028\u2029]/;
const TRADITIONAL_CHINESE_TAGS = new Set(["zh-tw", "zh-hk", "zh-mo", "zh-hant"]);

export function validLocaleTag(tag: string): boolean {
  return tag.length >= 2 && tag.length <= 20 && LOCALE_TAG_RE.test(tag);
}

export function isPlainTextValue(value: unknown, max: number): value is string {
  if (typeof value !== "string") return false;
  const trimmed = value.trim();
  return trimmed.length > 0 && trimmed.length <= max && !CTRL_RE.test(trimmed);
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

export function parseAppDisplayI18n(value: unknown): AppDisplayI18n | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const entries = Object.entries(value as Record<string, unknown>);
  if (entries.length === 0 || entries.length > MAX_I18N_LOCALES) return null;
  const out: AppDisplayI18n = {};
  for (const [tag, rawEntry] of entries) {
    if (!validLocaleTag(tag)) return null;
    if (!rawEntry || typeof rawEntry !== "object" || Array.isArray(rawEntry)) return null;
    const obj = rawEntry as Record<string, unknown>;
    const parsed: AppDisplayLocale = {};
    if (obj.title !== undefined) {
      if (!isPlainTextValue(obj.title, 80)) return null;
      parsed.title = obj.title.trim();
    }
    if (obj.summary !== undefined) {
      if (!isPlainTextValue(obj.summary, 160)) return null;
      parsed.summary = obj.summary.trim();
    }
    if (obj.tagline !== undefined) {
      if (!isPlainTextValue(obj.tagline, 80)) return null;
      parsed.tagline = obj.tagline.trim();
    }
    if (parsed.title !== undefined || parsed.summary !== undefined || parsed.tagline !== undefined) {
      out[tag] = parsed;
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
  const top = pickLocaleEntry(source.i18n, locale)?.[field];
  if (typeof top === "string" && top.trim().length > 0) return top;
  const listed = pickLocaleEntry(source.listing?.i18n, locale)?.[field];
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
