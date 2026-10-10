import { useState } from "react";
import type { ResourceState } from "../../lib/cache";
import {
  agentIsUnassigned,
  agentIssueIds,
  agentMatchesProject,
  issueIndex,
  type IssueIndex,
} from "../../lib/scope";
import { agentsEmptyCopy } from "../../lib/uxCopy";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import { IconSearch } from "../../ui/icons";
import Button from "../../ui/Button";
import { activeTasks, AgentActivity, WorkBlock } from "./AgentBlocks";
import AgentAvatar, { lookFor } from "./AgentAvatar";
import AgentDrawer from "./AgentDrawer";
import {
  AGENT_FILTERS,
  agentHoldsWorkButDead,
  agentMatchesFilter,
  agentMatchesSearch,
  agentOrder,
  LIFECYCLE_SECTIONS,
  lifecycleBadge,
  lifecycleOf,
  type AgentFilter,
} from "./agentView";
import "./agents.css";
import type { Agent, AgentsPayload, IssueCard } from "../../lib/types";

/** Exact assigned/owned issue links not already shown with current work. */
function RelatedIssues({
  agent,
  index,
  onOpenIssue,
}: {
  agent: Agent;
  index: IssueIndex;
  onOpenIssue: (id: string) => void;
}) {
  const viaTask = new Set(activeTasks(agent).map((task) => task.issue));
  const ids = agentIssueIds(agent, index).filter((id) => !viaTask.has(id));
  if (ids.length === 0) return null;
  return (
    <div className="num text-micro text-ink-500 mt-1 flex flex-wrap items-center gap-x-1.5">
      Issues
      {ids.map((id) => (
        <button
          key={id}
          className="lnk"
          onClick={(event) => {
            event.stopPropagation();
            onOpenIssue(id);
          }}
        >
          {id}
        </button>
      ))}
    </div>
  );
}

/** The card's work block: first active task as issue link + clamped title
 * + mono state line; the shared WorkBlock covers every other state. */
function CardWork({
  agent,
  onOpenIssue,
}: {
  agent: Agent;
  onOpenIssue: (id: string) => void;
}) {
  const task = activeTasks(agent)[0];
  if (!task)
    return (
      <div className="work">
        <WorkBlock agent={agent} onOpenIssue={onOpenIssue} />
      </div>
    );
  const runningTaskIds = new Set(
    (agent.running_messages ?? [])
      .map((message) => message.task)
      .filter(Boolean),
  );
  const live = runningTaskIds.has(task.task);
  return (
    <div className="work">
      <div className="flex items-start gap-1.5 min-w-0">
        <i
          className={`w-1.5 h-1.5 rounded-full shrink-0 mt-1.5 ${live ? "bg-info" : "bg-ink-600"}`}
        />
        {task.issue && (
          <button
            className="lnk num shrink-0"
            onClick={(event) => {
              event.stopPropagation();
              onOpenIssue(task.issue);
            }}
          >
            {task.issue}
          </button>
        )}
        <span className="work-tt text-ink-300" title={task.title ?? task.task}>
          {task.title ?? task.task}
        </span>
      </div>
      <div className="num text-micro text-ink-500 pl-3">
        {live ? "running" : task.task_state}
        {task.job_title
          ? ` · ${task.job_title}`
          : task.job
            ? ` · ${task.job}`
            : ""}
        {task.job_state ? ` · stage ${task.job_state}` : ""}
        {activeTasks(agent).length > 1
          ? ` · +${activeTasks(agent).length - 1} more`
          : ""}
      </div>
    </div>
  );
}

/** One catalog card: avatar top-center, name/meta, work, lifecycle badge
 * bottom-left opposite the queued chip, lifecycle-coloured top edge. */
function AgentCard({
  agent: a,
  index,
  onOpen,
  onOpenIssue,
}: {
  agent: Agent;
  index: IssueIndex;
  onOpen: () => void;
  onOpenIssue: (id: string) => void;
}) {
  const lifecycle = lifecycleOf(a);
  const tone = LIFECYCLE_SECTIONS.find((s) => s.key === lifecycle)!.tone;
  const badge = lifecycleBadge(a);
  return (
    <article
      className={`agent-card tone-${tone}`}
      onClick={onOpen}
      aria-label={`Agent ${a.alias}`}
    >
      <div className="top">
        <div className="avwrap">
          <AgentAvatar
            slug={lookFor(a.alias)}
            active={lifecycle === "active"}
            still={lifecycle !== "active"}
          />
          {agentHoldsWorkButDead(a) && (
            <span className="agent-hold" title="Dead but still holds work">
              !
            </span>
          )}
        </div>
        <div className="min-w-0">
          <button
            type="button"
            className="agent-name name"
            onClick={(event) => {
              event.stopPropagation();
              onOpen();
            }}
            aria-label={`Open agent ${a.alias}`}
          >
            {a.alias}
          </button>
          <p className="meta break-words">
            {a.role ?? "Agent"} · {a.provider} ·{" "}
            {a.group_root ? "root" : a.group}
          </p>
        </div>
      </div>
      <CardWork agent={a} onOpenIssue={onOpenIssue} />
      <RelatedIssues agent={a} index={index} onOpenIssue={onOpenIssue} />
      <div className="-mt-2 text-micro">
        <AgentActivity agent={a} />
      </div>
      <div className="foot">
        <span className={`stateb tone-${badge.tone}`}>
          <i aria-hidden="true" />
          {badge.label}
        </span>
        <span className={`agent-qchip${a.queued >= 100 ? " hot" : ""}`}>
          {a.queued} queued
        </span>
      </div>
    </article>
  );
}

