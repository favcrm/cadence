import type { ResourceState } from "../../lib/cache";
import type { IssueCard, Project } from "../../lib/types";
import Link from "../../ui/Link";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import { HealthBadge, ProgressBar } from "./HealthBadge";
import { healthView, progressView } from "./work";
import { projectSummary } from "./overview";
import ProjectMilestoneSummary from "./ProjectMilestoneSummary";

const path = (project: string, section = "overview") => project === "all" ? `/projects${section === "issues" ? "?view=list" : ""}` : `/projects/${encodeURIComponent(project)}${section === "overview" ? "" : `/${section}`}`;
export default function ProjectsOverview({ project, projects, issues, onOpenIssue, onRetry }: {
  project: string; projects: Project[]; issues: ResourceState<IssueCard[]>; onOpenIssue: (id: string) => void; onRetry: () => void;
}) {
  const cards = issues.data ?? [];
  const summary = projectSummary(cards, project);
  const all = project === "all";
  const selected = projects.find((item) => item.key === project);
  return <main className="project-overview px-4 lg:px-8 py-6 min-w-0">
    <header className="project-page-heading">
      <div><div className="slabel">{all ? "Your workspace" : "Project overview"}</div><h1>{all ? "Projects" : project}</h1><p>{all ? "See where work is moving and choose a project to explore." : "Current work, larger outcomes, and the next things to inspect."}</p></div>
      <Link className="project-action" href={path(project, "issues")}>{all ? "All issues" : "Browse issues"} <span aria-hidden>↗</span></Link>
    </header>
    <ResourceGate state={issues} loading="Reading project work…" failed="Project work could not be loaded" onRetry={onRetry} />
    <StaleChip state={issues} />
    <dl className="project-stats">
      {[["Open issues", summary.open], ["In progress", summary.doing], ["In review", summary.review], ["Blocked", summary.blocked]].map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{issues.data ? value : "—"}</dd></div>)}
    </dl>
    <p className="text-micro text-ink-500 mb-6">Issue counts exclude epics. In progress reflects tracker status; it does not mean an agent is currently running.</p>
    {!all && <ProjectMilestoneSummary project={project} issuesAsOf={issues.asOf} />}
    {all ? <section aria-label="Projects"><div className="flex items-center justify-between mb-3"><h2 className="text-section font-semibold text-ink-100">Your projects</h2><span className="text-label text-ink-500">{projects.length} projects</span></div>
      <div className="project-grid">{projects.map((item) => {
        const work = projectSummary(cards, item.key);
        return <article key={item.key} className="project-summary-card">
          <div className="flex items-center justify-between gap-3"><Link href={path(item.key)} className="project-name">{item.key} <span aria-hidden>↗</span></Link><span className="num text-micro text-ink-500">{item.prefix}</span></div>
          <div className="project-work-counts"><span><strong>{issues.data ? work.doing : "—"}</strong> in progress</span><span><strong>{issues.data ? work.review : "—"}</strong> in review</span><span className={work.blocked ? "text-warn" : ""}><strong>{issues.data ? work.blocked : "—"}</strong> blocked</span></div>
          <p className="text-label text-ink-500">{issues.data ? `${work.open} open issues · ${work.epics.length} active epics` : "Reading project work…"}</p>
          {issues.data && work.focus[0] ? <button className="project-focus-preview" onClick={() => onOpenIssue(work.focus[0].id)}><span className="slabel">{work.focus[0].status === "review" ? "In review" : work.focus[0].blocked ? "Blocked work" : "Current focus"}</span><span className="block mt-1 text-ink-200">{work.focus[0].title}</span><span className="num text-micro text-ink-500 mt-1 block">{work.focus[0].id}</span></button> : issues.data && <p className="project-focus-preview text-label text-ink-500">No open work. This project is ready for its next goal.</p>}
        </article>;
      })}</div>
      {projects.length === 0 && <p className="card p-6 text-ink-400">No projects are available yet.</p>}
    </section> : <div className="project-detail-grid">
      <section className="project-summary-card" aria-label="Current work"><div className="flex items-baseline justify-between mb-4"><h2 className="text-section text-ink-100 font-semibold">Current work</h2><Link className="lnk text-label" href={path(project, "issues")}>All issues ↗</Link></div><p className="text-label text-ink-500 mb-3">Reviews and blockers first, followed by work in progress.</p>
        <ul className="project-focus-list">{summary.focus.map((issue) => <li key={issue.id}><button onClick={() => onOpenIssue(issue.id)}><span className="flex items-center gap-2 text-micro text-ink-500"><span className="num">{issue.id}</span><span className="capitalize">{issue.status}</span>{issue.blocked && <span className="text-warn">Blocked</span>}<span className="ml-auto">{issue.priority}</span></span><span className="block text-ink-200 mt-1.5">{issue.title}</span><span className="block text-micro text-ink-500 mt-1.5">{issue.owner ?? "Unassigned"}</span></button></li>)}</ul>
        {issues.data && summary.focus.length === 0 && <p className="text-label text-ink-500 py-5">No open issues in this project.</p>}
      </section>
      <div className="space-y-5"><section className="project-summary-card" aria-label="Active epics"><div className="flex items-baseline justify-between mb-4"><h2 className="text-section text-ink-100 font-semibold">Active epics</h2><Link className="lnk text-label" href={path(project, "epics")}>See all ↗</Link></div>
        {summary.epics.slice().sort((a, b) => Number(b.work?.health?.state === "at_risk") - Number(a.work?.health?.state === "at_risk")).slice(0, 3).map((epic) => <div className="project-epic" key={epic.id}><button className="text-left text-secondary text-ink-200 hover:text-accent" onClick={() => onOpenIssue(epic.id)}>{epic.title}</button><div className="my-2 flex gap-2"><span className="num text-micro text-ink-500">{epic.id}</span><HealthBadge view={healthView(epic.work?.health)} /></div><ProgressBar view={progressView(epic.work?.progress)} tone={healthView(epic.work?.health).tone} /></div>)}
        {issues.data && summary.epics.length === 0 && <p className="text-label text-ink-500">No active epics yet.</p>}
      </section><section className="project-summary-card"><h2 className="text-section text-ink-100 font-semibold mb-3">Explore this project</h2><div className="project-section-links">{[["milestones", "Milestones", "Delivery dates, criteria, and evidence"], ["workflows", "Workflows", "Repeatable work you can run"], ["context", "Context", "Project guidance and reading order"]].map(([section, title, description]) => <Link key={section} href={path(project, section)}><span>{title} ↗</span><small>{description}</small></Link>)}</div>{selected?.repos[0]?.remote && <p className="text-micro text-ink-500 mt-4 break-all">{selected.repos[0].remote}</p>}</section></div>
    </div>}
  </main>;
}
