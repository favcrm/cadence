import { useContext, useState } from "react";
import type { ResourceState } from "../../lib/cache";
import {
  agentIsUnassigned,
  agentIssueIds,
  agentMatchesProject,
  issueIndex,
  type IssueIndex,
} from "../../lib/scope";
import { agentsEmptyCopy } from "../../lib/uxCopy";
import { useTranslate } from "../../lib/locale";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import { IconSearch } from "../../ui/icons";
import { navigate } from "../../lib/useLocation";
import { resources } from "../../lib/resources";
import { useQuery } from "../../lib/useResource";
import { setComposerDraft } from "../app-shell/chat/composerStore";
import Button from "../../ui/Button";
import { WriteGate } from "../auth/WriteGate";
import { activeTasks, AgentActivity, WorkBlock } from "./AgentBlocks";
import AgentAvatar, { agentLook } from "./AgentAvatar";
import AgentPage from "./AgentPage";
import { FirstJobs } from "./AgentTabs";
import HireSheet from "./HireSheet";
import NewAgentDialog from "./NewAgentDialog";
import Recruit from "./Recruit";
import { HireGate, RoleCard } from "./RecruitCards";
import {
  nextAlias,
  onTeamCount,
  roleOf,
  runsOnText,
  suggestedRoles,
  useAgentRoles,
} from "./agentRoles";
import type { AgentTab } from "./agentTabModel";
import {
  AGENT_FILTERS,
  agentHoldsWorkButDead,
  agentMatchesFilter,
  agentMatchesSearch,
  agentLocation,
  agentOrder,
  LIFECYCLE_SECTIONS,
  lifecycleBadge,
  lifecycleOf,
  type AgentFilter,
} from "./agentView";
import "./agents.css";
import type { Agent, AgentRole, AgentsPayload, IssueCard } from "../../lib/types";

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
  const t = useTranslate();
  const viaTask = new Set(activeTasks(agent).map((task) => task.issue));
  const ids = agentIssueIds(agent, index).filter((id) => !viaTask.has(id));
  if (ids.length === 0) return null;
  return (
    <div className="num text-micro text-ink-500 mt-1 flex flex-wrap items-center gap-x-1.5">
      {t("Issues")}
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
  const t = useTranslate();
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
        {live ? t("running") : task.task_state}
        {task.job_title
          ? ` · ${task.job_title}`
          : task.job
            ? ` · ${task.job}`
            : ""}
        {task.job_state ? ` · ${t("stage")} ${task.job_state}` : ""}
        {activeTasks(agent).length > 1
          ? ` · ${t("+{count} more").replace("{count}", String(activeTasks(agent).length - 1))}`
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
  roleTitle,
  hosted,
  isNew,
  onOpen,
  onOpenIssue,
}: {
  agent: Agent;
  index: IssueIndex;
  /** The catalogue role this agent was hired as, if any. */
  roleTitle?: string;
  hosted: boolean;
  isNew?: boolean;
  onOpen: () => void;
  onOpenIssue: (id: string) => void;
}) {
  const writeBlock = useContext(WriteGate);
  const t = useTranslate();
  const lifecycle = lifecycleOf(a);
  const tone = LIFECYCLE_SECTIONS.find((s) => s.key === lifecycle)!.tone;
  const badge = lifecycleBadge(a, t);
  return (
    <article
      className={`agent-card tone-${tone}${isNew ? " rc-new" : ""}`}
      aria-label={t("Agent {alias}").replace("{alias}", () => a.alias)}
    >
      <div className="top">
        <div className="avwrap">
          <AgentAvatar
            slug={agentLook(a.alias, { look: a.look ?? a.params?.look })}
            active={lifecycle === "active"}
            still={lifecycle !== "active"}
          />
          {agentHoldsWorkButDead(a) && (
            <span className="agent-hold" title={t("Dead but still holds work")}>
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
            aria-label={t("Open agent {alias}").replace("{alias}", () => a.alias)}
          >
            {a.alias}
          </button>
          <span className="ml-2 text-micro text-ink-500">
            {agentLocation(a) === "cloud" ? t("Cloud") : t("Local")}
          </span>
          <p className="meta break-words">
            {roleTitle ? (
              <>
                {roleTitle} · {hosted ? t("workspace") : t("this computer")}
              </>
            ) : (
              <>
                {a.role ?? t("Agent")} · {a.provider} ·{" "}
                {a.group_root ? t("root") : a.group}
              </>
            )}
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
        <span className="agent-foot-side">
          {/* Threads resolves the alias: the existing You ↔ agent row opens,
              else `?dm=` starts a local draft — no server row is created
              and nothing is sent until the operator writes. */}
          <Button
            variant="ghost"
            size="sm"
            href={`/threads?dm=${encodeURIComponent(a.alias)}`}
            disabled={writeBlock !== null}
            title={writeBlock ?? t("Message {alias}").replace("{alias}", () => a.alias)}
            aria-label={t("Message {alias}").replace("{alias}", () => a.alias)}
          >
            {t("Message")}
          </Button>
          <span className={`agent-qchip${a.queued >= 100 ? " hot" : ""}`}>
            {t("{count} queued").replace("{count}", String(a.queued))}
          </span>
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
  tab,
  recruit,
  onOpenAgent: setOpen,
  onTab,
  onRecruit,
  onOpenIssue,
  onRetry,
  onRetryAssignments,
}: {
  state: ResourceState<AgentsPayload>;
  issues: ResourceState<IssueCard[]>;
  project: string;
  open: string | null;
  tab: AgentTab;
  recruit: boolean;
  onOpenAgent: (alias: string | null) => void;
  onTab: (tab: AgentTab) => void;
  onRecruit: (on: boolean) => void;
  onOpenIssue: (id: string) => void;
  onRetry: () => void;
  onRetryAssignments: () => void;
}) {
  const t = useTranslate();
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<AgentFilter>("current");
  const [creating, setCreating] = useState(false);
  const [picked, setPicked] = useState<AgentRole | null>(null);
  const [joined, setJoined] = useState<{ alias: string; role: AgentRole } | null>(null);
  const roles = useAgentRoles();
  const installed = useQuery(resources.apps);
  const writeBlock = useContext(WriteGate);
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
      (a, b) =>
        Number(b.alias === joined?.alias) - Number(a.alias === joined?.alias) ||
        agentOrder(a) - agentOrder(b) ||
        a.alias.localeCompare(b.alias),
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
      ? t(agentsEmptyCopy(project))
      : filter === "current" && counts.current === 0 && !query.trim()
        ? t("No current agents. Select All, Stopped, or Mailboxes to see the full roster.")
      : t("No agents match these filters.");
  const agent = allAgents.find((a) => a.alias === open);
  const hosted = roles.data?.install.hosted ?? false;
  const suggestion = suggestedRoles(installed.data ?? [], roles.data?.roles ?? []);
  const taken = allAgents.map((a) => a.alias);
  const askMaster = () => {
    // Never sends: the prompt waits in Master's composer.
    setComposerDraft("home", "I need someone to ");
    navigate("/");
  };
  const hired = (alias: string, role: AgentRole) => {
    setPicked(null);
    setJoined({ alias, role });
    onRecruit(false);
    onRetry();
  };
  const placeholder = joined !== null && !allAgents.some((a) => a.alias === joined.alias);
  const tile = (
    <button
      key="recruit-tile"
      type="button"
      className="rc-tile"
      onClick={() => onRecruit(true)}
    >
      <span className="plus" aria-hidden="true">＋</span>
      {t("Recruit a role")}
      <small>{t("Developer, Accountant, Support…")}</small>
    </button>
  );
  const sheet = picked && roles.data && (
    <HireSheet
      key={picked.id}
      role={picked}
      payload={roles.data}
      alias={nextAlias(picked.id, taken)}
      taken={taken}
      onClose={() => setPicked(null)}
      onHired={hired}
    />
  );
  const summary = !payload
    ? state.status === "failed"
      ? t("Agents unavailable")
      : t("Loading agents…")
    : scopeUnavailable
      ? issuesState.status === "failed"
        ? t("Assignments unavailable")
        : t("Loading assignments…")
      : t("{current} current · {total}")
          .replace("{current}", String(counts.current))
          .replace(
            "{total}",
            () => project === "all"
              ? t("{count} total across all projects").replace("{count}", String(scoped.length))
              : t("{count} total in {project}").replace("{count}", String(scoped.length)).replace("{project}", () => project),
          );

  if (open) {
    return (
      <AgentPage
        key={open}
        alias={open}
        observed={agent}
        roles={roles}
        tab={tab}
        onTab={onTab}
        onBack={() => setOpen(null)}
        onOpenIssue={onOpenIssue}
      />
    );
  }
  if (recruit) {
    return (
      <>
        <Recruit
          roles={roles}
          agents={allAgents}
          writeBlock={writeBlock}
          onBack={() => onRecruit(false)}
          onPick={setPicked}
          onAskMaster={askMaster}
          onBlank={() => setCreating(true)}
        />
        {sheet}
        {creating && (
          <NewAgentDialog
            onClose={() => setCreating(false)}
            onCreated={() => {
              onRetry();
              onRecruit(false);
            }}
          />
        )}
      </>
    );
  }

  return (
    <main className="agents-page px-4 lg:px-8 pt-6 pb-9 w-full">
      <div className="flex flex-wrap items-center gap-3 mb-2">
        <h1 className="text-section font-semibold text-ink-100">{t("Agents")}</h1>
        <span className="text-label text-ink-500">{summary}</span>
        <StaleChip state={state} />
        <span className="ml-auto flex gap-2">
          <Button size="sm" onClick={askMaster}>
            {t("Ask Master")}
          </Button>
          <Button
            size="sm"
            variant="primary"
            onClick={() => onRecruit(true)}
            title={writeBlock ?? t("Recruit an agent from a ready-made role")}
          >
            ＋ {t("Recruit")}
          </Button>
        </span>
      </div>
      <p className="text-label text-ink-400 mb-5">
        {t("Your team. Recruit a ready-made role, or ask Master to find one.")}
      </p>
      <HireGate reason={writeBlock} />
      {joined && (
        <div className="rc-nudge" role="status">
          <AgentAvatar slug="custom" look={joined.role.look} size={48} active />
          <div>
            <div className="t">
              {t("{name} joined as {role}")
                .replace("{name}", () => joined.alias)
                .replace("{role}", () => joined.role.title)}
            </div>
            <div className="s">
              {t("Starting on {where}. Give a first job — it opens a thread.").replace(
                "{where}",
                () => t(runsOnText(hosted)),
              )}
            </div>
            <FirstJobs alias={joined.alias} tasks={joined.role.first_tasks} />
          </div>
        </div>
      )}

      {project !== "all" &&
        issuesState.data !== null &&
        (globalAgents.length > 0 || otherProjectAgents.length > 0) && (
          <p className="text-label text-ink-500 mb-4">
            {t("Agents working on or owning active issues in {project}.").replace("{project}", () => project)}{" "}
            {otherProjectAgents.length > 0 &&
              t("{count} in other projects are visible in All projects.").replace("{count}", String(otherProjectAgents.length))}
          </p>
        )}
      {payload?.daemon === "unreachable" && (
        <div
          className="card mb-4 px-4 py-3 text-label text-warn border-warn/40"
          role="status"
        >
          {t("Daemon unavailable. Showing the last known agent state.")}
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
              ? t("Project issue assignments could not be loaded. Select All projects to see every agent.")
              : t("Loading project issue assignments…")}
          </p>
          {issuesState.status === "failed" && (
            <Button onClick={onRetryAssignments}>{t("Retry assignments")}</Button>
          )}
        </div>
      )}
      {project !== "all" && issuesState.status === "stale" && (
        <p role="status" className="text-label text-warn mb-4">
          {t("Project issue assignments may be out of date.")}
        </p>
      )}

      {payload && !scopeUnavailable && (
        <>
          <div
            className="agents-triage"
            role="group"
            aria-label={t("Agent lifecycle triage")}
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
                <span className="agents-triage-t">{t(cell.label)}</span>
              </button>
            ))}
          </div>
          <div className="agents-tools mb-4">
            <div
              className="agents-filters"
              role="group"
              aria-label={t("Filter agents by status")}
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
                  {t(label)}
                  <span className="num">{counts[value]}</span>
                </button>
              ))}
            </div>
            <label className="agents-search" htmlFor="agents-search">
              <IconSearch />
              <span className="sr-only">{t("Search agents")}</span>
              <input
                id="agents-search"
                type="search"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder={t("Search agents, work, or issues")}
              />
            </label>
          </div>
          {filtered && (
            <div className="flex items-center gap-3 mb-3 text-label text-ink-500">
              <span role="status">
                {t("Showing {shown} of {total} agents")
                  .replace("{shown}", String(agents.length))
                  .replace("{total}", String(scoped.length))}
              </span>
              <Button variant="ghost" size="sm" onClick={reset}>
                {t("Clear filters")}
              </Button>
            </div>
          )}
          {placeholder && joined && (
            <div className="agents-grid mb-3">
              <div className="rc-seat new" aria-label={t("Agent {alias}").replace("{alias}", () => joined.alias)}>
                <AgentAvatar slug="custom" look={joined.role.look} size={44} active />
                <div>
                  <div className="t">{joined.alias}</div>
                  <div className="s">
                    {joined.role.title} · {hosted ? t("workspace") : t("this computer")}
                  </div>
                </div>
                <div className="st">
                  <span className="rc-dot" style={{ "--c": "var(--color-warn)" } as React.CSSProperties} />
                  {t("Starting…")}
                </div>
              </div>
            </div>
          )}
          {agents.length === 0 ? (
            <>
              <div className="card py-10 px-5 text-center text-label text-ink-500">
                {emptyCopy}
              </div>
              <div className="agents-grid mt-3">{tile}</div>
            </>
          ) : (
            sections.map((section, index) => (
              <section key={section.key}>
                <div className="agents-sec">
                  <h2>{t(section.title)}</h2>
                  <span className="cnt">{section.agents.length}</span>
                  <span className="rule" aria-hidden="true" />
                </div>
                <div className="agents-grid">
                  {section.agents.map((a) => (
                    <AgentCard
                      key={a.alias}
                      agent={a}
                      index={issueProjects}
                      roleTitle={roleOf(a, roles.data)?.title}
                      hosted={hosted}
                      isNew={a.alias === joined?.alias}
                      onOpen={() => setOpen(a.alias)}
                      onOpenIssue={onOpenIssue}
                    />
                  ))}
                  {index === sections.length - 1 && tile}
                </div>
              </section>
            ))
          )}
          {roles.data && suggestion.roles.length > 0 && (
            <>
              <div className="rc-sec">
                <h2>{t("Suggested for you")}</h2>
                <span>
                  {t("based on your apps: {apps}").replace("{apps}", () => suggestion.labels.join(", "))}
                </span>
              </div>
              <div className="rc-grid">
                {suggestion.roles.map((role) => (
                  <RoleCard
                    key={role.id}
                    role={role}
                    payload={roles.data!}
                    onTeam={onTeamCount(role, allAgents)}
                    onPick={setPicked}
                  />
                ))}
              </div>
            </>
          )}
          {project !== "all" &&
            issuesState.data !== null &&
            globalAgents.length > 0 && (
              <details className="mt-5 agents-unassigned">
                <summary className="text-label text-ink-500 cursor-pointer">
                  {t("Global or unassigned agents · {count}").replace("{count}", String(globalAgents.length))}
                </summary>
                <p className="text-label text-ink-500 my-2">
                  {t("These agents have no issue assignment to a project.")}
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
      {creating && (
        <NewAgentDialog
          onClose={() => setCreating(false)}
          onCreated={() => onRetry()}
        />
      )}
      {sheet}
      {creating && (
        <NewAgentDialog
          onClose={() => setCreating(false)}
          onCreated={() => onRetry()}
        />
      )}
    </main>
  );
}