/** Triage strip cell → the filter it applies. Dead and idle holdings
 * share the "holding" filter; the sections split them again. */
const TRIAGE: {
  label: string;
  tone: "fail" | "warn" | "info" | "ok";
  filter: AgentFilter;
  count: (a: Agent) => boolean;
}[] = [
  {
    label: "Dead · holding work",
    tone: "fail",
    filter: "holding",
    count: (a) => lifecycleOf(a) === "dead-holding",
  },
  {
    label: "Idle · holding claim",
    tone: "warn",
    filter: "holding",
    count: (a) => lifecycleOf(a) === "idle-holding",
  },
  {
    label: "Stale inbox",
    tone: "info",
    filter: "inbox",
    count: (a) => lifecycleOf(a) === "stale-inbox",
  },
  {
    label: "Needs attention",
    tone: "warn",
    filter: "attention",
    count: (a) => agentMatchesFilter(a, "attention"),
  },
  {
    label: "Working",
    tone: "ok",
    filter: "working",
    count: (a) => lifecycleOf(a) === "active",
  },
];

export default function Agents({
  state,
  issues: issuesState,
  project,
  open,
  onOpenAgent: setOpen,
  onOpenIssue,
  onRetry,
  onRetryAssignments,
}: {
  state: ResourceState<AgentsPayload>;
  issues: ResourceState<IssueCard[]>;
  project: string;
  open: string | null;
  onOpenAgent: (alias: string | null) => void;
  onOpenIssue: (id: string) => void;
  onRetry: () => void;
  onRetryAssignments: () => void;
}) {
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<AgentFilter>("current");
  const payload = state.data;
  const scopeUnavailable = project !== "all" && issuesState.data === null;
  const issueProjects = issueIndex(issuesState.data ?? []);
  const allAgents = payload?.agents ?? [];
  const scoped = allAgents.filter((a) =>
    agentMatchesProject(a, project, issueProjects),
  );
  const counts = Object.fromEntries(
    AGENT_FILTERS.map(({ value }) => [
      value,
      scoped.filter((a) => agentMatchesFilter(a, value)).length,
    ]),
  ) as Record<AgentFilter, number>;
  const agents = scoped
    .filter(
      (a) =>
        agentMatchesFilter(a, filter) &&
        agentMatchesSearch(a, query, issueProjects),
    )
    .sort(
      (a, b) => agentOrder(a) - agentOrder(b) || a.alias.localeCompare(b.alias),
    );
  const sections = LIFECYCLE_SECTIONS.map((section) => ({
    ...section,
    agents: agents.filter((a) => lifecycleOf(a) === section.key),
  })).filter((section) => section.agents.length > 0);
  const globalAgents = allAgents.filter((a) =>
    agentIsUnassigned(a, issueProjects),
  );
  const otherProjectAgents =
    project === "all"
      ? []
      : allAgents.filter(
          (a) =>
            !agentMatchesProject(a, project, issueProjects) &&
            !agentIsUnassigned(a, issueProjects),
        );
  const filtered = filter !== "current" || query.trim() !== "";
  const reset = () => {
    setQuery("");
    setFilter("current");
  };
  const emptyCopy =
    scoped.length === 0
      ? agentsEmptyCopy(project)
      : filter === "current" && counts.current === 0 && !query.trim()
        ? "No current agents. Select All, Stopped, or Mailboxes to see the full roster."
      : "No agents match these filters.";
  const agent = allAgents.find((a) => a.alias === open);
  const summary = !payload
    ? state.status === "failed"
      ? "Agents unavailable"
      : "Loading agents…"
    : scopeUnavailable
      ? issuesState.status === "failed"
        ? "Assignments unavailable"
        : "Loading assignments…"
      : `${counts.current} current · ${scoped.length} ${project === "all" ? "total across all projects" : `total in ${project}`}`;

  return (
    <main className="agents-page px-4 lg:px-8 pt-6 pb-9 w-full">
      <div className="flex flex-wrap items-center gap-3 mb-2">
        <h1 className="text-section font-semibold text-ink-100">Agents</h1>
        <span className="text-label text-ink-500">{summary}</span>
        <StaleChip state={state} />
      </div>
      <p className="text-label text-ink-400 mb-5">
        Current work, agent status, and recent activity.
      </p>

      {project !== "all" &&
        issuesState.data !== null &&
        (globalAgents.length > 0 || otherProjectAgents.length > 0) && (
          <p className="text-label text-ink-500 mb-4">
            Agents working on or owning active issues in {project}.{" "}
            {otherProjectAgents.length > 0 &&
              `${otherProjectAgents.length} in other projects are visible in All projects.`}
          </p>
        )}
      {payload?.daemon === "unreachable" && (
        <div
          className="card mb-4 px-4 py-3 text-label text-warn border-warn/40"
          role="status"
        >
          Daemon unavailable. Showing the last known agent state.
        </div>
      )}
      <ResourceGate
        state={state}
        loading="Loading agents…"
        failed="Could not load agents"
        onRetry={onRetry}
      />
      {payload && scopeUnavailable && (
        <div className="text-label text-warn mb-4 space-y-2">
          <p role={issuesState.status === "failed" ? "alert" : "status"}>
            {issuesState.status === "failed"
              ? "Project issue assignments could not be loaded. Select All projects to see every agent."
              : "Loading project issue assignments…"}
          </p>
          {issuesState.status === "failed" && (
            <Button onClick={onRetryAssignments}>Retry assignments</Button>
          )}
        </div>
      )}
      {project !== "all" && issuesState.status === "stale" && (
        <p role="status" className="text-label text-warn mb-4">
          Project issue assignments may be out of date.
        </p>
      )}

      {payload && !scopeUnavailable && (
        <>
          <div
            className="agents-triage"
            role="group"
            aria-label="Agent lifecycle triage"
          >
            {TRIAGE.map((cell) => (
              <button
                key={cell.label}
                type="button"
                className={`agents-triage-cell tone-${cell.tone}`}
                aria-pressed={filter === cell.filter}
                onClick={() => setFilter(cell.filter)}
              >
                <span className="agents-triage-n">
                  {scoped.filter(cell.count).length}
                </span>
                <span className="agents-triage-t">{cell.label}</span>
              </button>
            ))}
          </div>
          <div className="agents-tools mb-4">
            <div
              className="agents-filters"
              role="group"
              aria-label="Filter agents by status"
            >
              {AGENT_FILTERS.filter(
                (f) =>
                  f.value !== "other" || counts.other > 0 || filter === "other",
              ).map(({ value, label }) => (
                <button
                  key={value}
                  type="button"
                  className="agents-filter"
                  aria-pressed={filter === value}
                  onClick={() => setFilter(value)}
                >
                  {label}
                  <span className="num">{counts[value]}</span>
                </button>
              ))}
            </div>
            <label className="agents-search" htmlFor="agents-search">
              <IconSearch />
              <span className="sr-only">Search agents</span>
              <input
                id="agents-search"
                type="search"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder="Search agents, work, or issues"
              />
            </label>
          </div>
          {filtered && (
            <div className="flex items-center gap-3 mb-3 text-label text-ink-500">
              <span role="status">
                Showing {agents.length} of {scoped.length} agents
              </span>
              <Button variant="ghost" size="sm" onClick={reset}>
                Clear filters
              </Button>
            </div>
          )}
          {agents.length === 0 ? (
            <div className="card py-10 px-5 text-center text-label text-ink-500">
              {emptyCopy}
            </div>
          ) : (
            sections.map((section) => (
              <section key={section.key}>
                <div className="agents-sec">
                  <h2>{section.title}</h2>
                  <span className="cnt">{section.agents.length}</span>
                  <span className="rule" aria-hidden="true" />
                </div>
                <div className="agents-grid">
                  {section.agents.map((a) => (
                    <AgentCard
                      key={a.alias}
                      agent={a}
                      index={issueProjects}
                      onOpen={() => setOpen(a.alias)}
                      onOpenIssue={onOpenIssue}
                    />
                  ))}
                </div>
              </section>
            ))
          )}
          {project !== "all" &&
            issuesState.data !== null &&
            globalAgents.length > 0 && (
              <details className="mt-5 agents-unassigned">
                <summary className="text-label text-ink-500 cursor-pointer">
                  Global or unassigned agents · {globalAgents.length}
                </summary>
                <p className="text-label text-ink-500 my-2">
                  These agents have no issue assignment to a project.
                </p>
                <div className="flex flex-wrap gap-2">
                  {globalAgents.map((a) => (
                    <Button
                      key={a.alias}
                      size="sm"
                      onClick={() => setOpen(a.alias)}
                    >
                      {a.alias}
                    </Button>
                  ))}
                </div>
              </details>
            )}
        </>
      )}
      {open && (
        <AgentDrawer
          key={open}
          alias={open}
          observed={agent}
          onClose={() => setOpen(null)}
          onOpenIssue={(id) => {
            setOpen(null);
            onOpenIssue(id);
          }}
        />
      )}
    </main>
  );
}
