import { CRM_SECTIONS, crmSectionHref, type AppMenuSection } from "./CrmOutlet";
import { resolveLiveView, type AppViewReceipt } from "./app-views/viewReceipt";

/** One installed app in the board's Apps menu (CAD-1116). */
export interface AppNavEntry {
  installId: string;
  title: string;
  /** The installation itself. */
  href: string;
  /** The app on screen right now. */
  current: boolean;
  /** Native sections declared by the host. */
  sections: AppMenuSection[] | null;
  /** Verified table views from this installation's pinned receipt. */
  views?: AppMenuSection[] | null;
}

export interface AppNav {
  apps: AppNavEntry[];
}

/** An installation the host has verified: from the installation list or the shell's receipt. */
export interface VerifiedApp {
  installId: string;
  kind: string;
  title: string;
  /** Parsed descriptor, binding and their digest pins from one verified receipt. */
  viewReceipt?: AppViewReceipt | null;
}

/** Per-kind section declarations. This table is the one place that names a
 *  kind; descriptor-driven views are a separate, app-agnostic layer. */
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

function liveTableViews(receipt: AppViewReceipt | null | undefined): { id: string; title: string }[] {
  if (!receipt || receipt.error || !receipt.descriptor || !receipt.binding
      || !receipt.descriptorDigest || !receipt.bindingDigest) return [];
  return receipt.descriptor.views.flatMap((view) => {
    if (view.kind !== "table" || !resolveLiveView(receipt, view.id, null).ok) return [];
    const binding = receipt.binding!.bindings.find((item) => item.view === view.id);
    if (!binding?.ops.includes("list")) return [];
    return [{ id: view.id, title: view.title }];
  });
}

function liveViewHref(base: string, viewId: string): string {
  const [path, search] = base.split("?");
  const q = new URLSearchParams(search ?? "");
  q.set("view", viewId);
  q.delete("record");
  q.delete("appview");
  q.delete("contract-preview");
  q.delete("contract-preview-view");
  const s = q.toString();
  return path + (s ? `?${s}` : "");
}

function nativeHref(base: string): string {
  const [path, search] = base.split("?");
  const q = new URLSearchParams(search ?? "");
  q.delete("view");
  q.delete("record");
  q.delete("appview");
  const s = q.toString();
  return path + (s ? `?${s}` : "");
}

function liveSectionFromSearch(app: VerifiedApp, search: string): string | null {
  const receipt = app.viewReceipt;
  if (!receipt || receipt.error || !receipt.descriptor || !receipt.binding) return null;
  const query = new URLSearchParams(search);
  const requested = query.get("view");
  if (!requested) return null;
  const result = resolveLiveView(receipt, requested, query.get("record"));
  if (!result.ok) return null;
  const table = result.route.tableView ?? result.route.view;
  return `view:${table.id}`;
}

/** The section the URL is on, including only a live view proven by this app's receipt. */
export function sectionFromApp(app: VerifiedApp, search: string): string | null {
  const live = liveSectionFromSearch(app, search);
  if (live) return live;
  return SECTIONS_BY_KIND[app.kind]?.fromSearch(search) ?? null;
}

/** Kept for native section callers that only have a kind. */
export function sectionFromSearch(kind: string, search: string): string | null {
  return SECTIONS_BY_KIND[kind]?.fromSearch(search) ?? null;
}

export interface LastApp {
  installId: string;
  /** Native section id, or `view:<verified-table-id>`. */
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

function validRememberedSection(app: VerifiedApp, section: string | null): string | null {
  if (!section) return null;
  const native = SECTIONS_BY_KIND[app.kind]?.sections.some(([id]) => id === section);
  if (native) return section;
  if (!section.startsWith("view:")) return null;
  const id = section.slice("view:".length);
  return liveTableViews(app.viewReceipt).some((view) => view.id === id) ? section : null;
}

/**
 * The Apps menu. Pure. `apps` must come only from the verified
 * installation list or receipt, never the route. The active or last-used
 * installation expands; its live links come only from its paired,
 * digest-pinned descriptor/binding receipt.
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
  const activeSearch = activeHref.includes("?") ? activeHref.slice(activeHref.indexOf("?")) : "";
  return {
    apps: apps.map((app) => {
      const declaration = SECTIONS_BY_KIND[app.kind];
      const active = onAppScreen && app.installId === activeId;
      const expanded = app.installId === expandedId;
      const base = active ? activeHref : installHref(app.installId);
      const sections: AppMenuSection[] | null = expanded && declaration
        ? declaration.sections.map(([key, label]) => ({
          label,
          href: declaration.href(base, key),
          current: (active ? sectionFromApp(app, activeSearch) : validRememberedSection(app, last?.section ?? null)) === key,
        }))
        : null;
      const remembered = validRememberedSection(app, last?.installId === app.installId ? last.section : null);
      const activeSection = active ? sectionFromApp(app, activeSearch) : remembered;
      const views: AppMenuSection[] | null = expanded
        ? liveTableViews(app.viewReceipt).map((view) => ({
          label: view.title,
          href: liveViewHref(base, view.id),
          current: activeSection === `view:${view.id}`,
        }))
        : null;
      let href = active ? activeHref : installHref(app.installId);
      if (active && new URLSearchParams(activeSearch).has("view")) href = nativeHref(activeHref);
      if (!active && expanded) {
        if (remembered?.startsWith("view:")) {
          const id = remembered.slice("view:".length);
          href = liveViewHref(base, id);
        } else if (sections) {
          href = sections.find((item) => item.current)?.href ?? href;
        }
      }
      return { installId: app.installId, title: app.title, href, current: active, sections, views };
    }),
  };
}
