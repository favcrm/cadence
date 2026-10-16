import { createContext, useContext } from "react";

const LocaleTextContext = createContext<((english: string) => string) | null>(null);
export const LocaleTextProvider = LocaleTextContext.Provider;

export function useTranslate(): (english: string) => string {
  const value = useContext(LocaleTextContext);
  return value ?? ((english) => english);
}
