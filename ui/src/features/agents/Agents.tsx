import { useContext, useState, type ReactNode } from "react";
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
import HireSheet from "./HireSheet";
import ProfileSeat, { ConnectRuntime } from "./ProfileSeat";
import AgentSidebar from "./AgentSidebar";
import NewAgentDialog from "./NewAgentDialog";
import Recruit from "./Recruit";
import { HireGate, RoleCard } from "./RecruitCards";
import {
  nextAlias,
  onTeamCount,
  roleOf,
  suggestedRoles,
  useAgentRoles,
} from "./agentRoles";
import type { AgentTab } from "./agentTabModel";
import {
  agentLocation,
  agentMatchesFilter,
  agentMatchesSearch,
  agentOrder,
  profileMatchesFilter,
  profileMatchesSearch,
  profileWork,
  type AgentFilter,
} from "./agentView";
import "./agents.css";
import type { Agent, HiredProfile, AgentRole, AgentsPayload, IssueCard } from "../../lib/types";

/** "Accountant · this computer": the catalogue title, else "Master", else the
 *  team role's label (never the raw id or the provider), then where it runs. */
function roleLine(a: Agent, roleTitle: string | undefined, hosted: boolean, t: (s: string) => string): string {
  const label = (id: string | null | undefined) => ROLES.find(([key]) => key === id)?.[1];
  const title =
    roleTitle ?? (a.alias === MASTER ? t("Master") : (label(a.team_role) ?? label(a.role) ?? t("Agent")));
  const where = agentLocation(a) === "cloud" ? t("cloud") : hosted ? t("workspace") : t("this computer");
  // The model a runtime resolved to; `inherit` is not a model, so it is not shown.
  const model = a.model && a.model !== "inherit" ? ` · ${a.model}` : "";
  return `${title} · ${where}${model}`;
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
  const [creating, setCreating] = useState(false);
  const [picked, setPicked] = useState<AgentRole | null>(null);
  const [joined, setJoined] = useState<{ profile: HiredProfile; role: AgentRole } | null>(null);
  const roles = useAgentRoles();
  const installed = useQuery(resources.apps);
  const hireBlock = useContext(HireBlock);
  const payload = state.data;
  const issueIndexed = issueIndex(issuesState.data ?? []);
  // Agents belong to the team, not a project: the roster lists them all.
  const allAgents = payload?.agents ?? [];
  const profiles = payload?.profiles ?? [];
  const idleProfiles = profiles.filter((p) => profileMatchesFilter(p, "all"));
  // The cards are the current team; a search reaches everyone. Stopped agents
  // and mailboxes otherwise live in the sidebar's folded groups.
  const searching = query.trim() !== "";
  const filter: AgentFilter = searching ? "all" : "current";
  const sidebarAgents = allAgents.filter((a) => agentMatchesSearch(a, query, issueIndexed));
  const sidebarProfiles = idleProfiles.filter((p) => profileMatchesSearch(p, query));
  const shownProfiles = idleProfiles.filter(
    (p) => profileMatchesFilter(p, filter) && profileMatchesSearch(p, query),
  );
  const agents = allAgents
    .filter((a) => agentMatchesFilter(a, filter) && agentMatchesSearch(a, query, issueIndexed))
    .sort(
      (a, b) =>
        Number(b.alias === MASTER) - Number(a.alias === MASTER) ||
        agentOrder(a) - agentOrder(b) ||
        a.alias.localeCompare(b.alias),
    );
  const emptyCopy =
    allAgents.length + idleProfiles.length === 0
      ? t("No agents registered.")
      : searching
        ? t("No agents match “{query}”.").replace("{query}", () => query)
        : t("No current agents. Search to find stopped agents and mailboxes.");
  // A phone has no sidebar, so what it folds is listed under the cards there.
  const folded = searching ? [] : allAgents.filter((a) => !agentMatchesFilter(a, "current"));
  const agent = allAgents.find((a) => a.alias === open);
  const hosted = roles.data?.install.hosted ?? false;
  const suggestion = suggestedRoles(installed.data ?? [], roles.data?.roles ?? []);
  const groups = allAgents.filter((a) => a.role === "pm").map((a) => a.alias);
  const taken = [...allAgents.map((a) => a.alias), ...profiles.map((p) => p.alias)];
  const askMaster = onAsk;
  const profileTitle = (profile: HiredProfile) =>
    roles.data?.roles.find((r) => r.id === profile.catalogue)?.title ?? profile.role;
  const issueCards = issuesState.data ?? [];
  const hired = (profile: HiredProfile, role: AgentRole) => {
    setPicked(null);
    setJoined({ profile, role });
    onRecruit(false);
    onRetry();
  };
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

  const shell = (content: ReactNode) => (
    <div className="ag-shell">
      <AgentSidebar
        agents={sidebarAgents}
        profiles={sidebarProfiles}
        open={open}
        query={query}
        onQuery={setQuery}
        onOpen={setOpen}
        onOverview={() => setOpen(null)}
        onRecruit={() => onRecruit(true)}
      />
      <div className="ag-shell-body">{content}</div>
    </div>
  );

  if (open) {
    return shell(
      <AgentPage
        key={open}
        alias={open}
        observed={agent}
        roles={roles}
        profiles={profiles}
        tab={tab}
        onTab={onTab}
        onBack={() => setOpen(null)}
        onOpenIssue={onOpenIssue}
        hosted={hosted}
        groups={groups}
        rosterLoaded={payload != null}
      />
    );
  }
  if (ask) {
    return shell(
      <RecruitChat
        hireBlock={hireBlock}
        onBack={() => setOpen(null)}
      />
    );
  }
  if (recruit) {
    return shell(
      <>
        <Recruit
          roles={roles}
          profiles={profiles}
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

  return shell(
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
      {joined && profiles.some((p) => p.id === joined.profile.id && p.runtimes.length === 0) && (
        <div className="rc-nudge" role="status">
          <AgentAvatar slug="custom" look={joined.role.look} size={48} still />
          <div>
            <div className="t">
              {t("{name} was added as {role}")
                .replace("{name}", () => joined.profile.alias)
                .replace("{role}", () => joined.role.title)}
            </div>
            <div className="s">{t("Idle · no runtime")}</div>
            <ConnectRuntime profile={joined.profile} hosted={hosted} groups={groups} />
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
            {agents.map((a) => (
              <SeatCard
                key={a.alias}
                agent={a}
                roleLine={roleLine(a, roleOf(a, roles.data, profiles)?.title, hosted, t)}
                onOpen={() => setOpen(a.alias)}
              />
            ))}
            {shownProfiles.map((profile) => (
              <ProfileSeat
                key={`profile-${profile.id}`}
                profile={profile}
                roleTitle={profileTitle(profile)}
                work={profileWork(profile, issueCards)}
                onOpen={() => setOpen(profile.alias)}
              />
            ))}
            {!searching && tile}
          </div>
          {folded.length > 0 && (
            <details className="ag-folded">
              <summary>
                {t("Stopped and mailboxes")} <span className="num">{folded.length}</span>
              </summary>
              <div className="rc-seats">
                {folded.map((a) => (
                  <SeatCard
                    key={a.alias}
                    agent={a}
                    roleLine={roleLine(a, roleOf(a, roles.data, profiles)?.title, hosted, t)}
                    onOpen={() => setOpen(a.alias)}
                  />
                ))}
              </div>
            </details>
          )}
          {agents.length + shownProfiles.length === 0 && (
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
                    onTeam={onTeamCount(role, profiles)}
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
