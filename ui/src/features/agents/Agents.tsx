import { useContext, useState } from "react";
import type { ResourceState } from "../../lib/cache";
import { issueIndex } from "../../lib/scope";
import { useTranslate } from "../../lib/locale";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import { IconSearch } from "../../ui/icons";
import { resources } from "../../lib/resources";
import { useQuery } from "../../lib/useResource";
import Button from "../../ui/Button";
import { HireBlock } from "../auth/WriteGate";
import AgentAvatar from "./AgentAvatar";
import SeatCard from "./SeatCard";
import { MASTER } from "../home/master";
import { ROLES } from "../settings/ModelDefaults";
import RecruitChat from "./RecruitChat";
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
  agentLocation,
  agentMatchesFilter,
  agentMatchesSearch,
  agentOrder,
  type AgentFilter,
} from "./agentView";
import "./agents.css";
import type { Agent, AgentRole, AgentsPayload, IssueCard } from "../../lib/types";

/** The toolbar's segmented filters, in the order they read. */
const TOOLBAR_FILTERS: AgentFilter[] = ["current", "attention", "working", "idle", "stopped", "inbox", "all"];

/** "Accountant · this computer": the catalogue title, else "Master", else the
 *  team role's label (never the raw id or the provider), then where it runs. */
function roleLine(a: Agent, roleTitle: string | undefined, hosted: boolean, t: (s: string) => string): string {
  const label = (id: string | null | undefined) => ROLES.find(([key]) => key === id)?.[1];
  const title =
    roleTitle ?? (a.alias === MASTER ? t("Master") : (label(a.team_role) ?? label(a.role) ?? t("Agent")));
  const where = agentLocation(a) === "cloud" ? t("cloud") : hosted ? t("workspace") : t("this computer");
  return `${title} · ${where}`;
}

