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
import { activeTasks, AgentActivity, AgentQueue, AgentState, WorkBlock } from "./AgentBlocks";
import AgentDrawer from "./AgentDrawer";
import {
  AGENT_FILTERS,
  agentMatchesFilter,
  agentMatchesSearch,
  agentOrder,
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

function AgentIdentity({
  agent: a,
  onOpen,
}: {
  agent: Agent;
  onOpen: () => void;
}) {
  return (
    <div className="min-w-0">
      <button
        type="button"
        className="agent-name"
        onClick={onOpen}
        aria-label={`Open agent ${a.alias}`}
      >
        {a.alias}
      </button>
      <p className="text-micro text-ink-500 mt-1 break-words">
        {a.role ?? "Agent"} · {a.provider} · {a.group_root ? "root" : a.group}
      </p>
    </div>
  );
}

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
            <>
              <div className="agents-cards space-y-3">
                {agents.map((a) => (
                  <article key={a.alias} className="card p-4">
                    <div className="flex items-start justify-between gap-3">
                      <AgentIdentity
                        agent={a}
                        onOpen={() => setOpen(a.alias)}
                      />
                      <AgentState agent={a} />
                    </div>
                    {a.fenced && (
                      <p className="text-label text-fail mt-2">
                        Recovery required · Open agent for guidance
                      </p>
                    )}
                    <div className="mt-3">
                      <WorkBlock agent={a} onOpenIssue={onOpenIssue} />
                      <RelatedIssues
                        agent={a}
                        index={issueProjects}
                        onOpenIssue={onOpenIssue}
                      />
                    </div>
                    <div className="agents-card-footer">
                      <AgentQueue agent={a} />
                      <AgentActivity agent={a} />
                    </div>
                  </article>
                ))}
              </div>
              <div className="agents-table card overflow-hidden">
                <table className="w-full text-label">
                  <caption className="sr-only">
                    Agents in {project === "all" ? "all projects" : project}
                  </caption>
                  <thead>
                    <tr>
                      {[
                        "Agent",
                        "Status",
                        "Current work",
                        "Queue",
                        "Last activity",
                      ].map((h) => (
                        <th
                          key={h}
                          scope="col"
                          className="slabel font-normal text-left px-4 py-3"
                        >
                          {h}
                        </th>
                      ))}
                    </tr>
                  </thead>
                  <tbody className="divide-y divide-ink-700/70">
                    {agents.map((a) => (
                      <tr key={a.alias}>
                        <td>
                          <AgentIdentity
                            agent={a}
                            onOpen={() => setOpen(a.alias)}
                          />
                        </td>
                        <td>
                          <AgentState agent={a} />
                          {a.fenced && (
                            <p className="text-micro text-fail mt-2">
                              Recovery required
                            </p>
                          )}
                        </td>
                        <td>
                          <WorkBlock agent={a} onOpenIssue={onOpenIssue} />
                          <RelatedIssues
                            agent={a}
                            index={issueProjects}
                            onOpenIssue={onOpenIssue}
                          />
                        </td>
                        <td>
                          <AgentQueue agent={a} />
                        </td>
                        <td>
                          <AgentActivity agent={a} />
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </>
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
