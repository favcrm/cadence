import { onPrivateSessionReset } from "../../lib/privateSession";
import type { ScreenNavigation } from "../workspace-apps/screen/screenProtocol";

/** One installed app in the board's Apps menu (CAD-1116). */
export interface AppNavEntry {
  installId: string;
  title: string;
  /** The installation itself. */
  href: string;
  /** The app on screen right now. */
  current: boolean;
  /** Section rows, shown for the active or last-used app; null when its screen declares none. */
  sections: AppMenuSection[] | null;
}

/** One section row under an app in the Apps menu. */
export interface AppMenuSection {
  label: string;
  href: string;
  current: boolean;
}

/** Shown under Apps when the installed-app list could not be refreshed. */
export interface AppNavNotice {
  text: string;
  /** A busy daemon is being retried automatically; the manual retry follows if that fails. */
  retrying: boolean;
  onRetry: () => void;
}

export interface AppNav {
  apps: AppNavEntry[];
  notice?: AppNavNotice | null;
}

/** An installation the host has verified: from the installation list or the shell's receipt. */
export interface VerifiedApp {
  installId: string;
  kind: string;
  title: string;
  /** Consumed-mount declaration, pinned to verified package bytes by the host. */
  navigation?: ScreenNavigation | null;
}

/** Section rows for an app whose mounted screen declares navigation. */
interface KindSections {
  sections: [string, string][];
  /** The section the URL query names; the default when it names none. */
  fromSearch: (search: string) => string;
  href: (base: string, section: string) => string;
}

/** The section the URL is on, for a screen that declares sections. */
function declaredSections(navigation: ScreenNavigation): KindSections {
  return {
    sections: navigation.sections.map(s => [s.id, s.label]),
    fromSearch: search => {
      const selected = new URLSearchParams(search).get(navigation.section_query);
      return navigation.sections.find(s => s.id === selected)?.id ?? navigation.default_section;
    },
    href: (base, section) => {
      const [path, search] = base.split("?");
      const q = new URLSearchParams(search ?? "");
      if (section === navigation.default_section) q.delete(navigation.section_query); else q.set(navigation.section_query, section);
      q.delete("appview"); q.delete("record"); q.delete("segment");
      const next = q.toString(); return path + (next ? `?${next}` : "");
    },
  };
}
export function sectionFromSearch(search: string, navigation?: ScreenNavigation | null): string | null {
  return navigation ? declaredSections(navigation).fromSearch(search) : null;
}

export interface LastApp {
  installId: string;
  section: string | null;
}

const LAST_APP_KEY = "cadence.apps.last";

onPrivateSessionReset(() => {
  try { safeStorage()?.removeItem(LAST_APP_KEY); }
  catch { /* Storage may be blocked; the next private shell starts without app state. */ }
});

/** Per-browser memory of the last-used app and its section. Storage that is
 *  missing or throws means no memory, never an error. */
export function readLastApp(storage: Pick<Storage, "getItem"> | null = safeStorage()): LastApp | null {
  try {
    const raw = storage?.getItem(LAST_APP_KEY);
    if (!raw) return null;
    const v = JSON.parse(raw) as Partial<LastApp>;
    if (typeof v.installId !== "string" || v.installId === "") return null;
    return { installId: v.installId, section: typeof v.section === "string" ? v.section : null };
  } catch {
    return null;
  }
}

export function writeLastApp(last: LastApp, storage: Pick<Storage, "setItem"> | null = safeStorage()): void {
  try {
    storage?.setItem(LAST_APP_KEY, JSON.stringify(last));
  } catch {
    // Private mode or blocked storage: the menu still works, it just forgets.
  }
}

function safeStorage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

/**
 * The Apps menu data. Pure. `apps` must come only from the verified
 * installation list or receipt, never the route. On an app screen the
 * active app (`activeId`) expands, and only if it is verified; elsewhere
 * the remembered app expands, if it is still installed.
 */
export function buildAppNav(input: {
  apps: VerifiedApp[];
  onAppScreen: boolean;
  activeId: string | null;
  /** Current href of the active app (context kept) and its search. */
  activeHref: string;
  last: LastApp | null;
  installHref: (installId: string) => string;
}): AppNav {
  const { apps, onAppScreen, activeId, activeHref, last, installHref } = input;
  const expandedId = onAppScreen ? activeId : last?.installId ?? null;
  return {
    apps: apps.map((app) => {
      const decl = app.navigation ? declaredSections(app.navigation) : null;
      const active = onAppScreen && app.installId === activeId;
      let sections: AppMenuSection[] | null = null;
      let href = active ? activeHref : installHref(app.installId);
      if (decl && app.installId === expandedId) {
        const base = active ? activeHref : installHref(app.installId);
        const current = active
          ? decl.fromSearch(activeHref.includes("?") ? activeHref.slice(activeHref.indexOf("?")) : "")
          : last?.section ?? decl.sections[0][0];
        sections = decl.sections.map(([key, label]) => ({ label, href: decl.href(base, key), current: current === key }));
        // The app link of a remembered app returns to its last section.
        if (!active) href = sections.find((x) => x.current)?.href ?? href;
      }
      return { installId: app.installId, title: app.title, href, current: active, sections };
    }),
  };
}