export default function Agents({
  state,
  issues: issuesState,
  open,
  tab,
  recruit,
  ask,
  onAsk,
  onOpenAgent: setOpen,
  onTab,
  onRecruit,
  onOpenIssue,
  onRetry,
}: {
  state: ResourceState<AgentsPayload>;
  issues: ResourceState<IssueCard[]>;
  open: string | null;
  tab: AgentTab;
  recruit: boolean;
  ask: boolean;
  onAsk: () => void;
  onOpenAgent: (alias: string | null) => void;
  onTab: (tab: AgentTab) => void;
  onRecruit: (on: boolean) => void;
  onOpenIssue: (id: string) => void;
  onRetry: () => void;
}) {
  const t = useTranslate();
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<AgentFilter>("current");
  const [creating, setCreating] = useState(false);
  const [picked, setPicked] = useState<AgentRole | null>(null);
  const [joined, setJoined] = useState<{ alias: string; role: AgentRole } | null>(null);
  const roles = useAgentRoles();
  const installed = useQuery(resources.apps);
  const hireBlock = useContext(HireBlock);
  const payload = state.data;
  const issueIndexed = issueIndex(issuesState.data ?? []);
  // Agents belong to the team, not a project: the roster lists them all.
  const allAgents = payload?.agents ?? [];
  const counts = Object.fromEntries(
    AGENT_FILTERS.map(({ value }) => [value, allAgents.filter((a) => agentMatchesFilter(a, value)).length]),
  ) as Record<AgentFilter, number>;
  const agents = allAgents
    .filter((a) => agentMatchesFilter(a, filter) && agentMatchesSearch(a, query, issueIndexed))
    .sort(
      (a, b) =>
        Number(b.alias === MASTER) - Number(a.alias === MASTER) ||
        Number(b.alias === joined?.alias) - Number(a.alias === joined?.alias) ||
        agentOrder(a) - agentOrder(b) ||
        a.alias.localeCompare(b.alias),
    );
  const emptyCopy =
    allAgents.length === 0
      ? t("No agents registered.")
      : filter === "current" && counts.current === 0 && !query.trim()
        ? t("No current agents. Select All, Stopped, or Mailboxes to see the full roster.")
        : t("No agents match these filters.");
  const agent = allAgents.find((a) => a.alias === open);
  const hosted = roles.data?.install.hosted ?? false;
  const suggestion = suggestedRoles(installed.data ?? [], roles.data?.roles ?? []);
  const taken = allAgents.map((a) => a.alias);
  const askMaster = onAsk;
  const hired = (alias: string, role: AgentRole) => {
    setPicked(null);
    setJoined({ alias, role });
    onRecruit(false);
    onRetry();
  };
  const placeholder = joined !== null && !allAgents.some((a) => a.alias === joined.alias);
  const tile = (
    <button key="recruit-tile" type="button" className="rc-tile" onClick={() => onRecruit(true)}>
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
  if (ask) {
    return (
      <RecruitChat
        hireBlock={hireBlock}
        onBack={() => setOpen(null)}
      />
    );
  }
  if (recruit) {
    return (
      <>
        <Recruit
          roles={roles}
          agents={allAgents}
          hireBlock={hireBlock}
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
      <div className="rc-head">
        <div>
          <h1>
            {t("Agents")} <StaleChip state={state} />
          </h1>
          <p>{t("Your team. Recruit a ready-made role, or ask Master to find one.")}</p>
        </div>
        <div className="rc-acts">
          <Button onClick={askMaster}>{t("Ask Master")}</Button>
          <Button
            variant="primary"
            onClick={() => onRecruit(true)}
            title={hireBlock ?? t("Recruit an agent from a ready-made role")}
          >
            ＋ {t("Recruit")}
          </Button>
        </div>
      </div>
      <HireGate reason={hireBlock} />
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
      {payload?.daemon === "unreachable" && (
        <div className="card mb-4 px-4 py-3 text-label text-warn border-warn/40" role="status">
          {t("Daemon unavailable. Showing the last known agent state.")}
        </div>
      )}
      <ResourceGate state={state} loading="Loading agents…" failed="Could not load agents" onRetry={onRetry} />

      {payload && (
        <>
          <div className="ag-toolbar" role="search" aria-label={t("Find agents")}>
            <div className="ag-seg" role="group" aria-label={t("Filter agents by status")}>
              {AGENT_FILTERS.filter((f) => TOOLBAR_FILTERS.includes(f.value))
                .sort((x, y) => TOOLBAR_FILTERS.indexOf(x.value) - TOOLBAR_FILTERS.indexOf(y.value))
                .map(({ value, label }) => (
                  <button
                    key={value}
                    type="button"
                    className="ag-seg-btn"
                    aria-pressed={filter === value}
                    onClick={() => setFilter(value)}
                  >
                    {t(label)}
                    <span className="num">{counts[value]}</span>
                  </button>
                ))}
            </div>
            <label className="ag-search" htmlFor="agents-search">
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
          <div className="rc-seats">
            {placeholder && joined && (
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
            )}
            {agents.map((a) => (
              <SeatCard
                key={a.alias}
                agent={a}
                roleLine={roleLine(a, roleOf(a, roles.data)?.title, hosted, t)}
                isNew={a.alias === joined?.alias}
                onOpen={() => setOpen(a.alias)}
              />
            ))}
            {tile}
          </div>
          {agents.length === 0 && (
            <p className="text-label text-ink-500 mt-3" role="status">
              {emptyCopy}
            </p>
          )}
          {roles.data && suggestion.roles.length > 0 && (
            <>
              <div className="rc-sec">
                <h2>{t("Suggested for you")}</h2>
                <span>{t("based on your apps: {apps}").replace("{apps}", () => suggestion.labels.join(", "))}</span>
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
        </>
      )}
      {sheet}
      {creating && <NewAgentDialog onClose={() => setCreating(false)} onCreated={() => onRetry()} />}
    </main>
  );
}
