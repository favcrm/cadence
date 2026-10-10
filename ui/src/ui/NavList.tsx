import { Fragment, type ReactNode } from "react";
import { countLabel, issueCounts } from "../lib/counts";
import type { ResourceState } from "../lib/cache";
import type { AppNav } from "../features/app-shell/appNav";
import { prefetchWorkspaceApp } from "../features/workspace-apps/workspaceApps";
import { navMatches } from "../features/issues/model";
import { NAV, type Route, type Screen } from "../lib/router";
import type { IssueCard, Project } from "../lib/types";
import Link from "./Link";
import { IconAgents, IconApps, IconHome, IconOutbox, IconProjects, IconSettings, IconWiki } from "./icons";
import { useLocale } from "../lib/locale";

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
  /** Host-owned installed-apps menu (CAD-784, CAD-1116), or null before it is known. */
  appMenu?: AppNav | null;
  /** Landmark name; the phone menu is "Workspace". */
  label?: string;
  /** The phone menu closes itself when a link is followed. */
  onNavigate?: () => void;
  /** Pending To do items for the Home row's badge (CAD-1216); hidden at 0 or unknown. */
  homeCount?: number;
}

/** The one nav row list: the desktop sidebar and the phone menu both
 *  render it, so there is a single row style. */
export function NavList({ screen, navHref, appMenu = null, label = "Primary", onNavigate, homeCount = 0 }: NavListProps) {
  const { t } = useLocale();
  return (
    <nav className="grid gap-[2px]" aria-label={t(label)}>
      {NAV.map((item) => {
        const here = navMatches(screen, item.screen);
        const apps = item.screen === "apps" && appMenu !== null ? appMenu.apps : [];
        // With an app open under Apps the most specific row carries the accent;
        // the parent stays current for assistive tech only.
        const parent = here && apps.some((a) => a.current);
        return (
          <Fragment key={item.screen}>
            <Link
              href={navHref(item.route)}
              onClick={onNavigate}
              className="navlink"
              aria-current={here ? (parent ? "true" : "page") : undefined}
              data-parent-current={parent ? "" : undefined}
            >
              {NAV_ICONS[item.screen]}
              {t(item.label)}
              {item.screen === "home" && homeCount > 0 && (
                <span className="navbadge num" title={`${homeCount} ${t("waiting for you")}`}>
                  {homeCount}
                </span>
              )}
            </Link>
            {item.screen === "apps" && appMenu?.notice && (
              <div role="status" className="ml-[10px] border-l border-ink-700 pl-[6px] py-[3px] text-label text-ink-400">
                {appMenu.notice.text}{" "}
                {!appMenu.notice.retrying && (
                  <button type="button" onClick={appMenu.notice.onRetry} className="text-accent hover:underline">
                    {t("Retry")}
                  </button>
                )}
              </div>
            )}
            {apps.length > 0 && (
              <div className="mt-[2px] mb-[2px] grid gap-[1px] ml-[10px] border-l border-ink-700 pl-[6px]">
                {apps.map((app) => (
                  <Fragment key={app.installId}>
                    <Link
                      href={app.href}
                      onClick={onNavigate}
                      // CAD-1137: hover/focus warms the app's snapshot so
                      // opening it paints the last data at once.
                      onMouseEnter={() => prefetchWorkspaceApp(app.installId)}
                      onFocus={() => prefetchWorkspaceApp(app.installId)}
                      className="navlink navlink-sub"
                      title={app.title}
                      aria-current={here && app.current ? (app.sections ? "true" : "page") : undefined}
                      data-parent-current={here && app.current && app.sections ? "" : undefined}
                    >
                      <span className="truncate">{app.title}</span>
                    </Link>
                    {app.sections && (
                      <nav aria-label={`${app.title} sections`} className="grid gap-[1px] ml-[10px] border-l border-ink-700 pl-[6px]">
                        {app.sections.map((s) => (
                          <Link
                            key={s.label}
                            href={s.href}
                            onClick={onNavigate}
                            onMouseEnter={() => prefetchWorkspaceApp(app.installId)}
                            onFocus={() => prefetchWorkspaceApp(app.installId)}
                            className="navlink navlink-sub"
                            aria-current={s.current && app.current ? "page" : undefined}
                          >
                            {s.label}
                          </Link>
                        ))}
                      </nav>
                    )}
                  </Fragment>
                ))}
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
  const { t } = useLocale();
  // "…" until the cards load; a stale list keeps its numbers.
  const count = (key: string) => {
    if (!issues.data) return { n: issues.status === "failed" ? "!" : "…", title: issues.error ?? t("loading issues") };
    const c = issueCounts(issues.data, key);
    return { n: String(c.open), title: countLabel(c) };
  };
  const all = count("all");
  return (
    <div className="grid gap-[2px]">
      {projects.length === 0 && !projectsError ? (
        <span className="mx-[10px] py-1 text-micro text-ink-500">{t("No projects yet")}</span>
      ) : (
        <Link href={projectHref("all")} onClick={onNavigate} className={`proj ${project === "all" ? "on" : ""}`}>
          <span className="truncate">{t("All projects")}</span>
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
          {t("could not load projects")}
        </span>
      )}
    </div>
  );
}
