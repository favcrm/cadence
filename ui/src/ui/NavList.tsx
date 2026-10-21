import { Fragment, useEffect, useId, type ReactNode } from "react";
import { countLabel, issueCounts } from "../lib/counts";
import type { ResourceState } from "../lib/cache";
import type { AppNav } from "../features/app-shell/appNav";
import { noteCurrentApp, updateSidebarPrefs, useSidebarPrefs } from "../features/app-shell/sidebarPrefs";
import { prefetchWorkspaceApp } from "../features/workspace-apps/workspaceApps";
import { navMatches } from "../features/issues/model";
import { NAV, type Route, type Screen } from "../lib/router";
import type { IssueCard, Project } from "../lib/types";
import Link from "./Link";
import { IconAgents, IconApps, IconCaret, IconHome, IconMyWork, IconOutbox, IconPin, IconProjects, IconSettings, IconWiki, IconThreads } from "./icons";
import { useLocale } from "../lib/locale";

const NAV_ICONS: Record<string, ReactNode> = {
  home: <IconHome size={16} />,
  myWork: <IconMyWork size={16} />,
  projects: <IconProjects size={16} />,
  wiki: <IconWiki size={16} />,
  apps: <IconApps size={16} />,
  agents: <IconAgents size={16} />,
  threads: <IconThreads size={16} />,
  outbox: <IconOutbox size={16} />,
  settings: <IconSettings size={16} />,
};

/** A folding region (CAD-1692): animates its height through
 *  `grid-template-rows`, and is `inert` while folded so its links leave
 *  the tab order and the accessibility tree. */
function Fold({ id, open, children }: { id: string; open: boolean; children: ReactNode }) {
  return (
    <div id={id} className="navfold" data-open={open ? "" : undefined} aria-hidden={!open} inert={!open}>
      <div className="navfold-in">{children}</div>
    </div>
  );
}

/** The chevron button beside a nav row; the row's link keeps navigating. */
function Disclosure({ open, controls, label, onToggle }: { open: boolean; controls: string; label: string; onToggle: () => void }) {
  return (
    <button type="button" className="navdisc" aria-expanded={open} aria-controls={controls} aria-label={label} title={label} onClick={onToggle}>
      <IconCaret size={12} />
    </button>
  );
}

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
  const prefs = useSidebarPrefs();
  const foldId = useId();
  const apps = appMenu?.apps ?? [];
  const currentApp = apps.find((a) => a.current) ?? null;
  // An empty menu means the installed-app list has not answered yet: not a sighting.
  const currentInstall = apps.length === 0 ? undefined : currentApp?.installId ?? null;
  useEffect(() => {
    if (currentInstall !== undefined) noteCurrentApp(currentInstall);
  }, [currentInstall]);
  return (
    <nav className="grid gap-[2px]" aria-label={t(label)}>
      {NAV.map((item) => {
        const here = navMatches(screen, item.screen);
        if (item.screen !== "apps" || apps.length === 0) {
          return (
            <Fragment key={item.screen}>
              <Link
                href={navHref(item.route)}
                onClick={onNavigate}
                className="navlink"
                aria-current={here ? "page" : undefined}
              >
                {NAV_ICONS[item.screen]}
                {t(item.label)}
                {item.screen === "home" && homeCount > 0 && (
                  <span className="navbadge num" title={`${homeCount} ${t("waiting for you")}`}>
                    {homeCount}
                  </span>
                )}
              </Link>
              {item.screen === "apps" && appMenu?.notice && <AppNotice notice={appMenu.notice} />}
            </Fragment>
          );
        }
        // With an app open under Apps the most specific row carries the accent;
        // the parent stays current for assistive tech only.
        const parent = here && currentApp !== null;
        const open = prefs.appsOpen;
        return (
          <Fragment key={item.screen}>
            <div className="navrow">
              <Link
                href={navHref(item.route)}
                onClick={onNavigate}
                className="navlink"
                aria-current={here ? (parent ? "true" : "page") : undefined}
                data-parent-current={parent ? "" : undefined}
              >
                {NAV_ICONS[item.screen]}
                {t(item.label)}
              </Link>
              <Disclosure
                open={open}
                controls={foldId}
                label={t(open ? "Collapse apps" : "Expand apps")}
                onToggle={() => updateSidebarPrefs((p) => ({ ...p, appsOpen: !p.appsOpen }))}
              />
            </div>
            {appMenu?.notice && <AppNotice notice={appMenu.notice} />}
            {/* Folded on an app screen: the open app keeps one row, so the sidebar still says where you are. */}
            {!open && here && currentApp !== null && (
              <div className="navtree">
                <Link href={currentApp.href} onClick={onNavigate} className="navlink navlink-sub" title={currentApp.title} aria-current="page">
                  <span className="truncate">{currentApp.title}</span>
                </Link>
              </div>
            )}
            <Fold id={foldId} open={open}>
              <div className="navtree">
                {apps.map((app) => {
                  const sectionsOpen = !prefs.closedApps.includes(app.installId);
                  const sectionsId = `${foldId}-${app.installId}`;
                  return (
                    <Fragment key={app.installId}>
                      <div className="navrow">
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
                          <Disclosure
                            open={sectionsOpen}
                            controls={sectionsId}
                            label={`${t(sectionsOpen ? "Collapse" : "Expand")} ${app.title} ${t("sections")}`}
                            onToggle={() => updateSidebarPrefs((p) => ({
                              ...p,
                              closedApps: p.closedApps.includes(app.installId)
                                ? p.closedApps.filter((id) => id !== app.installId)
                                : [...p.closedApps, app.installId],
                            }))}
                          />
                        )}
                      </div>
                      {app.sections && (
                        <Fold id={sectionsId} open={sectionsOpen}>
                          <nav aria-label={`${app.title} ${t("sections")}`} className="navtree">
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
                                {t(s.label)}
                              </Link>
                            ))}
                          </nav>
                        </Fold>
                      )}
                    </Fragment>
                  );
                })}
              </div>
            </Fold>
          </Fragment>
        );
      })}
    </nav>
  );
}

