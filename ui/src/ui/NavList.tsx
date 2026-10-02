import { Fragment, type ReactNode } from "react";
import { countLabel, issueCounts } from "../lib/counts";
import type { ResourceState } from "../lib/cache";
import type { AppMenu } from "../features/app-shell/CrmOutlet";
import { navMatches } from "../features/issues/model";
import { NAV, type Route, type Screen } from "../lib/router";
import type { IssueCard, Project } from "../lib/types";
import Link from "./Link";
import { IconAgents, IconApps, IconHome, IconOutbox, IconProjects, IconSettings, IconWiki } from "./icons";

const NAV_ICONS: Record<string, ReactNode> = {
  home: <IconHome size={16} />,
  projects: <IconProjects size={16} />,
  wiki: <IconWiki size={16} />,
  apps: <IconApps size={16} />,
  agents: <IconAgents size={16} />,
  outbox: <IconOutbox size={16} />,
  settings: <IconSettings size={16} />,
};

interface NavListProps {
  screen: Screen;
  /** The href of a main-nav route (scope and drawer carried along). */
  navHref: (route: Route) => string;
  /** Host-owned App menu (CAD-784), or null off an installation. */
  appMenu?: AppMenu | null;
  /** Landmark name; the phone menu is "Workspace". */
  label?: string;
  /** The phone menu closes itself when a link is followed. */
  onNavigate?: () => void;
}

/** The one nav row list: the desktop sidebar and the phone menu both
 *  render it, so there is a single row style. */
export function NavList({ screen, navHref, appMenu = null, label = "Primary", onNavigate }: NavListProps) {
  return (
    <nav className="grid gap-[2px]" aria-label={label}>
      {NAV.map((item) => {
        const here = navMatches(screen, item.screen);
        const parent = item.screen === "apps" && appMenu !== null;
        return (
          <Fragment key={item.screen}>
            <Link
              href={navHref(item.route)}
              onClick={onNavigate}
              className="navlink"
              // With an app menu open the sub-item carries the accent; the
              // parent stays current for assistive tech only.
              aria-current={here ? (parent ? "true" : "page") : undefined}
              data-parent-current={here && parent ? "" : undefined}
            >
              {NAV_ICONS[item.screen]}
              {item.label}
            </Link>
            {parent && (
              <div className="mt-[2px] mb-[2px]">
                <nav aria-label={`${appMenu.title} sections`} className="grid gap-[1px] ml-[10px] border-l border-ink-700 pl-[6px]">
                  <div className="text-micro text-ink-500 px-2 pt-0.5 pb-1 truncate" title={appMenu.title}>{appMenu.title}</div>
                  {appMenu.sections.map((s) => (
                    <Link
                      key={s.label}
                      href={s.href}
                      onClick={onNavigate}
                      className="navlink navlink-sub"
                      aria-current={s.current ? "page" : undefined}
                    >
                      {s.label}
                    </Link>
                  ))}
                </nav>
              </div>
            )}
          </Fragment>
        );
      })}
    </nav>
  );
}

interface ProjectListProps {
  /** The project page on screen, or null when this screen is not one. */
  project: string | null;
  projectHref: (key: string) => string;
  projects: Project[];
  /** Counts derive from the cards (counts.ts), not `/api/projects`. */
  issues: ResourceState<IssueCard[]>;
  projectsError?: string | null;
  onNavigate?: () => void;
}

/** Project rows. With no projects (and no load error) the "All
 *  projects" row gives way to a plain line. */
export function ProjectList({ project, projectHref, projects, issues, projectsError, onNavigate }: ProjectListProps) {
  // "…" until the cards load; a stale list keeps its numbers.
  const count = (key: string) => {
    if (!issues.data) return { n: issues.status === "failed" ? "!" : "…", title: issues.error ?? "loading issues" };
    const c = issueCounts(issues.data, key);
    return { n: String(c.open), title: countLabel(c) };
  };
  const all = count("all");
  return (
    <div className="grid gap-[2px]">
      {projects.length === 0 && !projectsError ? (
        <span className="mx-[10px] py-1 text-micro text-ink-500">No projects yet</span>
      ) : (
        <Link href={projectHref("all")} onClick={onNavigate} className={`proj ${project === "all" ? "on" : ""}`}>
          <span className="truncate">All projects</span>
          <span className="num text-micro text-ink-500" title={all.title}>{all.n}</span>
        </Link>
      )}
      {projects.map((p) => (
        <Link
          key={p.key}
          href={projectHref(p.key)}
          onClick={onNavigate}
          className={`proj ${project === p.key ? "on" : ""}`}
        >
          <span className="truncate">{p.key}</span>
          <span className="num text-micro text-ink-500" title={count(p.key).title}>
            {p.prefix} {count(p.key).n}
          </span>
        </Link>
      ))}
      {projectsError && (
        <span className="mx-[11px] mt-1 text-micro text-fail" role="alert" title={projectsError}>
          could not load projects
        </span>
      )}
    </div>
  );
}
