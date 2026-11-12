import { useMemo } from "react";
import type { ResourceState } from "../../lib/cache";
import type { IssueCard, Project } from "../../lib/types";
import { resources } from "../../lib/resources";
import { useQuery } from "../../lib/useResource";
import Link from "../../ui/Link";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import { HealthBadge, ProgressBar } from "./HealthBadge";
import { readArchivedProjects, readLifecycle } from "./archiveContract";
import { healthView, progressView } from "./work";
import { projectSummary } from "./overview";
import ProjectMilestoneSummary from "./ProjectMilestoneSummary";
import { useLocale } from "../../lib/locale";

const path = (project: string, section = "overview") => project === "all" ? `/projects${section === "issues" ? "?view=list" : ""}` : `/projects/${encodeURIComponent(project)}${section === "overview" ? "" : `/${section}`}`;
export default function ProjectsOverview({ project, projects, issues, archivePending, onOpenIssue, onRetry }: {
  project: string; projects: Project[]; issues: ResourceState<IssueCard[]>;
  /** The slug is absent from the active list and its archive state is still
      being read (`reading`) or could not be proved (`unproved`): show that
      honestly instead of zero work. */
  archivePending?: "reading" | "unproved";
  onOpenIssue: (id: string) => void; onRetry: () => void;
}) {
  const { t, formatNumber } = useLocale();
  const cards = issues.data ?? [];
  const summary = projectSummary(cards, project);
  const all = project === "all";
  // A pending or unproved archived-project read has not proved what work
  // exists; never present the shared list's emptiness as this project's.
  const workKnown = issues.data !== null && !(archivePending !== undefined && !all);
  const selected = projects.find((item) => item.key === project);
  return <main className="project-overview px-4 lg:px-8 py-6 min-w-0">
    <header className="project-page-heading">
      <div><div className="slabel">{t(all ? "Your workspace" : "Project overview")}</div><h1>{all ? t("Projects") : project}</h1><p>{t(all ? "See where work is moving and choose a project to explore." : "Current work, larger outcomes, and the next things to inspect.")}</p></div>
      <Link className="project-action" href={path(project, "issues")}>{t(all ? "All issues" : "Browse issues")} <span aria-hidden>↗</span></Link>
    </header>
    {!all && <ArchivedProjectNotice project={project} />}
    <ResourceGate state={issues} loading="Reading project work…" failed="Project work could not be loaded" onRetry={onRetry} />
    <StaleChip state={issues} />
    {archivePending && <p className="roadmap-notice" role="status">
      {archivePending === "reading"
        ? t("Reading this project's archive state before showing its work…")
        : t("This project's archive state could not be proved, so its work is not shown as empty or active.")}
    </p>}
    <dl className="project-stats">
      {[[t("Open issues"), summary.open], [t("In progress"), summary.doing], [t("In review"), summary.review], [t("Blocked"), summary.blocked]].map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{workKnown ? value : "—"}</dd></div>)}
    </dl>
    <p className="text-micro text-ink-500 mb-6">{t("Issue counts exclude epics. In progress reflects tracker status; it does not mean an agent is currently running.")}</p>
    {!all && <ProjectMilestoneSummary project={project} issuesAsOf={archivePending ? null : issues.asOf} />}
    {all ? <section aria-label={t("Projects")}><div className="flex items-center justify-between mb-3"><h2 className="text-section font-semibold text-ink-100">{t("Your projects")}</h2><span className="text-label text-ink-500">{formatNumber(projects.length)} {t("projects")}</span></div>
      <div className="project-grid">{projects.map((item) => {
        const work = projectSummary(cards, item.key);
        return <article key={item.key} className="project-summary-card">
          <div className="flex items-center justify-between gap-3"><Link href={path(item.key)} className="project-name">{item.key} <span aria-hidden>↗</span></Link><span className="num text-micro text-ink-500">{item.prefix}</span></div>
          <div className="project-work-counts"><span><strong>{issues.data ? formatNumber(work.doing) : "—"}</strong> {t("in progress")}</span><span><strong>{issues.data ? formatNumber(work.review) : "—"}</strong> {t("in review")}</span><span className={work.blocked ? "text-warn" : ""}><strong>{issues.data ? formatNumber(work.blocked) : "—"}</strong> {t("blocked")}</span></div>
          <p className="text-label text-ink-500">{issues.data ? `${formatNumber(work.open)} ${t("open issues")} · ${formatNumber(work.epics.length)} ${t("active epics")}` : t("Reading project work…")}</p>
          {issues.data && work.focus[0] ? <button className="project-focus-preview" onClick={() => onOpenIssue(work.focus[0].id)}><span className="slabel">{t(work.focus[0].status === "review" ? "In review" : work.focus[0].blocked ? "Blocked work" : "Current focus")}</span><span className="block mt-1 text-ink-200">{work.focus[0].title}</span><span className="num text-micro text-ink-500 mt-1 block">{work.focus[0].id}</span></button> : issues.data && <p className="project-focus-preview text-label text-ink-500">{t("No open work. This project is ready for its next goal.")}</p>}
        </article>;
      })}</div>
      {projects.length === 0 && <p className="card p-6 text-ink-400">{t("No projects are available yet.")}</p>}
      <ArchivedProjectsList />
    </section> : <div className="project-detail-grid">
      <section className="project-summary-card" aria-label={t("Current work")}><div className="flex items-baseline justify-between mb-4"><h2 className="text-section text-ink-100 font-semibold">{t("Current work")}</h2><Link className="lnk text-label" href={path(project, "issues")}>{t("All issues")} ↗</Link></div><p className="text-label text-ink-500 mb-3">{t("Reviews and blockers first, followed by work in progress.")}</p>
        <ul className="project-focus-list">{summary.focus.map((issue) => <li key={issue.id}><button onClick={() => onOpenIssue(issue.id)}><span className="flex items-center gap-2 text-micro text-ink-500"><span className="num">{issue.id}</span><span className="capitalize">{t(issue.status)}</span>{issue.blocked && <span className="text-warn">{t("Blocked")}</span>}<span className="ml-auto">{issue.priority}</span></span><span className="block text-ink-200 mt-1.5">{issue.title}</span><span className="block text-micro text-ink-500 mt-1.5">{issue.owner ?? t("Unassigned")}</span></button></li>)}</ul>
        {workKnown && summary.focus.length === 0 && <p className="text-label text-ink-500 py-5">{t("No open issues in this project.")}</p>}
      </section>
      <div className="space-y-5"><section className="project-summary-card" aria-label={t("Active epics")}><div className="flex items-baseline justify-between mb-4"><h2 className="text-section text-ink-100 font-semibold">{t("Active epics")}</h2><Link className="lnk text-label" href={path(project, "epics")}>{t("See all")} ↗</Link></div>
        {summary.epics.slice().sort((a, b) => Number(b.work?.health?.state === "at_risk") - Number(a.work?.health?.state === "at_risk")).slice(0, 3).map((epic) => <div className="project-epic" key={epic.id}><button className="text-left text-secondary text-ink-200 hover:text-accent" onClick={() => onOpenIssue(epic.id)}>{epic.title}</button><div className="my-2 flex gap-2"><span className="num text-micro text-ink-500">{epic.id}</span><HealthBadge view={healthView(epic.work?.health, undefined, t)} /></div><ProgressBar view={progressView(epic.work?.progress, t)} tone={healthView(epic.work?.health, undefined, t).tone} t={t} /></div>)}
        {workKnown && summary.epics.length === 0 && <p className="text-label text-ink-500">{t("No active epics yet.")}</p>}
      </section><section className="project-summary-card"><h2 className="text-section text-ink-100 font-semibold mb-3">{t("Explore this project")}</h2><div className="project-section-links">{[["milestones", t("Milestones"), t("Delivery dates, criteria, and evidence")], ["workflows", t("Workflows"), t("Repeatable work you can run")], ["context", t("Context"), t("Project guidance and reading order")]].map(([section, title, description]) => <Link key={section} href={path(project, section)}><span>{title} ↗</span><small>{description}</small></Link>)}</div>{selected?.repos[0]?.remote && <p className="text-micro text-ink-500 mt-4 break-all">{selected.repos[0].remote}</p>}</section></div>
    </div>}
  </main>;
}