function AppNotice({ notice }: { notice: NonNullable<AppNav["notice"]> }) {
  const { t } = useLocale();
  return (
    <div role="status" className="ml-[10px] border-l border-ink-700 pl-[6px] py-[3px] text-label text-ink-400">
      {t(notice.text)}{" "}
      {!notice.retrying && (
        <button type="button" onClick={notice.onRetry} className="text-accent hover:underline">
          {t("Retry")}
        </button>
      )}
    </div>
  );
}

/** The Projects section (CAD-1692): its heading folds the list, which
 *  keeps its place under the nav. The desktop sidebar and the phone menu
 *  both render it. */
export function ProjectsSection(props: ProjectListProps) {
  const { t } = useLocale();
  const { projectsOpen } = useSidebarPrefs();
  const listId = useId();
  return (
    <section aria-label={t("Projects")}>
      <div className="projhead">
        <button
          type="button"
          className="projhead-toggle slabel"
          aria-expanded={projectsOpen}
          aria-controls={listId}
          onClick={() => updateSidebarPrefs((p) => ({ ...p, projectsOpen: !p.projectsOpen }))}
        >
          <IconCaret size={10} />
          {t("Projects")}
        </button>
        <span className="num text-[10px] text-ink-500">{projectsOpen ? "~/pm" : props.projects.length}</span>
      </div>
      <Fold id={listId} open={projectsOpen}>
        <ProjectList {...props} />
      </Fold>
    </section>
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
 *  projects" row gives way to a plain line. Pinned projects (CAD-1692)
 *  lead; with any pin the rest wait behind "Show N more", except the
 *  project on screen. */
export function ProjectList({ project, projectHref, projects, issues, projectsError, onNavigate }: ProjectListProps) {
  const { t } = useLocale();
  const prefs = useSidebarPrefs();
  // "…" until the cards load; a stale list keeps its numbers.
  const count = (key: string) => {
    if (!issues.data) return { n: issues.status === "failed" ? "!" : "…", title: issues.error ?? t("loading issues") };
    const c = issueCounts(issues.data, key);
    return { n: String(c.open), title: countLabel(c) };
  };
  const all = count("all");
  const byKey = new Map(projects.map((p) => [p.key, p]));
  // A pin for a project that no longer exists renders nothing.
  const pinned = prefs.pinned.flatMap((key) => byKey.get(key) ?? []);
  const rest = projects.filter((p) => !prefs.pinned.includes(p.key));
  const limited = pinned.length > 0 && !prefs.showAll;
  const shown = limited ? rest.filter((p) => p.key === project) : rest;
  const row = (p: Project, isPinned: boolean) => {
    const c = count(p.key);
    return (
      <div key={p.key} className="projrow" data-pinned={isPinned ? "" : undefined}>
        <Link
          href={projectHref(p.key)}
          onClick={onNavigate}
          className={`proj ${project === p.key ? "on" : ""}`}
          aria-current={project === p.key ? "page" : undefined}
        >
          <span className="truncate">{p.key}</span>
          <span className="proj-prefix num">{p.prefix}</span>
          <span className={`proj-count num ${c.n === "0" ? "zero" : ""}`} title={c.title}>{c.n}</span>
        </Link>
        <button
          type="button"
          className="projpin"
          aria-pressed={isPinned}
          aria-label={`${t(isPinned ? "Unpin" : "Pin")} ${p.key}`}
          title={t(isPinned ? "Unpin" : "Pin")}
          onClick={() => updateSidebarPrefs((s) => ({
            ...s,
            pinned: s.pinned.includes(p.key) ? s.pinned.filter((k) => k !== p.key) : [...s.pinned, p.key],
          }))}
        >
          <IconPin size={12} />
        </button>
      </div>
    );
  };
  return (
    <div className="grid gap-[2px]">
      {projects.length === 0 && !projectsError ? (
        <span className="mx-[10px] py-1 text-micro text-ink-500">{t("No projects yet")}</span>
      ) : (
        <Link href={projectHref("all")} onClick={onNavigate} className={`proj ${project === "all" ? "on" : ""}`}>
          <span className="truncate">{t("All projects")}</span>
          <span />
          <span className="proj-count num" title={all.title}>{all.n}</span>
        </Link>
      )}
      {pinned.map((p) => row(p, true))}
      {pinned.length > 0 && shown.length > 0 && <div className="projsep" role="separator" />}
      {shown.map((p) => row(p, false))}
      {pinned.length > 0 && (prefs.showAll ? rest.length > 0 : rest.length > shown.length) && (
        <button type="button" className="projmore" onClick={() => updateSidebarPrefs((s) => ({ ...s, showAll: !s.showAll }))}>
          {prefs.showAll ? t("Show pinned only") : t("Show {count} more").replace("{count}", String(rest.length - shown.length))}
        </button>
      )}
      {projectsError && (
        <span className="mx-[11px] mt-1 text-micro text-fail" role="alert" title={projectsError}>
          {t("could not load projects")}
        </span>
      )}
    </div>
  );
}
