import { useRef, useState } from "react";
import type { ResourceState } from "../../lib/cache";
import { prefetchIssue, resources } from "../../lib/resources";
import type { CreateProjectResponse, IssueCard, Project } from "../../lib/types";
import { navigate } from "../../lib/useLocation";
import { useNavigation } from "../../../screen-ui/src/navigation";
import Link from "../../ui/Link";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import { HealthBadge, ProgressBar } from "./HealthBadge";
import { healthView, progressView } from "./work";
import { projectSummary } from "./overview";
import NewProjectModal from "./NewProjectModal";
import { projectDisplayName, projectPresetLabel } from "./projectCreate";
import ProjectMilestoneSummary from "./ProjectMilestoneSummary";
import { useLocale } from "../../lib/locale";

const path = (project: string, section = "overview") => project === "all" ? `/projects${section === "issues" ? "?view=list" : ""}` : `/projects/${encodeURIComponent(project)}${section === "overview" ? "" : `/${section}`}`;
export default function ProjectsOverview({
  project,
  projects,
  issues,
  onOpenIssue,
  onRetry,
  milestonesEnabled = true,
  workflowsEnabled = true,
  archivedTab = false,
  activeHref,
  archivedHref,
  archivedProjects = null,
  archivedLoading = false,
  archivedError = null,
  archivedSupported = false,
  onRetryArchived,
  canCreateProject = false,
  hosted = false,
  onProjectCreated,
}: {
  project: string;
  projects: Project[];
  issues: ResourceState<IssueCard[]>;
  onOpenIssue: (id: string) => void;
  onRetry: () => void;
  milestonesEnabled?: boolean;
  workflowsEnabled?: boolean;
  archivedTab?: boolean;
  activeHref?: string;
  archivedHref?: string;
  archivedProjects?: Project[] | null;
  archivedLoading?: boolean;
  archivedError?: string | null;
  archivedSupported?: boolean;
  onRetryArchived?: () => void;
  canCreateProject?: boolean;
  hosted?: boolean;
  onProjectCreated?: (created: CreateProjectResponse) => void;
}) {
  const { t, formatNumber } = useLocale();
  const navigation = useNavigation();
  const [createOpen, setCreateOpen] = useState(false);
  const newProjectBtnRef = useRef<HTMLButtonElement | null>(null);
  const goProject = (key: string) => {
    const dest = path(key);
    if (navigation) navigation.navigate(dest);
    else navigate(dest);
  };
  const cards = issues.data ?? [];
  const summary = projectSummary(cards, project);
  const all = project === "all";
  const selected = projects.find((item) => item.key === project);
  const selectedName = !all ? projectDisplayName(selected ?? { key: project }) : null;
  const selectedPreset = !all ? projectPresetLabel(selected?.preset) : null;
  const selectedGoal = !all ? selected?.goal?.trim() || null : null;
  return <main className="project-overview px-4 lg:px-8 py-6 min-w-0">
    <header className="project-page-heading">
      <div>
        <div className="project-overview-kicker">
          <span className="slabel">{t(all ? "Your workspace" : "Project overview")}</span>
          {selectedPreset && <span className="project-preset-chip">{t(selectedPreset)}</span>}
          {!all && selected?.name?.trim() && selected.name.trim() !== selected.key && (
            <span className="num text-micro text-ink-500">{selected.key}</span>
          )}
        </div>
        <h1>{all ? t("Projects") : selectedName}</h1>
        <p>
          {all
            ? t(
                archivedTab
                  ? "Archived projects are hidden from active views and keep their work read-only."
                  : "Coordinate people, agents, deliverables and approvals across your workspace.",
              )
            : selectedGoal ?? t("Current work, active initiatives, and the next things to inspect.")}
        </p>
      </div>
      <div className="project-heading-actions">
        {all && !archivedTab && canCreateProject && (
          <button
            ref={newProjectBtnRef}
            type="button"
            className="btn btn-primary"
            onClick={() => setCreateOpen(true)}
          >
            {t("New project")}
          </button>
        )}
        <Link className="project-action" href={path(project, "issues")}>{t(all ? "All work" : "Browse work")} <span aria-hidden>↗</span></Link>
      </div>
    </header>
    {all && !archivedTab && canCreateProject && (
      <NewProjectModal
        open={createOpen}
        hosted={hosted}
        returnFocusTo={newProjectBtnRef.current}
        onClose={() => setCreateOpen(false)}
        onCreated={(created) => {
          setCreateOpen(false);
          void resources.projects.refresh();
          onProjectCreated?.(created);
          goProject(created.project.key);
        }}
      />
    )}
    {all && archivedSupported && activeHref && archivedHref && (
      <div className="flex flex-wrap items-center gap-2 mb-5" role="tablist" aria-label={t("Project status")}>
        <Link
          href={activeHref}
          role="tab"
          aria-selected={!archivedTab}
          className={`btn btn-sm ${!archivedTab ? "btn-primary" : "btn-ghost"}`}
        >
          {t("Active")} ({formatNumber(projects.length)})
        </Link>
        <Link
          href={archivedHref}
          role="tab"
          aria-selected={archivedTab}
          className={`btn btn-sm ${archivedTab ? "btn-primary" : "btn-ghost"}`}
        >
          {t("Archived")}{archivedProjects !== null ? ` (${formatNumber(archivedProjects.length)})` : ""}
        </Link>
      </div>
    )}
    {(!all || !archivedTab) && (
      <>
        <ResourceGate state={issues} loading="Reading project work…" failed="Project work could not be loaded" onRetry={onRetry} />
        <StaleChip state={issues} />
        <dl className="project-stats">
          {[[t("Open work"), summary.open], [t("In progress"), summary.doing], [t("In review"), summary.review], [t("Blocked"), summary.blocked]].map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{issues.data ? value : "—"}</dd></div>)}
        </dl>
        <p className="text-micro text-ink-500 mb-6">{t("Work counts exclude initiatives. In progress reflects tracker status; it does not mean an agent is currently running.")}</p>
      </>
    )}
    {!all && milestonesEnabled && <ProjectMilestoneSummary project={project} issuesAsOf={issues.asOf} />}
    {all && archivedTab ? (
      <section aria-label={t("Archived projects")}>
        <div className="flex items-center justify-between mb-3">
          <h2 className="text-section font-semibold text-ink-100">{t("Archived projects")}</h2>
          {archivedProjects !== null && (
            <span className="text-label text-ink-500">{formatNumber(archivedProjects.length)} {t("projects")}</span>
          )}
        </div>
        {archivedLoading && archivedProjects === null && (
          <p className="project-summary-card text-ink-400">{t("Reading archived projects…")}</p>
        )}
        {archivedError && archivedProjects === null && (
          <div className="project-summary-card text-ink-300">
            <p className="text-fail">{archivedError}</p>
            {onRetryArchived && (
              <button type="button" className="btn btn-sm mt-3" onClick={onRetryArchived}>
                {t("Retry")}
              </button>
            )}
          </div>
        )}
        {archivedProjects !== null && archivedProjects.length > 0 && (
          <div className="project-grid">
            {archivedProjects.map((item) => {
              const displayName = projectDisplayName(item);
              const presetLabel = projectPresetLabel(item.preset);
              const goal = item.goal?.trim();
              return (
                <article key={item.key} className="project-summary-card">
                  <div className="flex items-center justify-between gap-3">
                    <Link href={path(item.key)} className="project-name">{displayName} <span aria-hidden>↗</span></Link>
                    <div className="project-card-badges">
                      {presetLabel && <span className="project-preset-chip">{t(presetLabel)}</span>}
                      <span className="num text-micro text-ink-500">{item.prefix}</span>
                    </div>
                  </div>
                  {goal && <p className="project-card-goal mt-2">{goal}</p>}
                  <p className="text-label text-ink-400 mt-2">
                    {formatNumber(item.issues)} {t("items")} · {t("Read-only")}
                  </p>
                  {item.archived_at && (
                    <p className="text-micro text-ink-500 mt-1">
                      {t("Archived at")} {item.archived_at}
                      {item.archived_by ? ` · ${item.archived_by}` : ""}
                    </p>
                  )}
                  <div className="mt-4 flex flex-wrap gap-2">
                    <Link href={path(item.key)} className="btn btn-sm">{t("Browse read-only")}</Link>
                    <Link href={path(item.key, "settings")} className="btn btn-sm btn-ghost">{t("Restore in settings")}</Link>
                  </div>
                </article>
              );
            })}
          </div>
        )}
        {archivedProjects !== null && archivedProjects.length === 0 && (
          <p className="project-summary-card text-ink-400">{t("No archived projects.")}</p>
        )}
      </section>
    ) : all ? <section aria-label={t("Projects")}><div className="flex items-center justify-between mb-3"><h2 className="text-section font-semibold text-ink-100">{t("Your projects")}</h2><span className="text-label text-ink-500">{formatNumber(projects.length)} {t("projects")}</span></div>
      <div className="project-grid">{projects.map((item) => {
        const displayName = projectDisplayName(item);
        const presetLabel = projectPresetLabel(item.preset);
        const goal = item.goal?.trim();
        const work = projectSummary(cards, item.key);
        const focus = issues.data ? work.focus[0] : undefined;
        const dotCls = work.blocked > 0
          ? "is-blocked"
          : focus?.status === "review"
            ? "is-review"
            : focus
              ? "is-active"
              : "";
        const attnText = !issues.data
          ? t("Reading project work…")
          : focus
            ? `${t(focus.status === "review" ? "In review" : focus.blocked ? "Blocked work" : "Next up")} · ${focus.title}`
            : t("No open work. This project is ready for its next goal.");
        return <article
          key={item.key}
          role="link"
          tabIndex={0}
          aria-label={displayName}
          className="project-summary-card project-card-clickable"
          onClick={() => goProject(item.key)}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              goProject(item.key);
            }
          }}
        >
          <div className="project-card-eyebrow">
            <div className="project-card-title-row">
              <span className="project-name">{displayName}</span>
              <span className="project-card-arrow" aria-hidden="true">↗</span>
            </div>
            <div className="project-card-badges">
              {presetLabel && <span className="project-preset-chip">{t(presetLabel)}</span>}
              <span className="num text-micro text-ink-500 shrink-0">{item.prefix}</span>
            </div>
          </div>
          {goal && <p className="project-card-goal">{goal}</p>}
          <div className="project-card-attention">
            <span className={`project-attn-dot ${dotCls}`} aria-hidden="true" />
            <span className="project-card-attention-text">{attnText}</span>
          </div>
          <div className="project-card-foot">
            <span className="project-card-stats">
              {issues.data ? (
                <>
                  <span>{formatNumber(work.open)} {t("open")}</span>
                  {work.review > 0 && <span>· {formatNumber(work.review)} {t("in review")}</span>}
                  {work.blocked > 0 && <span className="text-warn">· {formatNumber(work.blocked)} {t("blocked")}</span>}
                </>
              ) : (
                <span>—</span>
              )}
            </span>
            <span className="num shrink-0">
              {issues.data ? `${formatNumber(work.epics.length)} ${t("initiatives")}` : item.prefix}
            </span>
          </div>
        </article>;
      })}</div>
      {projects.length === 0 && <p className="project-summary-card text-ink-400">{t("No projects are available yet.")}</p>}
    </section> : <div className="project-detail-grid">
      <section className="project-summary-card" aria-label={t("Current work")}><div className="flex items-baseline justify-between mb-3"><h2 className="text-section text-ink-100 font-semibold">{t("Current work")}</h2><Link className="lnk text-label" href={path(project, "issues")}>{t("All work")} ↗</Link></div><p className="text-label text-ink-500 m-0">{t("Click any item to inspect its details, reviews, and evidence.")}</p>
        <ul className="project-focus-list">{summary.focus.map((issue) => <li key={issue.id}>
          <article
            role="link"
            tabIndex={0}
            aria-label={`${issue.id} ${issue.title}`}
            className="project-focus-card"
            onClick={() => onOpenIssue(issue.id)}
            onMouseEnter={() => prefetchIssue(issue.id)}
            onFocus={() => prefetchIssue(issue.id)}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                onOpenIssue(issue.id);
              }
            }}
          >
            <span className="flex items-center gap-2 text-micro text-ink-500">
              <span className="num">{issue.id}</span>
              <span className="capitalize">{t(issue.status)}</span>
              {issue.blocked && <span className="text-warn">{t("Blocked")}</span>}
              <span className="ml-auto num">{issue.priority}</span>
            </span>
            <span className="project-focus-title">{issue.title}</span>
            <span className="text-micro text-ink-400">{issue.owner ?? t("Unassigned")}</span>
          </article>
        </li>)}</ul>
        {issues.data && summary.focus.length === 0 && <p className="text-label text-ink-500 py-5">{t("No open work in this project.")}</p>}
      </section>
      <div className="space-y-5"><section className="project-summary-card" aria-label={t("Active initiatives")}><div className="flex items-baseline justify-between mb-3"><h2 className="text-section text-ink-100 font-semibold">{t("Active initiatives")}</h2><Link className="lnk text-label" href={path(project, "plan")}>{t("See all")} ↗</Link></div>
        {summary.epics.slice().sort((a, b) => Number(b.work?.health?.state === "at_risk") - Number(a.work?.health?.state === "at_risk")).slice(0, 3).map((epic) => <div className="project-epic" key={epic.id}><button className="text-left text-secondary text-ink-200 hover:text-accent" onClick={() => onOpenIssue(epic.id)}>{epic.title}</button><div className="my-2 flex gap-2"><span className="num text-micro text-ink-500">{epic.id}</span><HealthBadge view={healthView(epic.work?.health, undefined, t)} /></div><ProgressBar view={progressView(epic.work?.progress, t)} tone={healthView(epic.work?.health, undefined, t).tone} t={t} /></div>)}
        {issues.data && summary.epics.length === 0 && <p className="text-label text-ink-500 m-0">{t("No active initiatives yet.")}</p>}
      </section><section className="project-summary-card"><h2 className="text-section text-ink-100 font-semibold mb-3">{t("Explore this project")}</h2><div className="project-section-links">{([
        ...(milestonesEnabled ? [["plan", t("Plan"), t("Milestones, delivery dates, and stage-gated initiatives")]] : [["plan", t("Plan"), t("Stage-gated initiatives and workstreams")]]),
        ...(workflowsEnabled ? [["context", t("Context & Workflows"), t("Project guidance, briefs, and repeatable workflows")]] : [["context", t("Context"), t("Project guidance and reading order")]]),
      ] as const).map(([section, title, description]) => <Link key={section} href={path(project, section)}><span>{title} ↗</span><small>{description}</small></Link>)}</div>{selected?.repos[0]?.remote && <p className="text-micro text-ink-500 mt-4 break-all">{selected.repos[0].remote}</p>}</section></div>
    </div>}
  </main>;
}
