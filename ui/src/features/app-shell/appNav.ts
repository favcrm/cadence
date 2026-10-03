import { CRM_SECTIONS, crmSectionHref, type AppMenuSection } from "./CrmOutlet";

/** One installed app in the board's Apps menu (CAD-1116). */
export interface AppNavEntry {
  installId: string;
  title: string;
  /** The installation itself. */
  href: string;
  /** The app on screen right now. */
  current: boolean;
  /** Section rows, shown for the active or last-used app; null when the kind declares none. */
  sections: AppMenuSection[] | null;
}

export interface AppNav {
  apps: AppNavEntry[];
}

/** An installation the host has verified: from the installation list or the shell's receipt. */
export interface VerifiedApp {
  installId: string;
  kind: string;
  title: string;
}

/** Per-kind section declarations. This table is the one place that names a
 *  kind; the CAD-811 surface declaration replaces it. Kinds without an
 *  entry (Social today) get only their app link. */
interface KindSections {
  sections: [string, string][];
  /** The section the URL query names; the default when it names none. */
  fromSearch: (search: string) => string;
  href: (base: string, section: string) => string;
}

const SECTIONS_BY_KIND: Record<string, KindSections> = {
  crm: {
    sections: CRM_SECTIONS,
    fromSearch: (search) => {
      const q = new URLSearchParams(search).get("crm");
      return q === "segments" || q === "campaigns" ? q : "customers";
    },
    href: (base, section) => crmSectionHref(base, section as Parameters<typeof crmSectionHref>[1]),
  },
};

/** The section the URL is on, for a kind that declares sections. */
export function sectionFromSearch(kind: string, search: string): string | null {
  return SECTIONS_BY_KIND[kind]?.fromSearch(search) ?? null;
}

export interface LastApp {
  installId: string;
  section: string | null;
}

const LAST_APP_KEY = "cadence.apps.last";

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
      const decl = SECTIONS_BY_KIND[app.kind];
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
