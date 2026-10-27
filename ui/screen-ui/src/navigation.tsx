import { createContext, useContext, type ReactNode } from "react";
import type { ButtonClickLike } from "../../src/ui/buttonActivate";
import { activateButtonHref } from "../../src/ui/buttonActivate";

export type NavigationOptions = { replace?: boolean };

/** Supplied by the embedding application; this module owns no router or session. */
export interface NavigationAdapter {
  navigate: (href: string, options?: NavigationOptions) => void;
  /** Visible/copyable URL. Callback-only frames use "#" to retain anchor keyboard semantics. */
  href: (href: string) => string;
  /** Only a host with usable URLs should permit browser modified-click navigation. */
  modifiedClicks: "browser" | "prevent";
}

const NavigationContext = createContext<NavigationAdapter | null>(null);

export function NavigationProvider({ adapter, children }: { adapter: NavigationAdapter; children: ReactNode }) {
  return <NavigationContext.Provider value={adapter}>{children}</NavigationContext.Provider>;
}

export function useNavigation(): NavigationAdapter | null {
  return useContext(NavigationContext);
}

export function activateNavigation(
  adapter: NavigationAdapter | null,
  href: string,
  event: ButtonClickLike,
  options?: NavigationOptions,
): void {
  if (event.defaultPrevented) return;
  if (adapter === null || (adapter.modifiedClicks === "prevent" &&
    (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey))) {
    event.preventDefault();
    return;
  }
  activateButtonHref(href, event, adapter.navigate, options);
}
