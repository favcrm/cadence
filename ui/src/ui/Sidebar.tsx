import { useRef, useState, type FocusEvent, type MouseEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";
import type { ResourceState } from "../lib/cache";
import type { AppNav } from "../features/app-shell/appNav";
import type { Route, Screen } from "../lib/router";
import type { IssueCard, Project } from "../lib/types";
import { Logo } from "./Logo";
import { NavList, ProjectsSection } from "./NavList";
import { useLocale } from "../lib/locale";
import { updateSidebarPrefs, useSidebarPrefs } from "../features/app-shell/sidebarPrefs";
import { IconCaret } from "./icons";

interface Props {
  screen: Screen;
  /** The href of a main-nav route (scope and drawer carried along). */
  navHref: (route: Route) => string;
  /** The project page on screen, or null when this screen is not one. */
  project: string | null;
  /** Opens that project's page. Never filters the current screen. */
  projectHref: (key: string) => string;
  projects: Project[];
  /** Counts derive from the cards (counts.ts), not `/api/projects`. */
  issues: ResourceState<IssueCard[]>;
  /** Set when the project list never loaded. */
  projectsError?: string | null;
  /** CAD-313: this browser's operator session — null while unknown. */
  signedIn?: boolean | null;
  /** The signed-in account row (an AccountMenu trigger="row"); App owns its data. */
  account?: ReactNode;
  /** The account dialog is open: raise the aside over the page's own layers, only then. */
  accountOpen?: boolean;
  /** Nested submenu for the verified active installation, or null
   *  when no installation workspace is on screen. Host-owned App
   *  menu (CAD-784): sections arrive as data with real hrefs — the
   *  sidebar never invents entries from the bare route. */
  appMenu?: AppNav | null;
  /** Pending To do items, for the Home row's badge. */
  homeCount?: number;
  /** The board does not serve the wiki (see wikiAvailable); do not advertise it there. */
  noWiki?: boolean;
}

export default function Sidebar({ screen, navHref, project, projectHref, projects, issues, projectsError, signedIn = null, account, accountOpen = false, appMenu = null, homeCount = 0, noWiki = false }: Props) {
  const { t } = useLocale();
  const { collapsed } = useSidebarPrefs();
  const logoRef = useRef<HTMLButtonElement>(null);
  const collapseRef = useRef<HTMLButtonElement>(null);
  const [tip, setTip] = useState<{ text: string; top: number; left: number } | null>(null);
  const showTip = (target: EventTarget | null) => {
    if (!collapsed || !(target instanceof Element)) return;
    const link = target.closest<HTMLElement>("[data-rail-label]");
    if (!link) return;
    const rect = link.getBoundingClientRect();
    setTip({ text: link.dataset.railLabel ?? "", top: rect.top + rect.height / 2, left: rect.right + 10 });
  };
  const hideTip = (event: MouseEvent | FocusEvent) => {
    if (!(event.relatedTarget instanceof Element) || !event.relatedTarget.closest("[data-rail-label]")) setTip(null);
  };
  const toggle = (next: boolean) => {
    setTip(null);
    updateSidebarPrefs((prefs) => ({ ...prefs, collapsed: next }));
    requestAnimationFrame(() => (next ? logoRef : collapseRef).current?.focus());
  };
  return (
    <aside data-account-open={accountOpen || undefined} data-sidebar-rail={collapsed || undefined}
      className={`hidden lg:flex sticky top-0 ${accountOpen ? "z-[35]" : ""} h-screen flex-col border-r border-ink-700 bg-ink-875 px-[14px] pt-[16px] pb-1`}
      onMouseOver={(event) => showTip(event.target)} onMouseOut={hideTip}
      onFocus={(event) => showTip(event.target)} onBlur={hideTip}>
      <div className="min-h-0 flex-1 overflow-y-auto" onScroll={() => setTip(null)}>
        <div className="sidebar-brand">
          {collapsed ? (
            <button ref={logoRef} type="button" className="sidebar-logo-button" aria-label={t("Expand sidebar")} aria-expanded="false" onClick={() => toggle(false)}>
              <Logo size={25} />
            </button>
          ) : (
            <>
              <span className="inline-flex items-center gap-2 text-ink-100 text-[17px] font-semibold tracking-[-.035em]">
                <Logo size={25} /><span>cadence<span className="text-ink-400 font-normal"> board</span></span>
              </span>
              <button ref={collapseRef} type="button" className="sidebar-collapse" aria-label={t("Collapse sidebar")} aria-expanded="true" onClick={() => toggle(true)}><IconCaret size={14} /></button>
            </>
          )}
        </div>
        <NavList screen={screen} navHref={navHref} appMenu={appMenu} homeCount={homeCount} noWiki={noWiki} rail={collapsed} />
        <div hidden={collapsed}><ProjectsSection project={project} projectHref={projectHref} projects={projects} issues={issues} projectsError={projectsError} /></div>
      </div>
      <div data-sidebar-footer className="pt-1 border-t border-ink-700 text-label text-ink-500">
        {signedIn === true ? account : (
          <div className="px-2 leading-9">{signedIn === false ? t("not signed in · read only") : "…"}</div>
        )}
      </div>
      {tip && createPortal(<span className="sidebar-tooltip" role="tooltip" style={{ top: tip.top, left: tip.left }}>{tip.text}</span>, document.body)}
    </aside>
  );
}