/**
 * CAD-2255: archived projects stay discoverable from the all-project view.
 * The capability is explicit: a server that ignores `?archived=1` is
 * `unavailable`, which says so — never "no archived projects".
 */
function ArchivedProjectsList() {
  const { t, formatNumber } = useLocale();
  const state = useQuery(resources.archivedProjects);
  const view = useMemo(() => readArchivedProjects(state.data), [state.data]);
  return <section className="mt-8" aria-label={t("Archived projects")}>
    <div className="flex flex-wrap items-center gap-2 mb-3">
      <h2 className="text-section font-semibold text-ink-100">{t("Archived projects")}</h2>
      <StaleChip state={state} />
    </div>
    {state.data === null ? (
      <ResourceGate state={state} loading={t("Reading archived projects…")} failed={t("Archived projects could not be read")} onRetry={() => void resources.archivedProjects.refresh()} />
    ) : view.kind === "unavailable" ? (
      <p className="card p-4 text-label text-ink-400" role="status">
        {t("This board's server does not offer an archived-project view, so archived projects cannot be listed here.")}
        <span className="mt-1 block text-ink-500">{view.reason}</span>
      </p>
    ) : view.kind === "unreadable" ? (
      <div className="card p-4" role="alert">
        <p className="text-label text-fail">{t("The archived-project answer could not be read.")}</p>
        <p className="mt-1 text-label text-ink-500">{view.reason}</p>
        <button type="button" className="btn mt-3" onClick={() => void resources.archivedProjects.refresh()}>{t("Read again")}</button>
      </div>
    ) : view.projects.length === 0 ? (
      <p className="card p-4 text-label text-ink-400">{t("No archived projects.")}</p>
    ) : (
      <ul className="grid gap-2">
        {view.projects.map((item) => <li key={item.key} className="card flex flex-wrap items-center gap-3 p-4">
          <div className="min-w-0 flex-1">
            <Link href={path(item.key)} className="project-name">{item.key} <span aria-hidden>↗</span></Link>
            <p className="text-label text-ink-500 mt-1">
              <span className="chip bg-warn/10 text-warn mr-2">{t("Archived")}</span>
              {`${formatNumber(item.issues)} ${t("issues")} · ${item.prefix}`}
            </p>
          </div>
          <Link className="lnk text-label" href={path(item.key, "settings")}>{t("Open settings")} ↗</Link>
        </li>)}
      </ul>
    )}
  </section>;
}

/**
 * A project page whose slug is absent from the active list may be archived.
 * Only a proved receipt says so; an unavailable capability stays silent so an
 * older server's project page is unchanged, and an unprovable answer never
 * presents the project as active.
 */
function ArchivedProjectNotice({ project }: { project: string }) {
  const { t } = useLocale();
  const state = useQuery(resources.projectLifecycle(project));
  const view = useMemo(() => readLifecycle(state.data, project), [state.data, project]);
  if (state.data === null) return null;
  if (view.kind === "available" && view.receipt.archived) {
    return <p className="roadmap-notice" role="status">
      {t("This project is archived. Its work and settings are read-only until it is restored.")}{" "}
      <Link className="lnk" href={path(project, "settings")}>{t("Open settings to restore")} ↗</Link>
    </p>;
  }
  if (view.kind === "unreadable") {
    return <p className="roadmap-notice" role="alert">
      {t("This project's archive state could not be proved, so it is not shown as either active or archived.")}
    </p>;
  }
  return null;
}
