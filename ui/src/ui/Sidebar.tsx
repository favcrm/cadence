import type { ReactNode } from "react";
import type { ResourceState } from "../lib/cache";
import type { AppNav } from "../features/app-shell/appNav";
import type { Route, Screen } from "../lib/router";
import type { IssueCard, Project } from "../lib/types";
import { Logo } from "./Logo";
import { NavList, ProjectList } from "./NavList";

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
}

export default function Sidebar({ screen, navHref, project, projectHref, projects, issues, projectsError, signedIn = null, account, accountOpen = false, appMenu = null }: Props) {
  return (
    <aside data-account-open={accountOpen || undefined} className={`hidden lg:flex sticky top-0 ${accountOpen ? "z-[35]" : ""} h-screen flex-col border-r border-ink-700 bg-ink-875 px-[14px] pt-[16px] pb-4`}>
      {/* Only the nav scrolls: the footer's account dialog opens above its row and must not be clipped by a scroll container. */}
      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="px-[7px] pb-[14px]">
          <span className="inline-flex items-center gap-2 text-ink-100 text-[17px] font-semibold tracking-[-.035em]">
            <Logo size={25} />
            <span>
              cadence<span className="text-ink-400 font-normal"> board</span>
            </span>
          </span>
        </div>
        <NavList screen={screen} navHref={navHref} appMenu={appMenu} />
        <div className="slabel flex justify-between mx-[11px] mt-[18px] mb-[5px]">
          <span>Projects</span>
          <span className="text-[10px]">~/pm</span>
        </div>
        <ProjectList project={project} projectHref={projectHref} projects={projects} issues={issues} projectsError={projectsError} />
      </div>
      <div data-sidebar-footer className="pt-2 border-t border-ink-700 text-label text-ink-500">
        {signedIn === true ? account : (
          <div className="px-2 py-1.5">{signedIn === false ? "not signed in · read only" : "…"}</div>
        )}
      </div>
    </aside>
  );
}
