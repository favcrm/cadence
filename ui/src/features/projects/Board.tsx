import { useWriteBlock } from "../auth/WriteGate";
import { useEffect, useState } from "react";
import type { WriteResp } from "../../lib/api";
import { boardHeadline, boardScope, boardVisible, issueCounts } from "../../lib/counts";
import { activeCount, epicProgress, matches, type BoardFilters } from "../../lib/filters";
import type { ResourceState } from "../../lib/cache";
import { issuePath } from "../issues/model";
import Link from "../../ui/Link";
import { agentIsUnassigned, agentMatchesProject, issueIndex } from "../../lib/scope";
import type { AgentsPayload, Health, IssueCard, Project } from "../../lib/types";
import type { ProjectView } from "../../lib/urlState";
import Card, { noDragReason } from "./Card";
import FilterBar from "./FilterBar";
import NewIssueForm from "./NewIssueForm";
import Button from "../../ui/Button";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import { useLocale } from "../../lib/locale";

const COLS: [string, string, number?][] = [
  ["backlog", "Backlog"],
  ["ready", "Ready"],
  ["doing", "Doing", 3],
  ["review", "Review"],
  ["done", "Done"],
];

const COL_DOT: Record<string, string> = {
  backlog: "bg-ink-600",
  ready: "bg-ink-400",
  doing: "bg-info",
  review: "bg-warn",
  done: "bg-ok",
};

const AGENT_DOT: Record<string, string> = {
  busy: "bg-info",
  idle: "bg-ok",
  stopped: "bg-ink-600",
  attention: "bg-fail",
};

const STATUS_CHIP: Record<string, string> = {
  backlog: "bg-ink-800 text-ink-400",
  ready: "bg-ink-800 text-ink-300",
  doing: "bg-info/10 text-info",
  review: "bg-warn/10 text-warn",
  done: "bg-ok/10 text-ok",
  dropped: "bg-ink-800 text-ink-500",
};

// The list's default order is attention-first — work in flight, then what
// could start, then the pile — rather than the kanban's column order.
const LIST_ORDER = new Map(
  ["doing", "review", "ready", "backlog", "done", "dropped"].map(
    (key, index) => [key, index],
  ),
);

const byPriority = (a: IssueCard, b: IssueCard) =>
  a.priority.localeCompare(b.priority) ||
  a.id.localeCompare(b.id, undefined, { numeric: true });

