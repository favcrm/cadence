import type { ResourceState } from "../lib/cache";
import type { AppMenu } from "../features/app-shell/CrmOutlet";
import type { Route, Screen } from "../lib/router";
import type { IssueCard, Project, Meta } from "../lib/types";
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
  sessionUser?: NonNullable<Meta["session"]>["user"];
  /** Nested submenu for the verified active installation, or null
   *  when no installation workspace is on screen. Host-owned App
   *  menu (CAD-784): sections arrive as data with real hrefs — the
   *  sidebar never invents entries from the bare route. */
  appMenu?: AppMenu | null;
}

export default function Sidebar({ screen, navHref, project, projectHref, projects, issues, projectsError, signedIn = null, sessionUser, appMenu = null }: Props) {
  return (
    <aside className="hidden lg:flex sticky top-0 h-screen flex-col border-r border-ink-700 bg-ink-875 px-[14px] pt-[16px] pb-4 overflow-y-auto">
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
      <div className="mt-auto pt-4 border-t border-ink-700 mx-1 text-ink-500 text-label leading-relaxed">
        <div className="slabel mb-1">session</div>
        {signedIn === true ? (
          <div title={sessionUser?.email}>
            <div className="truncate text-ink-300">{sessionUser?.name || sessionUser?.email || "operator"}</div>
            {sessionUser && <div className="truncate text-micro">{sessionUser.email}</div>}
            <div>{sessionUser?.role || "operator"} · signed in</div>
          </div>
        ) : signedIn === false ? "not signed in · read only" : "…"}
        <br />
        <span className="num text-micro">{location.host}</span>
      </div>
    </aside>
  );
}
