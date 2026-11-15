import { createContext, useContext, useMemo, useSyncExternalStore, type ReactNode } from "react";
import { LocaleTextProvider, useTranslate } from "../../src/lib/localeText";

export interface ScreenLocaleBridgeSource {
  getState: () => {
    snapshot?: { presentationLocale?: string | null } | null;
    context?: { locale?: string | null } | null;
  };
  subscribe: (listener: () => void) => () => void;
}

export type LocaleCatalog = Readonly<Record<string, Readonly<Record<string, string>>>>;

export interface ScreenLocaleContextValue {
  locale: string;
  t: (text: string) => string;
}

export interface LocaleBridgeProviderProps {
  bridge?: ScreenLocaleBridgeSource;
  locale?: string;
  fallbackLocale?: string;
  messages?: LocaleCatalog;
  translate?: (text: string, locale: string) => string;
  children: ReactNode;
}

const ScreenLocaleContext = createContext<ScreenLocaleContextValue | null>(null);

const NOOP_SUBSCRIBE = () => () => {};

const TRADITIONAL_CHINESE_TAGS = new Set(["zh-tw", "zh-hk", "zh-mo", "zh-hant"]);
const SIMPLIFIED_CHINESE_TAGS = new Set(["zh-cn", "zh-sg", "zh-my", "zh-hans"]);
const TRADITIONAL_PREFIXES = ["zh-hant-", "zh-tw-", "zh-hk-", "zh-mo-"];
const SIMPLIFIED_PREFIXES = ["zh-hans-", "zh-cn-", "zh-sg-", "zh-my-"];

function catalogLocaleCandidates(locale: string): string[] {
  const lower = locale.trim().replace(/_/g, "-").toLowerCase();
  const parts = lower.split("-").filter(Boolean);
  if (parts.length === 0) return [];
  const seen = new Set<string>();
  const out: string[] = [];
  const push = (tag: string) => {
    if (!tag || seen.has(tag)) return;
    seen.add(tag);
    out.push(tag);
  };
  push(lower);
  push(parts.join("-"));
  if (parts[0] === "zh") {
    // Never collapse zh-* down to bare "zh", keeping Traditional and Simplified isolated.
    for (let len = parts.length - 1; len >= 2; len -= 1) {
      push(parts.slice(0, len).join("-"));
    }
    if (TRADITIONAL_CHINESE_TAGS.has(lower) || TRADITIONAL_PREFIXES.some((p) => lower.startsWith(p))) {
      push("zh-hant");
      push("zh-tw");
      push("zh-hk");
      push("zh-mo");
    } else if (SIMPLIFIED_CHINESE_TAGS.has(lower) || SIMPLIFIED_PREFIXES.some((p) => lower.startsWith(p))) {
      push("zh-hans");
      push("zh-cn");
      push("zh-sg");
      push("zh-my");
    }
    return out;
  }
  for (let len = parts.length - 1; len >= 1; len -= 1) {
    push(parts.slice(0, len).join("-"));
  }
  return out;
}

function indexCatalog(
  messages: LocaleCatalog | undefined,
): Map<string, Readonly<Record<string, string>>> | null {
  if (!messages) return null;
  const indexed = new Map<string, Readonly<Record<string, string>>>();
  for (const [rawTag, dict] of Object.entries(messages)) {
    if (!dict || typeof dict !== "object") continue;
    const key = rawTag.trim().replace(/_/g, "-").toLowerCase();
    if (key && !indexed.has(key)) indexed.set(key, dict);
  }
  return indexed;
}

function lookupIndexedCatalog(
  indexed: Map<string, Readonly<Record<string, string>>> | null,
  locale: string,
  fallbackLocale: string,
  text: string,
): string {
  if (!indexed) return text;
  const candidates = [...catalogLocaleCandidates(locale), ...catalogLocaleCandidates(fallbackLocale)];
  for (const candidate of candidates) {
    const dict = indexed.get(candidate);
    if (dict && Object.prototype.hasOwnProperty.call(dict, text) && typeof dict[text] === "string") {
      return dict[text];
    }
  }
  return text;
}

/** Syncs screen bridge locale into `LocaleTextProvider` for standalone frame apps. */
export function LocaleBridgeProvider({
  bridge,
  locale,
  fallbackLocale = "en",
  messages,
  translate,
  children,
}: LocaleBridgeProviderProps) {
  const bridgedLocale = useSyncExternalStore(
    bridge ? bridge.subscribe : NOOP_SUBSCRIBE,
    () =>
      bridge?.getState().snapshot?.presentationLocale ??
      bridge?.getState().context?.locale ??
      null,
    () =>
      bridge?.getState().snapshot?.presentationLocale ??
      bridge?.getState().context?.locale ??
      null,
  );
  const resolvedLocale =
    (typeof locale === "string" && locale.trim()) ||
    (typeof bridgedLocale === "string" && bridgedLocale.trim()) ||
    fallbackLocale;

  const indexedMessages = useMemo(() => indexCatalog(messages), [messages]);

  const t = useMemo(
    () => (text: string) =>
      translate
        ? translate(text, resolvedLocale)
        : lookupIndexedCatalog(indexedMessages, resolvedLocale, fallbackLocale, text),
    [translate, indexedMessages, resolvedLocale, fallbackLocale],
  );

  const contextValue = useMemo(
    () => ({ locale: resolvedLocale, t }),
    [resolvedLocale, t],
  );

  return (
    <ScreenLocaleContext.Provider value={contextValue}>
      <LocaleTextProvider value={t}>{children}</LocaleTextProvider>
    </ScreenLocaleContext.Provider>
  );
}

/** Reads the active screen locale and translator from `LocaleBridgeProvider`. */
export function useScreenLocale(): ScreenLocaleContextValue {
  const ctx = useContext(ScreenLocaleContext);
  const fallbackT = useTranslate();
  return useMemo(
    () => ctx ?? { locale: "en", t: fallbackT },
    [ctx, fallbackT],
  );
}
