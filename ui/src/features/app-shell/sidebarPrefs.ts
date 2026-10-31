import { useSyncExternalStore } from "react";
import { onPrivateSessionReset } from "../../lib/privateSession";

/** Per-browser sidebar layout (CAD-1692): what is folded and which
 *  projects are pinned. The desktop sidebar and the phone menu share it. */
export interface SidebarPrefs {
  /** Desktop icon rail preference; mobile drawer visibility is separate. */
  collapsed: boolean;
  /** The Apps tree is unfolded. */
  appsOpen: boolean;
  /** Installations whose section list the user folded. */
  closedApps: string[];
  /** The Projects section is unfolded. */
  projectsOpen: boolean;
  /** Project keys pinned to the top, in pin order. */
  pinned: string[];
  /** With pins, also list the unpinned projects. */
  showAll: boolean;
}

export const SIDEBAR_PREFS_KEY = "cadence.sidebar";

export const DEFAULT_SIDEBAR_PREFS: SidebarPrefs = Object.freeze({
  collapsed: false,
  appsOpen: true,
  closedApps: [],
  projectsOpen: true,
  pinned: [],
  showAll: false,
}) as SidebarPrefs;

const flag = (v: unknown, fallback: boolean) => (typeof v === "boolean" ? v : fallback);
const ids = (v: unknown) => (Array.isArray(v) ? [...new Set(v.filter((x): x is string => typeof x === "string" && x !== ""))] : []);

/** Storage that is missing, throws or holds anything malformed means the
 *  defaults, never an error. */
export function readSidebarPrefs(storage: Pick<Storage, "getItem"> | null = safeStorage()): SidebarPrefs {
  try {
    const raw = storage?.getItem(SIDEBAR_PREFS_KEY);
    if (!raw) return DEFAULT_SIDEBAR_PREFS;
    const v = JSON.parse(raw) as Partial<Record<keyof SidebarPrefs, unknown>> | null;
    if (v === null || typeof v !== "object") return DEFAULT_SIDEBAR_PREFS;
    const d = DEFAULT_SIDEBAR_PREFS;
    return {
      collapsed: flag(v.collapsed, d.collapsed),
      appsOpen: flag(v.appsOpen, d.appsOpen),
      closedApps: ids(v.closedApps),
      projectsOpen: flag(v.projectsOpen, d.projectsOpen),
      pinned: ids(v.pinned),
      showAll: flag(v.showAll, d.showAll),
    };
  } catch {
    return DEFAULT_SIDEBAR_PREFS;
  }
}

export function writeSidebarPrefs(prefs: SidebarPrefs, storage: Pick<Storage, "setItem"> | null = safeStorage()): void {
  try {
    storage?.setItem(SIDEBAR_PREFS_KEY, JSON.stringify(prefs));
  } catch {
    // Private mode or blocked storage: the sidebar still works, it just forgets.
  }
}

let current: SidebarPrefs | null = null;
const listeners = new Set<() => void>();

function snapshot(): SidebarPrefs {
  if (current === null) current = readSidebarPrefs();
  return current;
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

export function updateSidebarPrefs(change: (prefs: SidebarPrefs) => SidebarPrefs): void {
  const next = change(snapshot());
  if (next === current) return;
  current = next;
  writeSidebarPrefs(next);
  for (const listener of [...listeners]) listener();
}

/** The board renders only in the browser, so the server snapshot is the
 *  same per-browser state (static renders in tests read it too). */
export function useSidebarPrefs(): SidebarPrefs {
  return useSyncExternalStore(subscribe, snapshot, snapshot);
}

// The app on screen as last seen: `undefined` before the first report,
// `null` off app screens, `LOADING` for the app page the board was loaded
// onto while its installation is still unresolved.
const LOADING = Symbol("loading");
let seenApp: string | null | typeof LOADING | undefined;

/** Entering an app unfolds Apps and that app's sections; loading the
 *  board straight onto an app page, staying on one app, and leaving an
 *  app never change a fold. `onAppScreen` comes from the route (known at
 *  once); `installId` is the resolved current installation, or null while
 *  the installed-app list or the app's receipt is still loading. */
export function noteAppScreen(onAppScreen: boolean, installId: string | null): void {
  if (!onAppScreen) {
    seenApp = null;
    return;
  }
  if (seenApp === undefined) {
    seenApp = installId ?? LOADING;
    return;
  }
  if (installId === null) return; // still resolving: decide when it lands
  if (seenApp === LOADING) {
    seenApp = installId;
    return;
  }
  if (installId === seenApp) return;
  seenApp = installId;
  updateSidebarPrefs((p) =>
    p.appsOpen && !p.closedApps.includes(installId)
      ? p
      : { ...p, appsOpen: true, closedApps: p.closedApps.filter((id) => id !== installId) });
}

onPrivateSessionReset(() => {
  try { safeStorage()?.removeItem(SIDEBAR_PREFS_KEY); }
  catch { /* Storage may be blocked; the next private shell starts with the defaults. */ }
  seenApp = undefined;
  current = DEFAULT_SIDEBAR_PREFS;
  for (const listener of [...listeners]) listener();
});

function safeStorage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}
