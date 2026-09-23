import { useState } from "react";
import { api, type WriteResp } from "../api";
import { epicProgress, matches, type BoardFilters } from "../filters";
import { agentIsUnassigned, agentMatchesProject, issueProjectMap } from "../scope";
import type { AgentsPayload, Health, IssueCard, Project } from "../types";
import type { ProjectView } from "../urlState";
import Card, { noDragReason } from "./Card";
import FilterBar from "./FilterBar";

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
    <section className="card overflow-hidden reveal" aria-label="Project issue list">
      <div className="sm:hidden divide-y divide-ink-700/70">
        {rows.map((issue) => (
          <button key={issue.id} className="w-full text-left px-3.5 py-3.5 hover:bg-ink-850" onClick={() => onOpen(issue.id)}>
            <div className="flex items-center gap-2">
              <span className="lnk num text-label">{issue.id}</span>
              <span className={`chip ${STATUS_CHIP[issue.status] ?? "bg-ink-800 text-ink-400"}`}>{issue.status}</span>
              <span className="num text-micro text-ink-500 ml-auto">{issue.priority}</span>
            </div>
            <div className="text-ink-200 mt-1.5 leading-[1.4]">{issue.title}</div>
            <div className="flex flex-wrap gap-x-3 gap-y-1 mt-2 text-micro text-ink-500">
              <span>{issue.owner ?? "unassigned"}</span>
              <span>{issue.component ?? "no component"}</span>
              <span>{issue.checks.done}/{issue.checks.total} checks</span>
              {project === "all" && <span>{issue.project}</span>}
            </div>
          </button>
        ))}
        {rows.length === 0 && <div className="px-4 py-10 text-center text-ink-500">No issues match this view.</div>}
      </div>
      <div className="hidden sm:block overflow-x-auto">
        <table className="w-full min-w-[46rem] text-label">
          <thead>
            <tr className="border-b border-ink-700 text-left">
              <th className="slabel font-normal px-4 py-2.5">{header("id", "issue")}</th>
              {project === "all" && <th className="slabel font-normal px-3 py-2.5">project</th>}
              <th className="slabel font-normal px-3 py-2.5">{header("status", "status")}</th>
              <th className="slabel font-normal px-3 py-2.5">{header("priority", "priority")}</th>
              <th className="slabel font-normal px-3 py-2.5">{header("owner", "owner")}</th>
              <th className="slabel font-normal px-3 py-2.5">component / tags</th>
              <th className="slabel font-normal px-3 py-2.5 text-right">checks</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-ink-700/70">
            {rows.map((issue) => (
              <tr key={issue.id} className="hover:bg-ink-850 transition-colors">
                <td className="px-4 py-3 min-w-0 max-w-[28rem]">
                  <button className="text-left min-w-0" onClick={() => onOpen(issue.id)}>
                    <span className="lnk num">{issue.id}</span>
                    <span className="block text-ink-200 truncate mt-0.5" title={issue.title}>{issue.title}</span>
                  </button>
                </td>
                {project === "all" && <td className="px-3 py-3 align-top num text-ink-400">{issue.project}</td>}
                <td className="px-3 py-3 align-top">
                  <span className={`chip ${STATUS_CHIP[issue.status] ?? "bg-ink-800 text-ink-400"}`}>{issue.status}</span>
                </td>
                <td className="px-3 py-3 align-top num text-ink-300">{issue.priority}</td>
                <td className="px-3 py-3 align-top num text-ink-400">{issue.owner ?? "unassigned"}</td>
                <td className="px-3 py-3 align-top min-w-[11rem]">
                  <span className="text-ink-400">{issue.component ?? "—"}</span>
                  {(issue.tags ?? []).length > 0 && (
                    <div className="flex flex-wrap gap-1 mt-1">
                      {issue.tags!.map((tag) => <span key={tag} className="chip bg-ink-800 text-ink-500">#{tag}</span>)}
                    </div>
                  )}
                </td>
                <td className="px-3 py-3 align-top text-right num text-ink-400">
                  {issue.checks.done}/{issue.checks.total}
                  {issue.blocked && <span className="block text-fail text-micro">blocked</span>}
                </td>
              </tr>
            ))}
            {rows.length === 0 && <tr><td colSpan={project === "all" ? 7 : 6} className="px-4 py-10 text-center text-ink-500">No issues match this view.</td></tr>}
          </tbody>
        </table>
      </div>
    </section>
  );
}

