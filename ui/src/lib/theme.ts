/**
 * The colour theme: dark by default, unless the operator picked one.
 * A pick is stored per browser and applied as `data-theme` on <html>,
 * which styles.css reads; "system" is stored explicitly so the media
 * query decides instead of the dark fallback. Storage can throw
 * (private windows, locked-down web views) — then the pick lasts only
 * for the page.
 */

export type ThemePref = "system" | "light" | "dark";

const THEME_KEY = "cadence-theme";

export function readThemePref(storage: Pick<Storage, "getItem"> | undefined): ThemePref {
  try {
    const value = storage?.getItem(THEME_KEY);
    return value === "light" || value === "dark" || value === "system" ? value : "dark";
  } catch {
    return "dark";
  }
}

export function writeThemePref(
  pref: ThemePref,
  storage: Pick<Storage, "setItem" | "removeItem"> | undefined,
): void {
  try {
    // "system" is a real pick, not an absence: it is stored explicitly so
    // the next load does not conflate it with the dark default.
    storage?.setItem(THEME_KEY, pref);
  } catch {
    // Unstorable: the theme still applies until the page reloads.
  }
}

/** system → light → dark → system. */
export function nextThemePref(pref: ThemePref): ThemePref {
  return pref === "system" ? "light" : pref === "light" ? "dark" : "system";
}

function browserStorage(): Storage | undefined {
  try {
    return typeof window === "undefined" ? undefined : window.localStorage;
  } catch {
    return undefined;
  }
}

export function applyThemePref(pref: ThemePref, root: HTMLElement = document.documentElement): void {
  if (pref === "system") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", pref);
}

/** The stored pick, applied — called once before the first render. */
export function initTheme(): ThemePref {
  const pref = readThemePref(browserStorage());
  applyThemePref(pref);
  return pref;
}

export function setThemePref(pref: ThemePref): void {
  writeThemePref(pref, browserStorage());
  applyThemePref(pref);
}