function IssueList({ issues, project, onOpen }: { issues: IssueCard[]; project: string; onOpen: (id: string) => void }) {
  const { t, formatNumber } = useLocale();
  const [sort, setSort] = useState<{ key: "id" | "title" | "status" | "priority" | "owner"; direction: "asc" | "desc" }>({ key: "status", direction: "asc" });
  const sortRows = (key: typeof sort.key) => setSort((current) => ({
    key,
    direction: current.key === key && current.direction === "asc" ? "desc" : "asc",
  }));
  const rows = issues.slice().sort((a, b) => {
    const av = sort.key === "status" ? LIST_ORDER.get(a.status) ?? 99 : sort.key === "priority" ? a.priority : (a[sort.key] ?? "");
    const bv = sort.key === "status" ? LIST_ORDER.get(b.status) ?? 99 : sort.key === "priority" ? b.priority : (b[sort.key] ?? "");
    const result = typeof av === "number" && typeof bv === "number" ? av - bv : String(av).localeCompare(String(bv), undefined, { numeric: true });
    return (sort.direction === "asc" ? result : -result) || byPriority(a, b);
  });
  const header = (key: typeof sort.key, label: string) => (
    <button className="inline-flex items-center gap-1 hover:text-ink-200" onClick={() => sortRows(key)}>
      {label}<span className="text-ink-600">{sort.key === key ? (sort.direction === "asc" ? "↑" : "↓") : "↕"}</span>
    </button>
  );
  return (
    <section className="card overflow-hidden reveal" aria-label={t("Project issue list")}>
      <div className="sm:hidden divide-y divide-ink-700/70">
        {rows.map((issue) => (
          <div key={issue.id} className="w-full text-left px-3.5 py-3.5 hover:bg-ink-850">
            <div className="flex items-center gap-2">
              <span className="num text-label text-ink-400">{issue.id}</span>
              <span className={`chip ${STATUS_CHIP[issue.status] ?? "bg-ink-800 text-ink-400"}`}>{t(issue.status)}</span>
              <span className="num text-micro text-ink-500 ml-auto">{t(issue.priority)}</span>
            </div>
            <Link href={issuePath(issue.project, issue.id)} className="board-issue-title block text-left text-ink-100 mt-1.5 leading-[1.4] font-medium hover:text-accent">{issue.title}</Link>
            <div className="flex flex-wrap gap-x-3 gap-y-1 mt-2 text-micro text-ink-500">
              <span>{issue.owner ?? t("unassigned")}</span>
              <span>{issue.component ?? "no component"}</span>
              <span>{formatNumber(issue.checks.done)}/{formatNumber(issue.checks.total)} {t("checks")}</span>
              {project === "all" && <span>{issue.project}</span>}
              <button type="button" className="board-preview-link ml-auto" aria-label={`${t("Preview")} ${issue.id}`} onClick={() => onOpen(issue.id)}>{t("Preview")}</button>
            </div>
          </div>
        ))}
        {rows.length === 0 && <div className="px-4 py-10 text-center text-ink-500">{t("No issues match this view.")}</div>}
      </div>
      <div className="hidden sm:block overflow-x-auto">
        <table className="w-full min-w-[46rem] text-label">
          <thead>
            <tr className="border-b border-ink-700 text-left">
              <th className="slabel font-normal px-4 py-2.5">{header("id", t("issue"))}</th>
              {project === "all" && <th className="slabel font-normal px-3 py-2.5">{t("project")}</th>}
              <th className="slabel font-normal px-3 py-2.5">{header("status", t("status"))}</th>
              <th className="slabel font-normal px-3 py-2.5">{header("priority", t("priority"))}</th>
              <th className="slabel font-normal px-3 py-2.5">{header("owner", t("owner"))}</th>
              <th className="slabel font-normal px-3 py-2.5">{t("component / tags")}</th>
              <th className="slabel font-normal px-3 py-2.5 text-right">{t("checks")}</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-ink-700/70">
            {rows.map((issue) => (
              <tr key={issue.id} className="hover:bg-ink-850 transition-colors">
                <td className="px-4 py-3 min-w-0 max-w-[28rem]">
                  <div className="min-w-0">
                    <span className="num text-ink-500">{issue.id}</span>
                    <Link href={issuePath(issue.project, issue.id)} className="block text-left text-ink-200 hover:text-accent truncate mt-0.5 max-w-full" title={issue.title}>{issue.title}</Link>
                    <button type="button" className="board-preview-link mt-1" aria-label={`${t("Preview")} ${issue.id}`} onClick={() => onOpen(issue.id)}>{t("Preview")}</button>
                  </div>
                </td>
                {project === "all" && <td className="px-3 py-3 align-top num text-ink-400">{issue.project}</td>}
                <td className="px-3 py-3 align-top">
                  <span className={`chip ${STATUS_CHIP[issue.status] ?? "bg-ink-800 text-ink-400"}`}>{t(issue.status)}</span>
                </td>
                <td className="px-3 py-3 align-top num text-ink-300">{t(issue.priority)}</td>
                <td className="px-3 py-3 align-top num text-ink-400">{issue.owner ?? t("unassigned")}</td>
                <td className="px-3 py-3 align-top min-w-[11rem]">
                  <span className="text-ink-400">{issue.component ?? "—"}</span>
                  {(issue.tags ?? []).length > 0 && (
                    <div className="flex flex-wrap gap-1 mt-1">
                      {issue.tags!.map((tag) => <span key={tag} className="chip bg-ink-800 text-ink-500">#{tag}</span>)}
                    </div>
                  )}
                </td>
                <td className="px-3 py-3 align-top text-right num text-ink-400">
                  {formatNumber(issue.checks.done)}/{formatNumber(issue.checks.total)}
                  {issue.blocked && <span className="block text-fail text-micro">{t("blocked")}</span>}
                </td>
              </tr>
            ))}
            {rows.length === 0 && <tr><td colSpan={project === "all" ? 7 : 6} className="px-4 py-10 text-center text-ink-500">{t("No issues match this view.")}</td></tr>}
          </tbody>
        </table>
      </div>
    </section>
  );
}

interface Props {
  issues: ResourceState<IssueCard[]>;
  onRetry: () => void;
  projects: Project[];
  agents: AgentsPayload | null;
  health: Health | null;
  project: string;
  view: ProjectView;
  onView: (view: ProjectView) => void;
  query: string;
  readOnly: boolean;
  /** Undefined while metadata is unresolved; null when resolved without a session. */
  sessionId?: string | null;
  actor: string;
  onQuery: (q: string) => void;
  filters: BoardFilters;
  onFilters: (f: BoardFilters) => void;
  onOpen: (id: string) => void;
  onMove: (issue: IssueCard, status: string) => void;
  onCreated: (resp: WriteResp, verb: string) => void;
  onError: (e: unknown, verb: string) => void;
  onAgents: () => void;
}

export default function Board({
  issues: issuesState,
  onRetry,
  projects,
  agents,
  health,
  project,
  view,
  onView,
  query,
  readOnly,
  sessionId,
  actor,
  onQuery,
  filters,
  onFilters,
  onOpen,
  onMove,
  onCreated,
  onError,
  onAgents,
}: Props) {
  const { t, formatNumber, locale } = useLocale();
  const block = useWriteBlock(readOnly);
  const [over, setOver] = useState<string | null>(null);
  const [newOpen, setNewOpen] = useState(false);
  const [draftSession, setDraftSession] = useState<string | null>(null);
  const [draftProject, setDraftProject] = useState(project);
  const showNewForm = newOpen && (sessionId === undefined || sessionId === draftSession);
  useEffect(() => {
    if (newOpen && sessionId !== undefined && sessionId !== draftSession) setNewOpen(false);
  }, [newOpen, sessionId, draftSession]);
  const issues = issuesState.data ?? [];
  const loaded = issuesState.data !== null;
  // `scope` is what the project and the search box leave; the filter
  // bar counts over it and its chips narrow it to `visible`. Both come
  // from counts.ts, the same source as the sidebar and overview numbers.
  const scope = boardScope(issues, project, query);
  const visible = boardVisible(scope, filters, query);
  const counts = issueCounts(issues, project);
  const title =
    project === "all"
      ? "All projects"
      : projects.find((p) => p.key === project)?.key ?? project;

  const issueProjects = issueIndex(issues);
  // Card agent chips read the agents resource's `by_issue` — the same
  // strip `/api/issues` embeds — so an agent event refreshes them
  // without refetching every card.
  const byIssue = agents?.by_issue;
  const withAgents = (t: IssueCard): IssueCard =>
    byIssue ? { ...t, agents: byIssue[t.id] ?? [] } : t;
  const scopedAgents = (agents?.agents ?? []).filter((agent) =>
    agentMatchesProject(agent, project, issueProjects),
  );
  const globalAgents = (agents?.agents ?? []).filter((agent) =>
    agentIsUnassigned(agent, issueProjects),
  );

  // issue id → agent aliases currently running a turn that names it
  const busy = new Map<string, string[]>();
  for (const a of scopedAgents) {
    for (const id of a.on) {
      busy.set(id, [...(busy.get(id) ?? []), a.alias]);
    }
  }
  const titleOf = new Map(issues.map((i) => [i.id, i.title]));

  const totals = scopedAgents.reduce(
    (sum, agent) => ({
      running: sum.running + agent.running,
      queued: sum.queued + agent.queued,
      fenced: sum.fenced + (agent.fenced ? 1 : 0),
      parked: sum.parked + agent.parked,
      inboxes: sum.inboxes + (agent.inbox ? 1 : 0),
    }),
    { running: 0, queued: 0, fenced: 0, parked: 0, inboxes: 0 },
  );
  const fencedAgents = scopedAgents.filter((a) => a.fenced);
  const activeAgents = scopedAgents.filter(
    (a) => a.running > 0 || a.fenced,
  );
  const idleCount = scopedAgents.filter(
    (a) => a.state === "idle" && !a.fenced,
  ).length;
  const stoppedCount = scopedAgents.filter(
    (a) => a.state === "stopped" && !a.fenced,
  ).length;

  // The five status columns over one set of cards. `lane` keys the
  // drop-target highlight so swimlanes light up one cell, not a column.
  const columns = (laneCards: IssueCard[], lane: string) => (
    <div className="grid grid-flow-col auto-cols-[minmax(232px,78vw)] lg:auto-cols-[minmax(0,1fr)] gap-3 overflow-x-auto lg:overflow-visible pb-2 snap-x snap-mandatory lg:snap-none">
      {COLS.map(([key, name, wip], ci) => {
        const cards = laneCards
          .filter((t) => t.status === key)
          .sort(byPriority);
        // The Done column stays mounted as a drop target even while done
        // cards are hidden — collapsing it shows the count and a reveal.
        // A typed search already surfaces done cards, so the hint only
        // counts what the facet filters leave — matching every other
        // column's count semantics.
        const doneHidden =
          key === "done" && !filters.showDone && query === ""
            ? scope.filter(
                (t) =>
                  t.status === "done" &&
                  matches(filters, t) &&
                  (lane === "" ||
                    (lane === "none" ? !t.parent : t.parent === lane)),
              ).length
            : 0;
        const cell = `${lane}:${key}`;
        return (
          <section
            key={key}
            className={`snap-start rounded-lg border bg-ink-875 ${
              lane === "" ? "min-h-[26rem]" : "min-h-[7rem]"
            } flex flex-col reveal transition-colors ${
              over === cell ? "border-accent/60" : "border-ink-700"
            }`}
            style={{ animationDelay: `${120 + ci * 45}ms` }}
            onDragOver={(e) => {
              if (readOnly) return;
              e.preventDefault();
              e.dataTransfer.dropEffect = "move";
              setOver(cell);
            }}
            onDragLeave={(e) => {
              if (!e.currentTarget.contains(e.relatedTarget as Node)) {
                setOver((o) => (o === cell ? null : o));
              }
            }}
            onDrop={(e) => {
              e.preventDefault();
              setOver(null);
              if (readOnly) return;
              const id = e.dataTransfer.getData("text/plain");
              const issue = issues.find((t) => t.id === id);
              if (!issue || issue.status === key) return;
              const reason = noDragReason(issue);
              if (reason) {
                onError(new Error(`${id}: ${reason}`), "move");
                return;
              }
              onMove(issue, key);
            }}
          >
            <header
              className={`${
                lane === "" ? "lg:sticky lg:top-[2.85rem] z-[5] " : ""
              }flex items-center gap-2 px-3.5 h-10 border-b border-ink-700 shrink-0 bg-ink-875 rounded-t-lg`}
            >
              <i className={`w-1.5 h-1.5 rounded-full ${COL_DOT[key]}`} />
              <h2 className="text-secondary font-semibold text-ink-100">
                {t(name)}
              </h2>
              <span className="kicker num">
                {doneHidden > 0 ? doneHidden : cards.length}
                {/* The WIP limit is board-wide — a lane shows its count only. */}
                {wip && lane === "" ? ` · limit ${wip}` : ""}
              </span>
            </header>
            <div className="p-2.5 space-y-2.5 flex-1">
              {doneHidden > 0 ? (
                <button
                  onClick={() => onFilters({ ...filters, showDone: true })}
                  className="kicker px-1 py-2 text-left hover:text-accent transition-colors"
                >
                  {formatNumber(doneHidden)} {t("done hidden — show")}
                </button>
              ) : cards.length === 0 ? (
                <p className="kicker px-1 py-2">{t("empty")}</p>
              ) : (
                cards.map((t) => (
                  <Card
                    key={t.id}
                    issue={withAgents(t)}
                    parentTitle={
                      t.parent ? titleOf.get(t.parent) : undefined
                    }
                    busyBy={busy.get(t.id) ?? []}
                    canDrag={!readOnly}
                    onOpen={onOpen}
                  />
                ))
              )}
            </div>
          </section>
        );
      })}
    </div>
  );

  // Swimlanes: one lane per epic that still has a visible child, then
  // the cards that belong to no epic.
  const epicIds = [...new Set(visible.map((t) => t.parent ?? ""))]
    .filter(Boolean)
    .sort((a, b) => a.localeCompare(b, undefined, { numeric: true }));
  const loose = visible.filter((t) => !t.parent);

  return (
    <main className="issues-board px-4 lg:px-8 pt-6 pb-9 w-full">
      {health && !health.pm_present && (
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 mb-5 reveal">
          <span className="chip bg-warn/10 text-warn">{t("no pm dir")}</span>
          <span className="text-secondary text-ink-400">
            {t("Nothing at")} {health.pm_dir ?? "~/pm"} {t("yet")} —{" "}
            <span className="num">cadence issue init</span> creates it.
          </span>
        </div>
      )}

      {fencedAgents.length > 0 && (
        <details className="card mb-4 px-4 py-3 border-warn/30 reveal"><summary className="text-label text-warn cursor-pointer">{formatNumber(fencedAgents.length)} {t("agents need attention · View details")}</summary><div className="flex flex-wrap items-center gap-x-4 gap-y-2 mt-3">
          <span className="chip bg-fail/10 text-fail">
            {formatNumber(fencedAgents.length)} {t("fenced")}
          </span>
          <span className="text-secondary text-ink-300 min-w-0">
            {fencedAgents.map((a) => a.alias).join(", ")} —{" "}
            {fencedAgents[0]?.recovery ??
              t("outcomes are uncertain until an operator reconciles")}
          </span>
          <button
            onClick={onAgents}
            className="chip bg-fail/10 text-fail hover:bg-fail/20 transition-colors ml-auto"
          >
            {t("open Agents")} →
          </button>
        </div></details>
      )}

      <div
        className="flex flex-wrap items-end gap-x-3 gap-y-3 mb-4 reveal"
        style={{ animationDelay: "40ms" }}
      >
        <h1 className="text-section font-semibold text-ink-100 leading-tight">
          {title}
        </h1>
        <span className="kicker" title={t("epics are excluded — their status rolls up from the issues counted")}>
          {loaded
            ? (() => {
                const active = visible.filter((t) => ["doing", "review"].includes(t.status)).length;
                const blocked = visible.filter((t) => t.blocked).length;
                if (locale === "en") return `${boardHeadline(counts, visible, filters, query)} · ${active} active · ${blocked} blocked`;
                const narrowed = query !== "" || activeCount(filters) > 0;
                const shownOpen = visible.filter((t) => t.status !== "done").length;
                const parts = [narrowed
                  ? `${formatNumber(shownOpen)} / ${formatNumber(counts.open)} ${t("open issues")}`
                  : `${formatNumber(counts.open)} ${t("open issues")}`];
                if (counts.done > 0) parts.push(`${formatNumber(counts.done)} ${t(filters.showDone ? "done shown" : "done hidden")}`);
                if (counts.dropped > 0) parts.push(`${formatNumber(counts.dropped)} ${t("dropped issues")}`);
                if (counts.containers > 0) parts.push(`${formatNumber(counts.containers)} ${t("epics")}`);
                parts.push(`${formatNumber(active)} ${t("active issues")}`, `${formatNumber(blocked)} ${t("blocked issues")}`);
                return parts.join(" · ");
              })()
            : "…"}
        </span>
        <StaleChip state={issuesState} />
        <div className="ml-auto flex items-center gap-2">
          {!readOnly && !showNewForm && <Button variant="primary" onClick={() => { setDraftSession(sessionId ?? null); setDraftProject(project); setNewOpen(true); }}>{t("New issue")}</Button>}
          <div className="flex items-center gap-1 rounded border border-ink-700 p-0.5" role="group" aria-label={t("Project view")}>
            {(["kanban", "list"] as const).map((mode) => (
              <button
                key={mode}
                aria-pressed={view === mode}
                onClick={() => onView(mode)}
                className={`chip !py-[.25rem] capitalize ${view === mode ? "bg-accent/10 text-accent" : "text-ink-500 hover:text-ink-200"}`}
              >
                {t(mode === "kanban" ? "board" : "list")}
              </button>
            ))}
          </div>
        </div>
        <input
          value={query}
          onChange={(e) => onQuery(e.target.value)}
          className="field w-full sm:w-64"
          aria-label={t("Search issues, owners or tags")}
          placeholder={t("Search issues, owners or tags")}
        />
      </div>

      {showNewForm && (
        <NewIssueForm key={draftSession ?? "no-session"} projects={projects} project={draftProject} readOnly={readOnly} writeReason={block} onCreated={onCreated} onError={onError} onCancel={() => setNewOpen(false)} />
      )}

      <details className="mb-4"><summary className="text-label text-ink-400 cursor-pointer">{t("Team status")} · {totals?.running == null ? "—" : formatNumber(totals.running)} {t("running")} · {totals?.queued == null ? "—" : formatNumber(totals.queued)} {t("queued")}{fencedAgents.length > 0 ? ` · ${formatNumber(fencedAgents.length)} ${t("need attention")}` : ""}</summary><div className="mt-3">      <section
        className="card mb-4 px-4 py-2.5 flex flex-wrap items-center gap-x-5 gap-y-2 reveal"
        style={{ animationDelay: "80ms" }}
        aria-label={t("Runtime")}
      >
        <div className="flex items-baseline gap-x-5 gap-y-1 flex-wrap">
          <span className="flex items-baseline gap-1.5">
            <span className="slabel">{t("running")}</span>
            <span className="num text-secondary text-ink-100">
              {totals?.running == null ? "—" : formatNumber(totals.running)}
            </span>
          </span>
          <span className="flex items-baseline gap-1.5">
            <span className="slabel">{t("queued")}</span>
            <span className="num text-secondary text-ink-100">
              {totals?.queued == null ? "—" : formatNumber(totals.queued)}
            </span>
          </span>
          <span className="flex items-baseline gap-1.5">
            <span className="slabel">{t("fenced")}</span>
            <span
              className={`num text-secondary ${
                (totals?.fenced ?? 0) > 0 ? "text-fail" : "text-ok"
              }`}
            >
              {totals?.fenced == null ? "—" : formatNumber(totals.fenced)}
            </span>
          </span>
          <span className="flex items-baseline gap-1.5">
            <span className="slabel">{t("parked")}</span>
            <span className="num text-secondary text-ink-100">
              {totals?.parked == null ? "—" : formatNumber(totals.parked)}
            </span>
          </span>
          {(totals?.inboxes ?? 0) > 0 && (
            <span
              className="flex items-baseline gap-1.5"
              title={t("inbox endpoints are mailboxes, not workers")}
            >
              <span className="slabel">{t("inboxes")}</span>
              <span className="num text-secondary text-ink-400">
                {formatNumber(totals.inboxes)}
              </span>
            </span>
          )}
        </div>
        <span className="hidden md:block w-px h-4 bg-ink-700" />
        <div
          className="flex items-center gap-2 min-w-0 flex-wrap"
          title={
            agents?.daemon === "reachable"
              ? t("read from the daemon socket")
              : t("daemon socket unreachable")
          }
        >
          <span className="slabel">{t("agents")}</span>
          <div className="flex flex-wrap gap-1.5">
            {agents?.daemon === "unreachable" && (
              <span className="chip bg-ink-800 !py-[.15rem] text-ink-500">
                <i className="w-1.5 h-1.5 rounded-full bg-ink-600" />
                {t("daemon unreachable")}
              </span>
            )}
            {activeAgents.map((a) => (
              <span
                key={a.alias}
                className="chip bg-ink-800 !py-[.15rem] text-ink-300"
              >
                <i
                  className={`w-1.5 h-1.5 rounded-full ${
                    a.fenced ? AGENT_DOT.attention : AGENT_DOT.busy
                  }`}
                />
                {a.alias}
                <span className="text-ink-500">
                  {t(a.fenced ? "fenced" : "busy")}
                </span>
                {a.on.map((id) => (
                  <button
                    key={id}
                    className="lnk"
                    onClick={() => onOpen(id)}
                  >
                    {id}
                  </button>
                ))}
              </span>
            ))}
            {idleCount > 0 && (
              <span
                className="chip bg-ink-800 !py-[.15rem] text-ink-400"
                title={scopedAgents
                  .filter((a) => a.state === "idle" && !a.fenced)
                  .map((a) => a.alias)
                  .join(", ")}
              >
                <i className="w-1.5 h-1.5 rounded-full bg-ok" />
                <span className="num text-ink-200">{formatNumber(idleCount)}</span>{t("idle")}
              </span>
            )}
            {stoppedCount > 0 && (
              <span
                className="chip bg-ink-800 !py-[.15rem] text-ink-400"
                title={scopedAgents
                  .filter((a) => a.state === "stopped" && !a.fenced)
                  .map((a) => a.alias)
                  .join(", ")}
              >
                <i className="w-1.5 h-1.5 rounded-full bg-ink-600" />
                <span className="num text-ink-200">{formatNumber(stoppedCount)}</span>{t("stopped")}
              </span>
            )}
            {project !== "all" && globalAgents.length > 0 && (
              <span
                className="chip bg-ink-800 !py-[.15rem] text-ink-500"
                title={t("agents without an exact issue binding remain global")}
              >
                {formatNumber(globalAgents.length)} {t("global/unassigned")}
              </span>
            )}
          </div>
        </div>
      </section>
</div></details>

      <ResourceGate
        state={issuesState}
        loading="loading issues — reading the tracker can take several seconds"
        failed="could not load issues"
        onRetry={onRetry}
      />
      {issuesState.status === "empty" && (
        <p className="kicker mb-4" role="status">
          {t(`the tracker has no issues yet${readOnly ? "" : " — use New issue to create one"}`)}
        </p>
      )}

      {loaded && (
        <FilterBar
          scope={scope}
          issues={issues}
          filters={filters}
          onChange={onFilters}
          view={view}
        />
      )}

      {!loaded ? null : view === "list" ? (
        <IssueList issues={visible} project={project} onOpen={onOpen} />
      ) : !filters.groupByEpic ? (
        columns(visible, "")
      ) : (
        <div className="space-y-5">
          {epicIds.map((epic) => {
            const p = epicProgress(issues, epic);
            const pct = Math.round(p.ratio * 100);
            return (
              <section key={epic} aria-label={`${t("Epic")} ${epic}`}>
                <header className="flex flex-wrap items-center gap-x-3 gap-y-1 mb-2">
                  <button
                    className="lnk num text-label"
                    onClick={() => onOpen(epic)}
                  >
                    {epic}
                  </button>
                  <h2 className="text-secondary font-semibold text-ink-100 min-w-0 truncate">
                    {titleOf.get(epic) ?? t("unknown epic")}
                  </h2>
                  <div
                    className="ml-auto flex items-center gap-2"
                    title={`${formatNumber(p.done)} ${t("of")} ${formatNumber(p.total)} ${t("children done (dropped excluded)")}`}
                  >
                    <div
                      className="w-32 h-1.5 rounded-full bg-ink-700 overflow-hidden"
                      role="progressbar"
                      aria-valuemin={0}
                      aria-valuemax={100}
                      aria-valuenow={pct}
                    >
                      <div
                        className="h-full bg-ok transition-[width]"
                        style={{ width: `${pct}%` }}
                      />
                    </div>
                    <span className="kicker num">
                      {p.done}/{p.total} · {pct}%
                    </span>
                  </div>
                </header>
                {columns(
                  visible.filter((t) => t.parent === epic),
                  epic,
                )}
              </section>
            );
          })}
          <section aria-label={t("No epic")}>
            <header className="flex items-center gap-x-3 mb-2">
              <h2 className="text-secondary font-semibold text-ink-300">
                {t("No epic")}
              </h2>
              <span className="kicker num">{loose.length}</span>
            </header>
            {columns(loose, "none")}
          </section>
        </div>
      )}

      <footer className="mt-8 pt-4 border-t border-ink-700 text-label text-ink-500 num">
        {t("source:")} {health?.pm_dir ?? "~/pm"} {t("issue folders")} ·{" "}
        /var/www/agent-notes {t("chains")} · {t("cadence daemon socket")} ·{" "}
        {readOnly
          ? `${t("writes are disabled")} — ${block}`
          : `${t("writes commit as")} ${actor}.`}
      </footer>
    </main>
  );
}