interface Props {
  issues: IssueCard[];
  projects: Project[];
  agents: AgentsPayload | null;
  health: Health | null;
  project: string;
  view: ProjectView;
  onView: (view: ProjectView) => void;
  query: string;
  readOnly: boolean;
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

/// Quick-add at the top of Backlog: title + project + priority, Enter
/// creates, Escape cancels.
function QuickAdd({
  projects,
  project,
  onCreated,
  onError,
}: {
  projects: Project[];
  project: string;
  onCreated: (resp: WriteResp, verb: string) => void;
  onError: (e: unknown, verb: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [title, setTitle] = useState("");
  const [priority, setPriority] = useState("P2");
  const [selProject, setSelProject] = useState("");
  const [busy, setBusy] = useState(false);
  const projectKey =
    project === "all" ? selProject || (projects[0]?.key ?? "") : project;

  if (!open) {
    return (
      <button
        onClick={() => setOpen(true)}
        className="w-full rounded border border-dashed border-ink-600 px-3 py-2 text-left text-label text-ink-500 hover:border-accent/60 hover:text-accent transition-colors"
      >
        + new issue
      </button>
    );
  }
  const submit = () => {
    const t = title.trim();
    if (!t || !projectKey || busy) return;
    setBusy(true);
    api
      .create({ project: projectKey, title: t, priority })
      .then((resp) => {
        onCreated(resp, `${resp.card.id} created`);
        setTitle("");
        setOpen(false);
      })
      .catch((e) => onError(e, "create"))
      .finally(() => setBusy(false));
  };
  return (
    <div className="card p-2.5 space-y-2 border-accent/40">
      <input
        autoFocus
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") submit();
          if (e.key === "Escape") {
            setTitle("");
            setOpen(false);
          }
        }}
        className="field w-full"
        placeholder={`title — creates in ${projectKey}`}
      />
      <div className="flex gap-1.5">
        {project === "all" ? (
          <select
            value={projectKey}
            onChange={(e) => setSelProject(e.target.value)}
            className="field !h-7 flex-1 text-label"
            aria-label="project"
          >
            {projects.map((p) => (
              <option key={p.key} value={p.key}>
                {p.key}
              </option>
            ))}
          </select>
        ) : (
          <span className="chip bg-ink-800 text-ink-400 self-center">
            {projectKey}
          </span>
        )}
        <select
          value={priority}
          onChange={(e) => setPriority(e.target.value)}
          className="field !h-7 text-label"
          aria-label="priority"
        >
          {["P0", "P1", "P2", "P3"].map((p) => (
            <option key={p}>{p}</option>
          ))}
        </select>
        <button
          onClick={submit}
          disabled={!title.trim() || busy}
          className="h-7 px-2.5 rounded bg-accent text-ink-950 text-label font-medium disabled:opacity-40"
        >
          add
        </button>
      </div>
    </div>
  );
}

export default function Board({
  issues,
  projects,
  agents,
  health,
  project,
  view,
  onView,
  query,
  readOnly,
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
  const [over, setOver] = useState<string | null>(null);
  // `scope` is what the project and the search box leave; the filter
  // bar counts over it and its chips narrow it to `visible`.
  const scope = issues.filter(
    (t) =>
      !t.container &&
      t.status !== "dropped" &&
      (project === "all" || t.project === project) &&
      (!query ||
        (t.id + t.title + (t.owner ?? "") + (t.tags ?? []).join(" "))
          .toLowerCase()
          .includes(query.toLowerCase())),
  );
  // Finished work stays out of the default view; typing a search looks
  // through it anyway since a query means "find this", not "browse".
  const visible = scope.filter(
    (t) =>
      matches(filters, t) &&
      (filters.showDone || t.status !== "done" || query !== ""),
  );
  const title =
    project === "all"
      ? "All projects"
      : projects.find((p) => p.key === project)?.key ?? project;

  const issueProjects = issueProjectMap(issues);
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
  const columns = (laneCards: IssueCard[], lane: string, quickAdd: boolean) => (
      <div className="grid grid-flow-col auto-cols-[minmax(232px,78vw)] lg:auto-cols-[minmax(0,1fr)] gap-3 overflow-x-auto lg:overflow-visible pb-2 snap-x snap-mandatory lg:snap-none">
      {COLS.map(([key, name, wip], ci) => {
        const showAdd = quickAdd && !readOnly;
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
                {name}
              </h2>
              <span className="kicker num">
                {doneHidden > 0 ? doneHidden : cards.length}
                {/* The WIP limit is board-wide — a lane shows its count only. */}
                  {wip && lane === "" ? ` of ${wip} wip` : ""}
              </span>
            </header>
            <div className="p-2.5 space-y-2.5 flex-1">
              {key === "backlog" && showAdd && (
                <QuickAdd
                  projects={projects}
                  project={project}
                  onCreated={onCreated}
                  onError={onError}
                />
              )}
              {doneHidden > 0 ? (
                <button
                  onClick={() => onFilters({ ...filters, showDone: true })}
                  className="kicker px-1 py-2 text-left hover:text-accent transition-colors"
                >
                  {doneHidden} done hidden — show
                </button>
              ) : cards.length === 0 && !(key === "backlog" && showAdd) ? (
                <p className="kicker px-1 py-2">empty</p>
              ) : (
                cards.map((t) => (
                  <Card
                    key={t.id}
                    issue={t}
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
    <main className="px-4 lg:px-8 pt-6 pb-9 max-w-[106rem] w-full">
      {health && !health.pm_present && (
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 mb-5 reveal">
          <span className="chip bg-warn/10 text-warn">no pm dir</span>
          <span className="text-secondary text-ink-400">
            Nothing at {health.pm_dir ?? "~/pm"} yet —{" "}
            <span className="num">cadence issue init</span> creates it.
          </span>
        </div>
      )}

      {fencedAgents.length > 0 && (
        <div className="card mb-4 px-4 py-3 flex flex-wrap items-center gap-x-4 gap-y-2 border-fail/40 reveal">
          <span className="chip bg-fail/10 text-fail">
            {fencedAgents.length} fenced
          </span>
          <span className="text-secondary text-ink-300 min-w-0">
            {fencedAgents.map((a) => a.alias).join(", ")} —{" "}
            {fencedAgents[0]?.recovery ??
              "outcomes are uncertain until an operator reconciles"}
          </span>
          <button
            onClick={onAgents}
            className="chip bg-fail/10 text-fail hover:bg-fail/20 transition-colors ml-auto"
          >
            open Agents →
          </button>
        </div>
      )}

      <div
        className="flex flex-wrap items-end gap-x-3 gap-y-3 mb-4 reveal"
        style={{ animationDelay: "40ms" }}
      >
        <h1 className="text-section font-semibold text-ink-100 leading-tight">
          {title}
        </h1>
        <span className="kicker">
          {visible.length} issues · {visible.filter((t) => ["doing", "review"].includes(t.status)).length} active · {visible.filter((t) => t.blocked).length} blocked
        </span>
        <div className="ml-auto flex items-center gap-1 rounded border border-ink-700 p-0.5" role="group" aria-label="Project view">
          {(["kanban", "list"] as const).map((mode) => (
            <button
              key={mode}
              aria-pressed={view === mode}
              onClick={() => onView(mode)}
              className={`chip !py-[.25rem] capitalize ${view === mode ? "bg-accent/10 text-accent" : "text-ink-500 hover:text-ink-200"}`}
            >
              {mode === "kanban" ? "board" : "list"}
            </button>
          ))}
        </div>
        <input
          value={query}
          onChange={(e) => onQuery(e.target.value)}
          className="field w-full sm:w-64"
          placeholder="Search issues, owners or tags"
        />
      </div>

      <section
        className="card mb-4 px-4 py-2.5 flex flex-wrap items-center gap-x-5 gap-y-2 reveal"
        style={{ animationDelay: "80ms" }}
        aria-label="Runtime"
      >
        <div className="flex items-baseline gap-x-5 gap-y-1 flex-wrap">
          <span className="flex items-baseline gap-1.5">
            <span className="slabel">running</span>
            <span className="num text-secondary text-ink-100">
              {totals?.running ?? "—"}
            </span>
          </span>
          <span className="flex items-baseline gap-1.5">
            <span className="slabel">queued</span>
            <span className="num text-secondary text-ink-100">
              {totals?.queued ?? "—"}
            </span>
          </span>
          <span className="flex items-baseline gap-1.5">
            <span className="slabel">fenced</span>
            <span
              className={`num text-secondary ${
                (totals?.fenced ?? 0) > 0 ? "text-fail" : "text-ok"
              }`}
            >
              {totals?.fenced ?? "—"}
            </span>
          </span>
          <span className="flex items-baseline gap-1.5">
            <span className="slabel">parked</span>
            <span className="num text-secondary text-ink-100">
              {totals?.parked ?? "—"}
            </span>
          </span>
          {(totals?.inboxes ?? 0) > 0 && (
            <span
              className="flex items-baseline gap-1.5"
              title="inbox endpoints are mailboxes, not workers"
            >
              <span className="slabel">inboxes</span>
              <span className="num text-secondary text-ink-400">
                {totals?.inboxes}
              </span>
            </span>
          )}
        </div>
        <span className="hidden md:block w-px h-4 bg-ink-700" />
        <div
          className="flex items-center gap-2 min-w-0 flex-wrap"
          title={
            agents?.daemon === "reachable"
              ? "read from the daemon socket"
              : "daemon socket unreachable"
          }
        >
          <span className="slabel">agents</span>
          <div className="flex flex-wrap gap-1.5">
            {agents?.daemon === "unreachable" && (
              <span className="chip bg-ink-800 !py-[.15rem] text-ink-500">
                <i className="w-1.5 h-1.5 rounded-full bg-ink-600" />
                daemon unreachable
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
                  {a.fenced ? "fenced" : "busy"}
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
                <span className="num text-ink-200">{idleCount}</span>idle
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
                <span className="num text-ink-200">{stoppedCount}</span>stopped
              </span>
            )}
            {project !== "all" && globalAgents.length > 0 && (
              <span
                className="chip bg-ink-800 !py-[.15rem] text-ink-500"
                title="agents without an exact issue binding remain global"
              >
                {globalAgents.length} global/unassigned
              </span>
            )}
          </div>
        </div>
      </section>

      <FilterBar
        scope={scope}
        issues={issues}
        filters={filters}
        onChange={onFilters}
      />

      {view === "list" ? (
        <IssueList issues={visible} project={project} onOpen={onOpen} />
      ) : !filters.groupByEpic ? (
        columns(visible, "", true)
      ) : (
        <div className="space-y-5">
          {epicIds.map((epic) => {
            const p = epicProgress(issues, epic);
            const pct = Math.round(p.ratio * 100);
            return (
              <section key={epic} aria-label={`Epic ${epic}`}>
                <header className="flex flex-wrap items-center gap-x-3 gap-y-1 mb-2">
                  <button
                    className="lnk num text-label"
                    onClick={() => onOpen(epic)}
                  >
                    {epic}
                  </button>
                  <h2 className="text-secondary font-semibold text-ink-100 min-w-0 truncate">
                    {titleOf.get(epic) ?? "unknown epic"}
                  </h2>
                  <div
                    className="ml-auto flex items-center gap-2"
                    title={`${p.done} of ${p.total} children done (dropped excluded)`}
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
                  false,
                )}
              </section>
            );
          })}
          <section aria-label="No epic">
            <header className="flex items-center gap-x-3 mb-2">
              <h2 className="text-secondary font-semibold text-ink-300">
                No epic
              </h2>
              <span className="kicker num">{loose.length}</span>
            </header>
            {columns(loose, "none", true)}
          </section>
        </div>
      )}

      <footer className="mt-8 pt-4 border-t border-ink-700 text-label text-ink-500 num">
        source: {health?.pm_dir ?? "~/pm"} issue folders ·{" "}
        /var/www/agent-notes chains · cadence daemon socket ·{" "}
        {readOnly
          ? "read-only — writes are disabled."
          : `writes commit as ${actor}.`}
      </footer>
    </main>
  );
}
