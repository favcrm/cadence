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

function lookupCatalog(
  messages: LocaleCatalog | undefined,
  locale: string,
  fallbackLocale: string,
  text: string,
): string {
  if (!messages) return text;
  const direct = messages[locale]?.[text];
  if (direct !== undefined) return direct;
  const normalized = locale.replace(/_/g, "-");
  if (normalized !== locale) {
    const normalizedDirect = messages[normalized]?.[text];
    if (normalizedDirect !== undefined) return normalizedDirect;
  }
  const parts = normalized.split("-").filter(Boolean);
  for (let len = parts.length - 1; len >= 1; len -= 1) {
    const prefix = parts.slice(0, len).join("-");
    const match = messages[prefix]?.[text];
    if (match !== undefined) return match;
  }
  const fallback = messages[fallbackLocale]?.[text];
  return fallback !== undefined ? fallback : text;
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

  const t = useMemo(
    () => (text: string) =>
      translate
        ? translate(text, resolvedLocale)
        : lookupCatalog(messages, resolvedLocale, fallbackLocale, text),
    [translate, messages, resolvedLocale, fallbackLocale],
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
